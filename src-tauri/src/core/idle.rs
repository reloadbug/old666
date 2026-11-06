//! 闲置判定引擎 —— P1 的核心智力。
//!
//! 输入：一个进程的原始数据 + 历史采样 + 用户白名单。
//! 输出：六维打分 + 三档结论 + **人话解释**。
//!
//! 判定哲学（贯穿全产品）：
//! **宁可少杀，不可错杀。** 任何一条硬否决命中，直接 Forbidden，不参与打分。

use std::collections::HashMap;

use super::blacklist;
use super::process::RawProc;
use super::signature;
use super::types::{IdleScore, SignatureStatus, Verdict};

/// 判定所需的额外上下文（由采集层提供，避免引擎自己去查系统）。
#[derive(Default, Clone)]
pub struct JudgeContext {
    /// 当前前台进程 PID
    pub foreground_pid: u32,
    /// 持有**可见**顶层窗口的进程集合
    pub window_pids: Vec<u32>,
    /// 持有**任意**顶层窗口（含隐藏）的进程集合。
    /// 托盘应用 / 输入法 / 常驻助手都会落在这里。
    pub window_pids_all: Vec<u32>,
    /// 祖先链上存在窗口进程的 PID 集合（chrome 渲染进程、IDE 下的 node 等）
    pub window_ancestor_pids: Vec<u32>,
    /// 外壳进程（`explorer.exe`）的 PID 集合 —— 它们的子进程是用户亲手启动的
    pub shell_pids: Vec<u32>,
    /// 当前**仍存活**的 PID 集合。
    /// 父进程还活着 → 说明它是某个正在运行的程序的一部分，不是被落下的残留。
    pub live_pids: Vec<u32>,
    /// pid → 连续「空闲」观测轮数。用于要求多轮确认，避免一次采样就下结论。
    pub idle_streak: HashMap<u32, u8>,
    /// 用户白名单（小写进程名）
    pub whitelist: Vec<String>,
    /// 本进程自身 PID（永不结束自己）
    pub self_pid: u32,
}

/// 判定结果。
pub struct Judgement {
    pub verdict: Verdict,
    pub score: IdleScore,
    pub reasons: Vec<String>,
}

/// 判定阈值。
pub const SAFE_THRESHOLD: u32 = 62;
pub const KEEP_THRESHOLD: u32 = 38;

/// 判为「可安全结束」前，要求进程已经**连续**空闲的观测轮数。
///
/// 「不常用」本质上是个时间概念，不是一帧快照能回答的问题。
/// 要求至少两轮观测都为空闲，才能过滤掉「刚好这一瞬间没占 CPU」的假信号
/// （编译、索引、后台任务都会周期性地掉到 0%）。
pub const MIN_IDLE_STREAK: u8 = 1;

/// 判为「可安全结束」前，要求进程至少已存活这么久（秒）。
///
/// 5 分钟以内的进程一律不动。理由有两个：
/// 1. 「占用大量系统资源」是个累积概念，刚起来的东西还谈不上占用；
/// 2. 短命进程（命令行工具、安装器子进程、构建任务）会大量误入候选，
///    它们本来马上就会自己退出，动它们没有任何收益，只有风险。
pub const MIN_UPTIME_FOR_SAFE: u64 = 300;

/// CPU 占用达到这个百分比（已按核心数归一）就不算「闲置」。
///
/// 这是**硬封顶**而不是打分项：早期版本只把它算进加权分，
/// 结果一个烧 40% CPU 的进程靠其他维度的高分照样能凑到 69 分被判可结束。
/// 正在吃 CPU 的进程，无论别的维度多漂亮，都不许动。
pub const MAX_IDLE_CPU_PERCENT: f32 = 2.0;

