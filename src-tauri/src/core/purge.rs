//! 净化引擎 —— P1 的主流程编排。
//!
//! 一次完整净化 = 采集 → 判定 → （按结论）结束 / 裁剪 → 记账 → 落快照。
//!
//! 安全四重兜底（设计文档 4.5）：
//! 1. **硬黑名单**：判定期就 Forbidden，永不进入结束队列；
//! 2. **执行前二次校验**：真正调用 `kill` 之前再查一次黑名单与自身 PID；
//! 3. **底层安全闸**：`killer` 内部还会过一遍 [`safety::check_execute`]，
//!    连「调用方漏写校验」这种结构性错误也拦得住；
//! 4. **快照回滚**：所有结束动作都有记录，可一键拉回。
//!
//! ## 演练模式
//!
//! [`PurgeMode::DryRun`] 下，引擎会**完整走完判定与全部安全校验**，
//! 把「打算结束谁、打算裁谁」如实算出来，但**不调用任何系统写操作**，
//! 也不落快照。自测、压力测试、UI 预演一律走这个模式 ——
//! 「200 次净化零误杀」这条验收标准就是靠它验证的，验证过程本身
//! 不会对真实系统造成任何影响。

use super::blacklist;
use super::idle::{self, JudgeContext};
use super::killer::{self, KillMethod};
use super::perf::{human_bytes, SystemSampler};
use super::process::{self, RawProc, Sampler};
use super::safety;
use super::store::{new_snapshot_id, Snapshot, Whitelist};
use super::types::{
    CollectStats, KilledRecord, ProcessInfo, PurgeResult, SignatureStatus, SystemMetrics, Verdict,
};

/// 工作集裁剪门槛：低于此值的进程裁了也没意义。
///
/// 定在 12 MB 是有实测依据的：Windows 上服务宿主（`svchost` / `RuntimeBroker`
/// 等）单实例普遍在 15–40 MB，实例数上百。门槛若定 30 MB，
/// 绝大多数的它们会被一刀切掉，「释放内存」这项用户可感知收益直接归零
/// （实测 118 个服务宿主里只剩 2 个够格）。12 MB 能把主体纳入，
/// 同时仍然排掉那些裁了也没多少东西的小进程。
const TRIM_MIN_BYTES: u64 = 12 * 1024 * 1024;
/// 裁剪时允许的最大 CPU 占用（活跃进程裁剪会变慢）。
const TRIM_MAX_CPU: f32 = 1.0;

/// 可以安全裁剪工作集的「服务宿主」类进程。
///
/// 这些进程大多在硬黑名单里（**不能结束**），但**裁剪**它们是安全的：
/// 裁剪不终止进程，只把无人访问的物理页换出，进程下次访问时系统自动换回。
/// 这正是 Windows 自身在内存压力下的标准行为，也是「释放内存」这项
/// 用户可感知收益的主要来源。
///
/// 真正的红线是「结束」，不是「裁剪」—— 两者风险量级完全不同，
/// 早期版本用同一把尺子，结果把收益最大的对象全挡在了门外。
const TRIM_OK_SERVICE_HOSTS: &[&str] = &[
    "svchost",        // 服务宿主，通常几十个实例，收益最大
    "dllhost",        // COM+ 代理
    "wmiprvse",       // WMI 提供程序宿主
    "taskhostw",      // 任务宿主
    "spoolsv",        // 打印后台
    "runtimebroker",  // 系统 API 代理
    "fontdrvhost",    // 字体驱动宿主
    "conhost",        // 控制台宿主
    "openconsole",
    "searchprotocolhost",
    "searchfilterhost",
];

/// 该进程是否属于「可安全裁剪」的服务宿主类。
fn is_trim_ok_service_host(name: &str) -> bool {
    let n = blacklist::normalize(name);
    TRIM_OK_SERVICE_HOSTS.contains(&n.as_str())
}

/// 该进程是否「属于某个交互式程序」。
///
/// 两处判断共用这一个定义：
/// - **判定期**：属于交互式程序 → 不得判 `Safe`（`idle.rs` 里用同样的信号，
///   那边保留了逐条理由，便于给用户解释）；
/// - **裁剪期**：属于交互式程序 → 不得裁剪工作集。
///   裁剪虽不结束进程，但会把物理页强制换出；用户切回去时立刻硬缺页、
///   明显卡顿。对正在用的程序做这件事，与「快」的产品承诺直接冲突。
fn part_of_interactive_app(p: &RawProc, env: &Env) -> bool {
    // 持有任何顶层窗口（含托盘）
    if env.all_windows.contains(&p.pid) {
        return true;
    }
    // 祖先链上有窗口进程（浏览器渲染进程、IDE 下的 node 等）
    if env.window_ancestors.contains(&p.pid) {
        return true;
    }
    // 当前前台
    if p.pid == env.foreground_pid {
        return true;
    }
    // 父进程处于**交互式会话** → 是用户正在用的程序的子进程。
    //
    // 注意判据是「父进程所在会话」而不是「子进程所在会话」：
    // 每个用户的服务（如 WpnUserService）由 session 0 的 `services.exe`
    // 拉起，但宿主 svchost 跑在用户会话里。按子进程会话判，会把这类
    // 正常服务误判成用户程序，从而白白丢掉最大的一块裁剪收益。
    if p.ppid != 0 {
        if let Some(&parent_session) = env.session_of.get(&p.ppid) {
            if parent_session != 0 {
                return true;
            }
        }
    }
    false
}

/// 该进程是否满足裁剪的前置条件（与判定结论无关）。
fn trim_eligible(p: &RawProc, env: &Env) -> bool {
    p.cpu_percent < TRIM_MAX_CPU
        && p.working_set >= TRIM_MIN_BYTES
        && !part_of_interactive_app(p, env)
}

/// 净化的作用范围。
///
/// * `SafeOnly` —— 只处理「完全闲置」的目标（P1 的安全口径）。
/// * `SafeAndBackground` —— 在安全口径之上，额外纳入「有窗口但当前不在前台、
///   且几乎不占 CPU」的后台程序（游戏模式用）。这类进程有窗口 → 在 P1 的
///   判定里会被封顶为 Keep，所以必须由这里显式放宽。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PurgeScope {
    SafeOnly,
    SafeAndBackground,
}

/// 「游戏模式」下的后台裁剪资格。
///
/// 条件：**有可见窗口**、**不是当前前台**、CPU 占用很低、工作集够大。
///
/// 这条比 [`trim_eligible`] 宽松 —— 它允许动「用户看得见的窗口程序」。
/// 之所以敢放宽：裁剪**不结束进程**，只是把物理页换出，用户切回时系统
/// 会自动换回；而且只对「不在前台 + 几乎不耗 CPU」的程序动手，
/// 不影响正在玩的那一局。
///
/// 仍然排除的：当前前台进程、CPU 正在忙的程序、祖先链上有窗口的子进程
/// （浏览器渲染进程这类不能动）。
fn background_trim_eligible(p: &RawProc, env: &Env) -> bool {
    if p.pid == env.foreground_pid {
        return false;
    }
    if !env.visible_windows.contains(&p.pid) {
        return false;
    }
    if env.window_ancestors.contains(&p.pid) {
        return false;
    }
    if p.cpu_percent >= TRIM_MAX_CPU {
        return false;
    }
    p.working_set >= TRIM_MIN_BYTES
}

/// 净化执行模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PurgeMode {
    /// 真实执行：调用 `TerminateProcess` / `EmptyWorkingSet`，并落快照。
    Live,
    /// 演练：只算不做。判定、安全校验、收益估算全部照跑，但零系统改动。
    DryRun,
}

impl PurgeMode {
    pub fn is_dry(self) -> bool {
        matches!(self, PurgeMode::DryRun)
    }
}

/// 净化引擎。持有采样器与 PDH 句柄，跨刷新复用（否则 CPU 增量永远算不出来）。
pub struct PurgeEngine {
    sampler: Sampler,
    perf: SystemSampler,
    pub whitelist: Whitelist,
    /// 最近一次扫描结果（供 UI 增量刷新）
    pub last: Vec<ProcessInfo>,
    mode: PurgeMode,
    /// 是否把快照写盘。自测时关掉，免得污染用户真实数据。
    persist_snapshots: bool,
    /// pid → 连续「空闲」观测轮数。用于要求多轮确认。
    idle_streak: std::collections::HashMap<u32, u8>,
}

/// 一次采集里与判定相关的系统环境快照。
///
/// 之所以抽出来：窗口枚举、前台查询、祖先链回溯都是**每轮只该做一次**的
/// 全局计算，不该在逐个进程判定时重复调用。
struct Env {
    foreground_pid: u32,
    visible_windows: std::collections::HashSet<u32>,
    all_windows: std::collections::HashSet<u32>,
    window_ancestors: std::collections::HashSet<u32>,
    shell_pids: std::collections::HashSet<u32>,
    live_pids: std::collections::HashSet<u32>,
    /// pid → 会话 ID。用于判断「父进程是否处于服务会话」。
    session_of: std::collections::HashMap<u32, u32>,
}