/// 对一个进程做完整判定。
pub fn judge(p: &RawProc, ctx: &JudgeContext) -> Judgement {
    let name = blacklist::normalize(&p.name);
    let mut reasons: Vec<String> = Vec::new();

    // =====================================================================
    // 第一层：硬否决（不参与打分，直接 Forbidden）
    // =====================================================================
    if blacklist::is_protected_pid(p.pid) {
        return forbidden("内核伪进程（PID ≤ 4），系统地基");
    }
    if p.pid == ctx.self_pid {
        return forbidden("这是老六清净自己");
    }
    if p.pid == 0 || name.is_empty() {
        return forbidden("无法识别的进程，按安全原则跳过");
    }
    if blacklist::is_denied_name(&name) {
        return forbidden("系统关键进程，结束会导致系统异常");
    }

    // 白名单是「用户说了算」的最高优先级，但**不能覆盖上面的硬黑名单**
    if ctx.whitelist.iter().any(|w| w == &name) {
        return Judgement {
            verdict: Verdict::Keep,
            score: IdleScore::default(),
            reasons: vec!["你已加入白名单，永久保留".into()],
        };
    }

    let exe = p.exe_path();
    if let Some(path) = exe.as_deref() {
        if blacklist::is_system_path(path) {
            // 系统目录下的进程：只有当它同时是「非微软签名」时才值得警惕，
            // 否则一律保守处理
            return forbidden("位于系统保护目录，按安全原则跳过");
        }
    }

    // =====================================================================
    // 第二层：六维打分
    // =====================================================================

    // --- 维度 1：活跃度（CPU 越低越闲）---
    let activity = score_activity(p.cpu_percent);

    // --- 维度 2：前台关联 ---
    let is_fg = p.pid == ctx.foreground_pid;
    let has_win = ctx.window_pids.contains(&p.pid);
    let has_any_win = ctx.window_pids_all.contains(&p.pid);
    let under_window_app = ctx.window_ancestor_pids.contains(&p.pid);
    let under_shell = p.ppid != 0 && ctx.shell_pids.contains(&p.ppid);
    // 父进程仍在运行 → 它是某个活着的程序的一部分（chrome 渲染进程、
    // WebView2 子进程、IDE 下的 node…）。真正的「残留」是父进程已退出、
    // 自己被落下的孤儿，那种情况下 ppid 指向一个已经不存在的 PID。
    let parent_alive = p.ppid != 0 && ctx.live_pids.contains(&p.ppid);

    let foreground = if is_fg {
        0
    } else if has_win {
        18
    } else if has_any_win || under_window_app {
        // 没有可见窗口，但自己或祖先持有窗口 —— 属于某个交互式应用，
        // 只是当前没显示出来（最小化到托盘 / 后台标签页 / IDE 的子进程）
        35
    } else if parent_alive {
        // 父进程还活着，说明有程序正在管着它
        60
    } else {
        100
    };

    // --- 维度 3：使用频率（启动后是否长期无动作）---
    let frequency = score_frequency(p);

    // --- 维度 4：签名来源 ---
    let sig = signature::verify_optional(exe.as_deref());
    let signature_score = match &sig {
        SignatureStatus::Valid(_) => {
            if signature::is_trusted_vendor(&sig) {
                25
            } else {
                55
            }
        }
        SignatureStatus::Unsigned => 90,
        SignatureStatus::Invalid => 100,
        SignatureStatus::Unknown => 50,
    };

    // --- 维度 5：系统关键性（父进程、路径、线程数等综合）---
    let criticality = score_criticality(p, exe.as_deref());

    // --- 维度 6：白名单（已在上层处理，此处固定 0）---
    let whitelist = 0u8;

    let score = IdleScore {
        activity,
        foreground,
        frequency,
        signature: signature_score,
        criticality,
        whitelist,
    };
    let total = score.total();

    // 人话解释：把最有说服力的 2–3 条挑出来给用户看
    if is_fg {
        reasons.push("当前正在前台使用".into());
    } else if has_win {
        reasons.push("有可见窗口，可能正在用".into());
    } else if has_any_win {
        reasons.push("持有窗口（可能在托盘/后台），仍属在用程序".into());
    } else if under_window_app {
        reasons.push("属于某个正在使用的程序的子进程".into());
    } else if under_shell {
        reasons.push("由资源管理器启动，是你自己打开的程序".into());
    } else if parent_alive {
        reasons.push("父进程仍在运行，属于某个活动程序".into());
    } else {
        reasons.push("无窗口、无前台关联、父进程已退出".into());
    }
    if p.cpu_percent < 0.1 {
        reasons.push(format!("CPU 占用 {:.1}%，几乎无活动", p.cpu_percent));
    } else if p.cpu_percent > 5.0 {
        reasons.push(format!("CPU 占用 {:.1}%，仍在工作", p.cpu_percent));
    }
    match &sig {
        SignatureStatus::Valid(s) => reasons.push(format!("已签名（{s}）")),
        SignatureStatus::Unsigned => reasons.push("无数字签名".into()),
        SignatureStatus::Invalid => reasons.push("签名无效，需警惕".into()),
        SignatureStatus::Unknown => {}
    }
    if p.uptime_secs() > 3600 && p.io_read == 0 && p.io_write == 0 {
        reasons.push("运行超 1 小时，零磁盘读写".into());
    }

    let verdict = if total >= SAFE_THRESHOLD {
        Verdict::Safe
    } else if total >= KEEP_THRESHOLD {
        Verdict::Keep
    } else {
        Verdict::Forbidden
    };

    // =====================================================================
    // 第三层：Safe 的「封顶」条件
    //
    // 分数再高，只要命中下面任何一条，最高只能到 Keep。
    // 这是「宁可少杀，不可错杀」在代码里的落点。
    // =====================================================================
    let mut verdict = verdict;

    let interactive =
        is_fg || has_win || has_any_win || under_window_app || under_shell || parent_alive;
    if verdict == Verdict::Safe && interactive {
        verdict = Verdict::Keep;
    }

    // 多轮确认：只有上一轮也观测到空闲，才允许判 Safe
    if verdict == Verdict::Safe {
        let streak = ctx.idle_streak.get(&p.pid).copied().unwrap_or(0);
        if streak < MIN_IDLE_STREAK {
            verdict = Verdict::Keep;
            reasons.push("首次观测到空闲，需再确认一轮".into());
        }
    }

    // 存活时间门槛：太年轻的进程不是「占用资源的老赖」
    if verdict == Verdict::Safe && p.uptime_secs() < MIN_UPTIME_FOR_SAFE {
        verdict = Verdict::Keep;
        reasons.push(format!("刚启动 {} 秒，暂不处理", p.uptime_secs()));
    }

    // CPU 门槛：正在干活的进程不是「闲置」
    if verdict == Verdict::Safe && p.cpu_percent >= MAX_IDLE_CPU_PERCENT {
        verdict = Verdict::Keep;
        reasons.push(format!("CPU 占用 {:.1}%，仍在工作", p.cpu_percent));
    }

    Judgement {
        verdict,
        score,
        reasons,
    }
}