/// 采集当前环境。
fn probe_env(raw: &[RawProc]) -> Env {
    let visible_windows = process::pids_with_visible_window();
    let all_windows = process::pids_with_any_window();
    let window_ancestors = process::pids_with_window_ancestor(raw, &all_windows);
    let live_pids: std::collections::HashSet<u32> = raw.iter().map(|p| p.pid).collect();
    let session_of: std::collections::HashMap<u32, u32> =
        raw.iter().map(|p| (p.pid, p.session_id)).collect();
    let shell_pids = raw
        .iter()
        .filter(|p| blacklist::normalize(&p.name) == "explorer")
        .map(|p| p.pid)
        .collect();

    Env {
        foreground_pid: process::foreground_pid(),
        visible_windows,
        all_windows,
        window_ancestors,
        shell_pids,
        live_pids,
        session_of,
    }
}

impl Default for PurgeEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl PurgeEngine {
    /// 生产引擎：真实执行。
    pub fn new() -> Self {
        Self::with_mode(PurgeMode::Live)
    }

    /// 演练引擎：只算不做。**所有自测一律用这个**。
    pub fn dry() -> Self {
        Self::with_mode(PurgeMode::DryRun)
    }

    pub fn with_mode(mode: PurgeMode) -> Self {
        Self {
            sampler: Sampler::new(),
            perf: SystemSampler::new(),
            whitelist: Whitelist::load(),
            last: Vec::new(),
            mode,
            persist_snapshots: !mode.is_dry(),
            idle_streak: std::collections::HashMap::new(),
        }
    }

    pub fn mode(&self) -> PurgeMode {
        self.mode
    }

    pub fn is_dry_run(&self) -> bool {
        self.mode.is_dry()
    }

    /// 关闭快照写盘（自测用，避免污染用户的 `snapshots.json`）。
    pub fn set_persist_snapshots(&mut self, on: bool) {
        self.persist_snapshots = on;
    }

    /// PDH 是否可用（P0 报告用）。
    pub fn perf_healthy(&self) -> bool {
        self.perf.is_healthy()
    }

    /// 推进「连续空闲」计数。
    ///
    /// 空闲的判定故意做得**保守且廉价**：CPU 低于 1% 且不持有任何窗口。
    /// 计数只增不减地累积连续轮次，一旦某轮不满足就归零。
    fn advance_streak(&mut self, raw: &[RawProc], env: &Env) {
        let mut next: std::collections::HashMap<u32, u8> =
            std::collections::HashMap::with_capacity(raw.len());
        for p in raw {
            let idle = p.cpu_percent < 1.0
                && !env.all_windows.contains(&p.pid)
                && !env.window_ancestors.contains(&p.pid);
            let prev = self.idle_streak.get(&p.pid).copied().unwrap_or(0);
            next.insert(p.pid, if idle { prev.saturating_add(1) } else { 0 });
        }
        // 只保留仍存活的 PID，防止长跑内存增长
        self.idle_streak = next;
    }

    pub fn cpu_count(&self) -> u32 {
        self.sampler.cpu_count()
    }

    /// 扫描一轮，返回判定后的进程列表 + 系统指标 + 采集耗时。
    pub fn scan(&mut self, with_paths: bool) -> (Vec<ProcessInfo>, SystemMetrics, CollectStats) {
        let (raw, stats) = self.sampler.collect(with_paths);
        let env = probe_env(&raw);
        self.advance_streak(&raw, &env);
        let ctx = self.build_ctx(&env);
        let list = judge_all(&raw, &ctx);

        let threads: u32 = raw.iter().map(|p| p.thread_count).sum();
        let metrics = self.perf.sample(list.len() as u32, threads);

        self.last = list.clone();
        (list, metrics, stats)
    }

    fn build_ctx(&self, env: &Env) -> JudgeContext {
        JudgeContext {
            foreground_pid: env.foreground_pid,
            window_pids: env.visible_windows.iter().copied().collect(),
            window_pids_all: env.all_windows.iter().copied().collect(),
            window_ancestor_pids: env.window_ancestors.iter().copied().collect(),
            shell_pids: env.shell_pids.iter().copied().collect(),
            live_pids: env.live_pids.iter().copied().collect(),
            idle_streak: self.idle_streak.clone(),
            whitelist: self.whitelist.names.clone(),
            self_pid: std::process::id(),
        }
    }

    /// 执行净化。
    ///
    /// * `targets` —— `None` = 自动模式（结束所有判定为 Safe 的进程）；
    ///   `Some(pids)` = 只处理用户勾选的这些（**仍然**会拦下 Forbidden）。
    /// * `grace_ms` —— 温和关闭等待时长。
    ///
    /// 演练模式（[`PurgeMode::DryRun`]）下只产出计划，不碰系统。
    pub fn purge(&mut self, targets: Option<Vec<u32>>, grace_ms: u64) -> PurgeResult {
        self.purge_scoped(targets, grace_ms, PurgeScope::SafeOnly, true, true)
    }

    /// 带作用域与裁剪开关的净化。
    ///
    /// * `scope` —— [`PurgeScope::SafeOnly`] 是 P1 的安全口径；`SafeAndBackground`
    ///   额外允许裁剪「有窗口但当前不在前台」的后台程序（**只裁剪，不结束**）。
    /// * `trim_enabled` —— 是否做工作集裁剪。
    /// * `kill_enabled` —— 是否结束进程。关掉它就是「只裁剪不结束」。
    ///
    /// 注意作用域**只放宽裁剪范围，不放宽结束范围**：结束的候选永远只来自
    /// 判定为 `Safe` 的进程，窗口类程序永远不会因为「切到游戏模式」而被结束。
    /// 红线是「结束」，不是「裁剪」。
    pub fn purge_scoped(
        &mut self,
        targets: Option<Vec<u32>>,
        grace_ms: u64,
        scope: PurgeScope,
        trim_enabled: bool,
        kill_enabled: bool,
    ) -> PurgeResult {
        let t0 = std::time::Instant::now();
        let dry = self.mode.is_dry();

        // 用最新数据重新判定，不信任 UI 传来的旧结论（防 TOCTOU）
        let (raw, _) = self.sampler.collect(true);
        let env = probe_env(&raw);
        self.advance_streak(&raw, &env);
        let ctx = self.build_ctx(&env);

        let snapshot_id = if dry { String::new() } else { new_snapshot_id() };
        let mut result = PurgeResult {
            snapshot_id: snapshot_id.clone(),
            timestamp: process::now_unix(),
            dry_run: dry,
            ..Default::default()
        };

        let selected: Vec<&RawProc> = match &targets {
            None => raw.iter().collect(),
            Some(ids) => {
                let set: std::collections::HashSet<u32> = ids.iter().copied().collect();
                raw.iter().filter(|p| set.contains(&p.pid)).collect()
            }
        };

        let mut to_kill: Vec<(u32, String, Option<String>, u64)> = Vec::new();
        let mut to_trim: Vec<(u32, String, Option<String>, u64)> = Vec::new();

        for p in selected {
            let j = idle::judge(p, &ctx);

            // ---- 兜底 1：硬黑名单 / 内核进程 / 自身 ----
            if j.verdict == Verdict::Forbidden {
                if targets.is_some() {
                    // 用户明确点了它，得告诉他为什么不动
                    result.blocked.push(KilledRecord {
                        pid: p.pid,
                        name: p.name.clone(),
                        exe_path: p.exe_path.clone(),
                        freed_bytes: 0,
                        method: "skipped".into(),
                        error: j.reasons.first().cloned(),
                    });
                }
                // 硬黑名单进程**绝不结束**，但服务宿主类可以安全裁剪：
                // svchost 这类进程内存占用可观、实例数上百，且裁剪不终止进程，
                // 是「释放内存」收益最大的来源。红线是「结束」，不是「裁剪」。
                //
                // 注意：这段必须在 `continue` **之前** —— 早期版本把它写在了
                // 下面的 match 里，结果因为这里先 continue 而成了死代码，
                // 裁剪收益永远是 0。
                if trim_enabled
                    && is_trim_ok_service_host(&p.name)
                    && trim_eligible(p, &env)
                {
                    to_trim.push((p.pid, p.name.clone(), p.exe_path.clone(), p.working_set));
                }
                continue;
            }

            match j.verdict {
                Verdict::Safe => {
                    if kill_enabled {
                        to_kill.push((
                            p.pid,
                            p.name.clone(),
                            p.exe_path.clone(),
                            p.working_set,
                        ));
                    } else if trim_enabled && trim_eligible(p, &env) {
                        // 「只裁剪不结束」模式：Safe 的目标降级为裁剪对象。
                        // 用户要的是「省内存」而不是「关程序」。
                        to_trim.push((
                            p.pid,
                            p.name.clone(),
                            p.exe_path.clone(),
                            p.working_set,
                        ));
                    }
                }
                Verdict::Keep => {
                    // 不结束，但如果是「不属于任何交互式程序 + 几乎不占 CPU +
                    // 内存占用可观」，顺手裁一下工作集 —— 这是「释放内存」的
                    // 主要来源，也是对用户完全无感的那部分收益
                    if trim_enabled && trim_eligible(p, &env) {
                        to_trim.push((
                            p.pid,
                            p.name.clone(),
                            p.exe_path.clone(),
                            p.working_set,
                        ));
                    }
                    // 「游戏模式」的口径：在安全口径之上，允许裁剪「有可见窗口
                    // 但当前不在前台、且几乎不占 CPU」的后台程序。
                    //
                    // 这类进程在 P1 的判定里会被「有可见窗口」封顶成 Keep，
                    // 所以要由这里显式放宽 —— 但**只放宽裁剪**：
                    // 它们有窗口，可能随时被用户切回来，结束掉是误杀。
                    if trim_enabled
                        && scope == PurgeScope::SafeAndBackground
                        && j.verdict == Verdict::Keep
                        && background_trim_eligible(p, &env)
                    {
                        to_trim.push((
                            p.pid,
                            p.name.clone(),
                            p.exe_path.clone(),
                            p.working_set,
                        ));
                    }
                }
                Verdict::Forbidden => unreachable!("Forbidden 已在上面 continue 处理"),
            }
        }

        // ---- 结束进程 ----
        for (pid, name, path, ws) in to_kill {
            // 兜底 2：执行前二次校验（与底层安全闸同源，这里提前拦是为了
            // 在结果里给出「我帮你挡住了」的明确解释）
            if let Err(why) = safety::check_target(pid, Some(&name)) {
                result.blocked.push(KilledRecord {
                    pid,
                    name,
                    exe_path: path,
                    freed_bytes: 0,
                    method: "skipped".into(),
                    error: Some(why),
                });
                continue;
            }
            if pid == std::process::id() {
                result.blocked.push(KilledRecord {
                    pid,
                    name,
                    exe_path: path,
                    freed_bytes: 0,
                    method: "skipped".into(),
                    error: Some("不能结束自己".into()),
                });
                continue;
            }

            // ---- 演练：只记账，不执行 ----
            if dry {
                result.planned_bytes += ws;
                result.killed.push(KilledRecord {
                    pid,
                    name,
                    exe_path: path,
                    // 演练没有真的释放内存，所以这里填的是**预估**量，
                    // 用于让报告看得出「这一条值多少」。真实收益统一走
                    // `PurgeResult::planned_bytes`，`freed_bytes` 恒为 0。
                    freed_bytes: ws,
                    method: "dry-run".into(),
                    error: None,
                });
                continue;
            }

            let (method, err) = killer::kill_two_stage(pid, &name, grace_ms);
            let freed = if err.is_none() && method != KillMethod::AlreadyGone {
                ws
            } else {
                0
            };
            if err.is_none() && method != KillMethod::AlreadyGone {
                result.freed_bytes += freed;
            }
            result.killed.push(KilledRecord {
                pid,
                name,
                exe_path: path,
                freed_bytes: freed,
                method: method.as_str().into(),
                error: err,
            });
        }

        // ---- 裁剪工作集 ----
        for (pid, name, path, ws) in to_trim {
            // 组队阶段已经过 trim_eligible / is_trim_ok_service_host 两道筛，
            // 这里只再排掉内核 PID 与自身
            if blacklist::is_protected_pid(pid) || pid == std::process::id() {
                continue;
            }
            if dry {
                result.planned_bytes += ws;
                result.trimmed.push(KilledRecord {
                    pid,
                    name,
                    exe_path: path,
                    freed_bytes: ws, // 预估值，见上方说明
                    method: "dry-run".into(),
                    error: None,
                });
                continue;
            }
            match killer::trim_working_set(pid, &name, ws) {
                Ok(freed) => {
                    result.freed_bytes += freed;
                    result.trimmed.push(KilledRecord {
                        pid,
                        name,
                        exe_path: path,
                        freed_bytes: freed,
                        method: "trim".into(),
                        error: None,
                    });
                }
                Err(e) => result.trimmed.push(KilledRecord {
                    pid,
                    name,
                    exe_path: path,
                    freed_bytes: 0,
                    method: "trim".into(),
                    error: Some(e),
                }),
            }
        }

        result.elapsed_ms = t0.elapsed().as_millis() as u64;

        // ---- 落快照（演练模式与自测一律跳过）----
        if !dry && self.persist_snapshots {
            let snap = Snapshot {
                id: snapshot_id,
                timestamp: result.timestamp,
                killed: result.killed.clone(),
                trimmed: result.trimmed.clone(),
                freed_bytes: result.freed_bytes,
            };
            let _ = Snapshot::append(snap);
        }

        result
    }
}