fn forbidden(reason: &str) -> Judgement {
    Judgement {
        verdict: Verdict::Forbidden,
        score: IdleScore::default(),
        reasons: vec![reason.to_string()],
    }
}

/// CPU 占用 → 活跃度分。0% → 100 分，25%+ → 0 分。
fn score_activity(cpu: f32) -> u8 {
    if cpu <= 0.05 {
        return 100;
    }
    if cpu >= 25.0 {
        return 0;
    }
    let v = 100.0 - (cpu / 25.0) * 100.0;
    v.clamp(0.0, 100.0) as u8
}

/// 使用频率：启动越久、IO 越少 → 越闲。
fn score_frequency(p: &RawProc) -> u8 {
    let up = p.uptime_secs();
    let io = p.io_read.saturating_add(p.io_write);

    // 刚启动的进程（<60s）可能是开机自启，也可能正在初始化，给中性分
    if up < 60 {
        return 45;
    }
    // 运行很久但完全没 IO → 典型僵尸后台
    if up > 1800 && io == 0 {
        return 100;
    }
    if up > 600 && io < 64 * 1024 {
        return 80;
    }
    if up > 300 {
        return 62;
    }
    50
}

/// 系统关键性：父进程为系统进程、线程数多、内存大 → 越关键。
fn score_criticality(p: &RawProc, exe: Option<&str>) -> u8 {
    let mut s = 85u8;

    // 父进程是系统关键进程（svchost/services/wininit 等拉起的）→ 显著降分
    if p.ppid != 0 && p.ppid <= 4 {
        s = s.saturating_sub(25);
    }
    // 线程数多说明是正经的多线程服务，不是闲置残留
    if p.thread_count >= 32 {
        s = s.saturating_sub(20);
    } else if p.thread_count >= 16 {
        s = s.saturating_sub(10);
    }
    // 内存占用大 → 更可能是活跃组件
    if p.working_set >= 512 * 1024 * 1024 {
        s = s.saturating_sub(25);
    } else if p.working_set >= 128 * 1024 * 1024 {
        s = s.saturating_sub(12);
    }
    // 位于 Program Files 的通常是正规安装的软件
    if let Some(path) = exe {
        let lp = path.to_ascii_lowercase();
        if lp.contains("\\program files") {
            s = s.saturating_sub(10);
        }
    }
    // 会话 0 是服务会话，普通用户进程不该在这里
    if p.session_id == 0 {
        s = s.saturating_sub(8);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FILETIME 纪元差（1601 → 1970），单位 100ns。
    const EPOCH_DIFF: u64 = 116_444_736_000_000_000;

    /// 构造一个「`age_secs` 秒前启动」的 FILETIME。
    fn created_ago(age_secs: u64) -> u64 {
        let unix = super::super::process::now_unix().saturating_sub(age_secs);
        EPOCH_DIFF + unix * 10_000_000
    }

    fn proc(name: &str, pid: u32) -> RawProc {
        RawProc {
            pid,
            ppid: 1000,
            name: name.into(),
            working_set: 10 * 1024 * 1024,
            private_bytes: 8 * 1024 * 1024,
            thread_count: 4,
            handle_count: 40,
            // 默认按「1 小时前启动」—— 多数用例关心的是长期驻留的进程
            create_time: created_ago(3600),
            kernel_time: 0,
            user_time: 0,
            io_read: 0,
            io_write: 0,
            session_id: 1,
            ..Default::default()
        }
    }

    /// 一个「已经连续空闲过一轮」的上下文 —— 多轮确认的前提。
    fn ctx_with_streak(pid: u32) -> JudgeContext {
        let mut c = JudgeContext::default();
        c.idle_streak.insert(pid, MIN_IDLE_STREAK);
        c
    }

    #[test]
    fn system_process_is_forbidden() {
        let j = judge(&proc("lsass.exe", 700), &JudgeContext::default());
        assert_eq!(j.verdict, Verdict::Forbidden);
    }

    #[test]
    fn pid_below_4_is_forbidden() {
        let j = judge(&proc("anything.exe", 4), &JudgeContext::default());
        assert_eq!(j.verdict, Verdict::Forbidden);
    }

    #[test]
    fn whitelisted_is_keep() {
        let mut ctx = JudgeContext::default();
        ctx.whitelist = vec!["mytool".into()];
        let j = judge(&proc("mytool.exe", 5000), &ctx);
        assert_eq!(j.verdict, Verdict::Keep);
    }

    #[test]
    fn foreground_never_safe() {
        let mut ctx = JudgeContext::default();
        ctx.foreground_pid = 5000;
        let p = RawProc {
            cpu_percent: 0.0,
            ..proc("someapp.exe", 5000)
        };
        let j = judge(&p, &ctx);
        assert_ne!(j.verdict, Verdict::Safe, "前台进程绝不能判可结束");
    }

    #[test]
    fn idle_background_can_be_safe() {
        let p = RawProc {
            cpu_percent: 0.0,
            working_set: 8 * 1024 * 1024,
            thread_count: 2,
            io_read: 0,
            io_write: 0,
            ..proc("updater-tray.exe", 8000)
        };
        let j = judge(&p, &ctx_with_streak(8000));
        assert_eq!(
            j.verdict,
            Verdict::Safe,
            "连续两轮确认空闲的无窗口后台进程应判可结束，实际 {:?}",
            j.reasons
        );
    }

    /// 高 CPU 必须硬封顶：即便其他维度全是满分（无窗口、父进程已退出、
    /// 连续空闲、老进程），正在吃 CPU 的也不许判可结束。
    #[test]
    fn high_cpu_caps_even_with_perfect_other_signals() {
        let p = RawProc {
            ppid: 99999, // 父进程已退出
            cpu_percent: 40.0,
            working_set: 12 * 1024 * 1024,
            thread_count: 2,
            io_read: 0,
            io_write: 0,
            ..proc("render.exe", 8001)
        };
        let j = judge(&p, &ctx_with_streak(8001));
        assert_eq!(j.verdict, Verdict::Keep, "高 CPU 进程不得判可结束，实际 {:?}", j.reasons);
        assert!(j.reasons.iter().any(|r| r.contains("仍在工作")), "{:?}", j.reasons);
    }

    // =====================================================================
    // 以下是「宁可少杀」的封顶规则 —— 每一条都对应一次真实的误杀风险
    // =====================================================================

    /// 单轮观测到空闲不够，必须多轮确认。
    #[test]
    fn first_sight_idle_is_keep_not_safe() {
        let p = RawProc {
            cpu_percent: 0.0,
            ..proc("updater-tray.exe", 8100)
        };
        let j = judge(&p, &JudgeContext::default()); // streak 为空 = 首次观测
        assert_eq!(j.verdict, Verdict::Keep, "首轮不得直接判可结束");
        assert!(j.reasons.iter().any(|r| r.contains("需再确认")), "{:?}", j.reasons);
    }

    /// 持有**隐藏**顶层窗口 → 不是闲置。
    /// 这条专门挡托盘应用 / 输入法 / 常驻助手 —— 它们都没有可见窗口。
    #[test]
    fn hidden_window_caps_at_keep() {
        let mut ctx = ctx_with_streak(8200);
        ctx.window_pids_all = vec![8200];
        let p = RawProc {
            cpu_percent: 0.0,
            ..proc("tray-helper.exe", 8200)
        };
        let j = judge(&p, &ctx);
        assert_ne!(j.verdict, Verdict::Safe, "有隐藏窗口的进程不得判可结束");
    }

    /// 祖先链上有窗口进程 → 不是闲置。
    /// 这条挡 chrome 渲染进程、IDE 下的 node.exe、助手拉起的 shell。
    #[test]
    fn window_ancestor_caps_at_keep() {
        let mut ctx = ctx_with_streak(8300);
        ctx.window_ancestor_pids = vec![8300];
        let p = RawProc {
            cpu_percent: 0.0,
            ..proc("chrome.exe", 8300)
        };
        let j = judge(&p, &ctx);
        assert_ne!(j.verdict, Verdict::Safe, "属于在用程序的子进程不得判可结束");
    }

    /// 父进程是资源管理器 → 用户亲手启动的，不许动。
    #[test]
    fn shell_child_caps_at_keep() {
        let mut ctx = ctx_with_streak(8400);
        ctx.shell_pids = vec![1000]; // proc() 的 ppid
        let p = RawProc {
            cpu_percent: 0.0,
            ..proc("myapp.exe", 8400)
        };
        let j = judge(&p, &ctx);
        assert_ne!(j.verdict, Verdict::Safe, "用户从资源管理器启动的程序不得判可结束");
    }

    /// 输入法与杀软必须硬否决。
    #[test]
    fn ime_and_av_are_forbidden() {
        for n in [
            "iFlyInput.exe",
            "ctfmon.exe",
            "MpDefenderCoreService.exe",
            "DefenderSessionHelper.exe",
            "ScreenConnect.WindowsClient.exe",
            "gamingservices.exe",
            "WidgetBoard.exe",
            "NVIDIA Overlay.exe",
        ] {
            let j = judge(&proc(n, 9000), &JudgeContext::default());
            assert_eq!(j.verdict, Verdict::Forbidden, "{n} 必须被硬否决");
        }
    }

    /// 父进程还活着 → 不是被落下的残留，不许动。
    #[test]
    fn live_parent_caps_at_keep() {
        let mut ctx = ctx_with_streak(8600);
        ctx.live_pids = vec![1000]; // proc() 的 ppid
        let p = RawProc {
            cpu_percent: 0.0,
            ..proc("msedgewebview2.exe", 8600)
        };
        let j = judge(&p, &ctx);
        assert_ne!(j.verdict, Verdict::Safe, "父进程仍在运行的程序不得判可结束");
    }

    /// 刚启动的进程不许动 —— 短命工具进程不该进候选。
    #[test]
    fn fresh_process_is_not_safe() {
        let p = RawProc {
            ppid: 99999, // 父进程已退出，其他条件都满足
            cpu_percent: 0.0,
            working_set: 12 * 1024 * 1024,
            thread_count: 2,
            io_read: 0,
            io_write: 0,
            create_time: created_ago(60), // 只启动了 1 分钟
            ..proc("just-started.exe", 8700)
        };
        assert!(p.uptime_secs() < MIN_UPTIME_FOR_SAFE, "夹具应构造成年轻进程");
        let j = judge(&p, &ctx_with_streak(8700));
        assert_eq!(j.verdict, Verdict::Keep, "刚启动的进程不得判可结束");
        assert!(j.reasons.iter().any(|r| r.contains("刚启动")), "{:?}", j.reasons);
    }

    /// 真正该被清理的目标：无窗口、无窗口祖先、父进程已退出、连续空闲。
    #[test]
    fn genuine_leftover_is_safe() {
        let p = RawProc {
            ppid: 99999, // 父进程已不存在
            cpu_percent: 0.0,
            working_set: 12 * 1024 * 1024,
            thread_count: 2,
            io_read: 0,
            io_write: 0,
            ..proc("leftover-updater.exe", 8500)
        };
        let j = judge(&p, &ctx_with_streak(8500));
        assert_eq!(j.verdict, Verdict::Safe, "真正的残留进程应可清理，实际 {:?}", j.reasons);
    }
}