/// 对一批原始进程做判定，产出 UI 直接可用的 `ProcessInfo`。
pub fn judge_all(raw: &[RawProc], ctx: &JudgeContext) -> Vec<ProcessInfo> {
    let mut out: Vec<ProcessInfo> = Vec::with_capacity(raw.len());

    for p in raw {
        let j = idle::judge(p, ctx);
        let sig = match &p.exe_path {
            Some(path) if !path.is_empty() => super::signature::verify(path),
            _ => SignatureStatus::Unknown,
        };

        out.push(ProcessInfo {
            pid: p.pid,
            ppid: p.ppid,
            name: if p.name.is_empty() {
                format!("PID {}", p.pid)
            } else {
                p.name.clone()
            },
            exe_path: p.exe_path.clone(),
            uptime_secs: p.uptime_secs(),
            cpu_percent: p.cpu_percent,
            working_set: p.working_set,
            private_bytes: p.private_bytes,
            thread_count: p.thread_count,
            handle_count: p.handle_count,
            io_read_bytes: p.io_read,
            io_write_bytes: p.io_write,
            session_id: p.session_id,
            has_window: ctx.window_pids.contains(&p.pid),
            has_tray_window: ctx.window_pids_all.contains(&p.pid),
            under_window_app: ctx.window_ancestor_pids.contains(&p.pid),
            is_foreground: p.pid == ctx.foreground_pid,
            signature: sig,
            group: process::group_of(&p.name, p.exe_path.as_deref()),
            verdict: j.verdict,
            score: j.score,
            reasons: j.reasons,
        });
    }

    // 排序：可结束的排最前（用户最关心），同档按内存降序（省得最多）
    out.sort_by(|a, b| {
        let rank = |v: &Verdict| match v {
            Verdict::Safe => 0,
            Verdict::Keep => 1,
            Verdict::Forbidden => 2,
        };
        rank(&a.verdict)
            .cmp(&rank(&b.verdict))
            .then_with(|| b.working_set.cmp(&a.working_set))
    });

    out
}

/// 生成净化结果的一句话结论（产品文案，直接给用户看）。
///
/// 演练结果的文案单独走一条路，避免把「预计」说成「已释放」。
pub fn summarize(r: &PurgeResult) -> String {
    if r.dry_run {
        // 演练里所有记录都是 dry-run，所以这里按「无错误」计数，
        // 不能复用下面的 real() 过滤 —— 那会把计划数全滤成 0。
        let planned = r.killed.iter().filter(|k| k.error.is_none()).count();
        let trimmed = r.trimmed.iter().filter(|k| k.error.is_none()).count();
        let blocked = r.blocked.len();
        let mut s = format!(
            "【演练】预计可释放 {} 内存，计划结束 {planned} 个闲置进程",
            human_bytes(r.planned_bytes)
        );
        if trimmed > 0 {
            s.push_str(&format!("，瘦身 {trimmed} 个后台进程"));
        }
        if blocked > 0 {
            s.push_str(&format!("；安全挡下 {blocked} 个系统进程"));
        }
        s.push_str(&format!("。未对系统做任何改动，耗时 {} ms。", r.elapsed_ms));
        return s;
    }

    // 真实模式：dry-run 记录不算数（正常情况下也不会出现在这里）
    let real = |k: &&KilledRecord| k.error.is_none() && k.method != "dry-run";
    let killed = r.killed.iter().filter(real).count();
    let trimmed = r.trimmed.iter().filter(real).count();
    let blocked = r.blocked.len();

    let mut s = format!(
        "释放 {} 内存，结束 {} 个闲置进程",
        human_bytes(r.freed_bytes),
        killed
    );
    if trimmed > 0 {
        s.push_str(&format!("，顺带瘦身 {trimmed} 个后台进程"));
    }
    if blocked > 0 {
        s.push_str(&format!("；出于安全挡下 {blocked} 个系统进程"));
    }
    s.push_str(&format!("。耗时 {} ms。", r.elapsed_ms));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::blacklist;

    /// 压力测试轮数。默认 25（保证 `cargo test` 秒级返回）；
    /// 完整 200 轮的验收跑 `old666-poc --stress 200`。
    fn stress_rounds() -> usize {
        std::env::var("OLD666_STRESS_ROUNDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(25)
    }

    // -----------------------------------------------------------------
    // 安全约束：本文件所有测试一律用 `PurgeEngine::dry()`。
    // 演练模式不调用任何系统写操作、不落快照，因此测试本身零副作用。
    // -----------------------------------------------------------------

    #[test]
    fn engine_scan_works() {
        let mut e = PurgeEngine::dry();
        let (list, m, stats) = e.scan(true);
        assert!(!list.is_empty());
        assert!(m.mem_total > 0);
        assert_eq!(stats.process_count as usize, list.len());
        // 系统关键进程必须全被判 Forbidden
        for p in list.iter().filter(|p| {
            matches!(
                blacklist::normalize(&p.name).as_str(),
                "lsass" | "csrss" | "wininit" | "services"
            )
        }) {
            assert_eq!(p.verdict, Verdict::Forbidden, "{} 应为禁止结束", p.name);
        }
    }

    /// 演练模式必须做到「零系统改动」：不真结束、不真释放、不落快照。
    #[test]
    fn dry_run_never_touches_system() {
        let mut e = PurgeEngine::dry();
        assert!(e.is_dry_run());
        e.scan(true);
        let r = e.purge(None, 200);

        assert!(r.dry_run, "结果应标记为演练");
        assert!(r.snapshot_id.is_empty(), "演练不应生成快照 ID");
        assert_eq!(r.freed_bytes, 0, "演练不得虚报已释放内存");
        for k in &r.killed {
            assert_eq!(k.method, "dry-run", "演练中的结束记录必须是 dry-run");
        }
        for k in &r.trimmed {
            assert_eq!(k.method, "dry-run");
        }
        // 每条记录里的 freed_bytes 是**预估值**，只用于展示「这条值多少」；
        // 真实收益统一走 planned_bytes，绝不计入 freed_bytes。
        let sum_planned: u64 = r.killed.iter().map(|k| k.freed_bytes).sum::<u64>()
            + r.trimmed.iter().map(|k| k.freed_bytes).sum::<u64>();
        assert_eq!(
            r.planned_bytes, sum_planned,
            "planned_bytes 应等于各条预估量之和"
        );
    }

    /// 自动净化（演练）跑一轮：没有任何系统关键进程进入结束队列。
    #[test]
    fn auto_purge_never_queues_system_processes() {
        let mut e = PurgeEngine::dry();
        e.scan(true);
        let r = e.purge(None, 200);

        for k in &r.killed {
            assert!(
                safety::check_target(k.pid, Some(&k.name)).is_ok(),
                "自动净化把系统进程排进了结束队列：{} (PID {})",
                k.name,
                k.pid
            );
        }
    }

    /// 用户明确点名要结束系统进程 —— 必须被挡下，且给出原因。
    #[test]
    fn explicit_target_still_blocks_forbidden() {
        let mut e = PurgeEngine::dry();
        e.scan(true);
        let sys: Vec<u32> = e
            .last
            .iter()
            .filter(|p| p.verdict == Verdict::Forbidden)
            .map(|p| p.pid)
            .take(5)
            .collect();
        if sys.is_empty() {
            return;
        }
        let r = e.purge(Some(sys.clone()), 100);

        assert_eq!(r.killed.len(), 0, "系统进程不应有任何一条进入结束队列");
        assert!(!r.blocked.is_empty(), "应给出「被挡下」的明确记录");
        assert!(
            r.blocked.iter().all(|b| b.error.is_some()),
            "每条拦截都应带原因"
        );
    }

    /// **回归守卫**：交互式程序绝不能被排进结束队列。
    ///
    /// 这条是照着一次真实的误杀事故加的 —— 早期版本只看「可见窗口」，
    /// 结果把 `chrome.exe`、`iFlyInput.exe`（输入法）、
    /// `WorkBuddy.exe`（助手自身）、以及它们拉起的 `node.exe`/`bash.exe`
    /// 全部判成了「可安全结束」。一个真实的桌面系统上，
    /// 这类进程通常占绝大多数。
    #[test]
    fn interactive_processes_are_never_queued() {
        let mut e = PurgeEngine::dry();
        e.scan(true);
        e.scan(true); // 第二轮，让「多轮确认」不至于把一切都压成 Keep
        let r = e.purge(None, 100);

        let mut violations: Vec<String> = Vec::new();
        for k in &r.killed {
            if let Some(p) = e.last.iter().find(|p| p.pid == k.pid) {
                if p.has_window || p.has_tray_window || p.under_window_app {
                    violations.push(format!(
                        "{} (PID {}, 可见窗口={} 托盘窗口={} 属于在用程序={})",
                        p.name, p.pid, p.has_window, p.has_tray_window, p.under_window_app
                    ));
                }
            }
        }
        assert!(
            violations.is_empty(),
            "交互式程序被排进了结束队列：{:#?}",
            violations
        );
    }

    /// **回归守卫**：服务宿主必须能进入裁剪队列。
    ///
    /// 对应一个真实 bug：`Verdict::Forbidden` 在 match 之前就 `continue` 了，
    /// 于是写在 match 里的服务宿主裁剪逻辑成了死代码 —— 编译能过、
    /// 测试全绿，但「释放内存」这项核心收益永远是 0。
    #[test]
    fn service_hosts_are_trim_candidates() {
        let mut e = PurgeEngine::dry();
        e.scan(true);
        e.scan(true);
        let r = e.purge(None, 50);

        let candidates = e
            .last
            .iter()
            .filter(|p| {
                is_trim_ok_service_host(&p.name)
                    && p.working_set >= TRIM_MIN_BYTES
                    && p.cpu_percent < TRIM_MAX_CPU
                    && !p.has_window
                    && !p.has_tray_window
                    && !p.under_window_app
            })
            .count();

        if candidates == 0 {
            return; // 本机没有够格的服务宿主，这条用例不适用
        }
        assert!(
            !r.trimmed.is_empty(),
            "本机有 {candidates} 个够格的服务宿主，裁剪队列却是空的（组队逻辑可能被 continue 跳过）"
        );
    }

    /// 硬黑名单进程（输入法 / 杀软 / 外壳组件）一条都不许进结束队列。
    #[test]
    fn denied_names_are_never_queued() {
        let mut e = PurgeEngine::dry();
        e.scan(true);
        e.scan(true);
        let r = e.purge(None, 100);
        for k in &r.killed {
            assert!(
                !blacklist::is_denied_name(&k.name),
                "黑名单进程被排进了结束队列：{} (PID {})",
                k.name,
                k.pid
            );
        }
    }

    /// 白名单进程永不进入结束队列。
    #[test]
    fn whitelisted_never_queued() {
        let mut e = PurgeEngine::dry();
        e.scan(true);
        // 挑一个判定为 Safe 的，加进白名单，再跑一遍
        let Some(victim) = e.last.iter().find(|p| p.verdict == Verdict::Safe).cloned() else {
            return;
        };
        e.whitelist.names.push(blacklist::normalize(&victim.name));
        e.scan(true);
        let r = e.purge(None, 100);

        assert!(
            !r.killed.iter().any(|k| k.pid == victim.pid),
            "白名单进程 {} 不应被结束",
            victim.name
        );
    }

    /// **P1 核心验收**：连续 N 轮净化，零误杀。
    ///
    /// 完整 200 轮见 `old666-poc --stress 200`（含真实结束链路）。
    #[test]
    fn zero_misfire_stress() {
        let rounds = stress_rounds();
        let mut e = PurgeEngine::dry();
        let mut misfires: Vec<String> = Vec::new();
        let mut queued_total = 0usize;

        for i in 0..rounds {
            e.scan(true);
            let r = e.purge(None, 50);
            queued_total += r.killed.len();
            for k in &r.killed {
                if safety::check_target(k.pid, Some(&k.name)).is_err()
                    || blacklist::is_denied_name(&k.name)
                {
                    misfires.push(format!("第 {} 轮：{} (PID {})", i + 1, k.name, k.pid));
                }
            }
        }

        assert!(
            misfires.is_empty(),
            "{rounds} 轮净化出现 {} 次误杀：{:#?}",
            misfires.len(),
            misfires
        );
        println!("零误杀压力测试：{rounds} 轮，累计排队 {queued_total} 个进程，误杀 0");
    }

    #[test]
    fn summarize_reads_naturally() {
        let r = PurgeResult {
            freed_bytes: 512 * 1024 * 1024,
            elapsed_ms: 1200,
            ..Default::default()
        };
        let s = summarize(&r);
        assert!(s.contains("512.0 MB"));
        assert!(s.contains("结束 0 个闲置进程"));
    }

    /// 演练文案不能把「预计」说成「已释放」。
    #[test]
    fn dry_run_summary_is_honest() {
        let r = PurgeResult {
            planned_bytes: 512 * 1024 * 1024,
            elapsed_ms: 900,
            dry_run: true,
            killed: vec![KilledRecord {
                pid: 1,
                name: "x.exe".into(),
                exe_path: None,
                freed_bytes: 0,
                method: "dry-run".into(),
                error: None,
            }],
            ..Default::default()
        };
        let s = summarize(&r);
        assert!(s.contains("演练"), "实际：{s}");
        assert!(s.contains("预计"), "实际：{s}");
        assert!(s.contains("未对系统做任何改动"), "实际：{s}");
        // 计划数必须如实反映（早期版本在演练分支里误用真实模式过滤器，
        // 把 1 条计划记录过滤成了 0，这里钉住这个回归）
        assert!(s.contains("计划结束 1 个闲置进程"), "实际：{s}");
        // 不能出现「已释放」这种既成事实的措辞
        assert!(!s.contains("释放 0"), "实际：{s}");
    }
}
