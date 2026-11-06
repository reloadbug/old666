//! P0 技术验证 CLI —— 不需要 GUI 就能跑完全部技术风险点。
//!
//! 验证清单（对应 `docs/实施计划.md` P0 任务）：
//! - [x] `windows-rs` 进程枚举（`NtQuerySystemInformation`）
//! - [x] PDH 性能计数器采集 CPU / 内存 / 磁盘 / 网络
//! - [x] `TerminateProcess` 完整链路（两段式结束）
//! - [x] `EmptyWorkingSet` 工作集裁剪
//! - [x] 采集性能基线（进程数 / 耗时）
//! - [x] 提权状态与 `SeDebugPrivilege`
//!
//! ## 安全边界（重要）
//!
//! 本工具**只会结束它自己拉起的 `--child` 子进程**，绝不碰任何用户进程：
//!
//! - 结束链路验证前，会把 PID 白名单闸设为「仅这几个子进程 + 自身」，
//!   `killer` 的每个写操作都要过这道闸；
//! - 一键净化段落跑的是**演练模式**，只出计划、不做任何系统改动；
//! - 压力测试段落全部是演练，零副作用。
//!
//! 用法：
//! ```text
//! old666-poc                 # 人类可读报告
//! old666-poc --json          # 机器可读（给报告脚本用）
//! old666-poc --quiet         # 跳过净化段落
//! old666-poc --stress 200    # P1 零误杀压力测试（200 轮，零副作用）
//! old666-poc --child         # 内部用：长眠进程，作为被结束的测试目标
//! ```

use std::time::{Duration, Instant};

use old666_lib::core::{
    self, blacklist, killer, memory, perf, process, purge::PurgeEngine, purge::PurgeMode, safety,
};

/// 内部子进程模式：长眠，供父进程结束。
fn child_sleep() -> ! {
    std::thread::sleep(Duration::from_secs(600));
    std::process::exit(0)
}

/// 取 `--flag <值>` 形式的值。
fn value_of(args: &[String], flag: &str) -> Option<String> {
    let pos = args.iter().position(|a| a == flag)?;
    args.get(pos + 1).cloned()
}

/// `--svc-probe` 的实现：**只读**探测自动服务能不能被修改。
///
/// 只调用 `OpenServiceW(SERVICE_CHANGE_CONFIG)` 再立刻关掉，不写任何值。
/// 被安全软件自保护的服务即使管理员也打不开 —— 这类服务在界面上
/// 应该在**确认之前**就标成「改不动」，而不是等用户点了才报错。
fn svc_probe_main(json: bool) -> i32 {
    use old666_lib::core::startup_ctl::{self, Target};

    let svcs = old666_lib::core::startup::read_services();
    let auto: Vec<_> = svcs.iter().filter(|s| s.start == 2).collect();
    let mut ok = 0usize;
    let mut denied: Vec<(String, String, String)> = Vec::new();

    for s in &auto {
        let t = Target::Service {
            key_name: s.key_name.clone(),
        };
        match startup_ctl::probe_writable(&t) {
            Ok(()) => ok += 1,
            Err(e) => denied.push((s.key_name.clone(), s.display.clone(), e)),
        }
    }

    if json {
        let payload = serde_json::json!({
            "auto_start": auto.len(),
            "writable": ok,
            "protected": denied.len(),
            "items": denied.iter().map(|(k, d, e)| serde_json::json!({
                "key": k, "display": d, "reason": e,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
        return 0;
    }

    println!("自动启动服务：{} 个", auto.len());
    println!("  可修改      : {} 个", ok);
    println!("  受保护/改不动: {} 个", denied.len());
    if !denied.is_empty() {
        println!("\n改不动的服务（前 20 个）：");
        for (key, disp, why) in denied.iter().take(20) {
            println!("  {:<28} {:<34} {}", truncate(key, 28), truncate(disp, 34), why);
        }
        if denied.len() > 20 {
            println!("  … 另有 {} 个", denied.len() - 20);
        }
    }
    0
}

/// `--startup-apply` 的实现：真机验证禁用/延迟启动链路。
///
/// 安全边界：
/// * 默认演练（dry），必须显式 `--allow-write` 才真正动系统；
/// * 走 `apply_scoped` —— 白名单闸只覆盖这一个条目，执行完立刻撤销；
/// * 结果会打印批次 ID，配合 `--startup-restore` 做回滚实测。
fn startup_apply_main(action: &str, name: Option<&str>, allow_write: bool) -> i32 {
    use old666_lib::core::startup_ctl::{self, Mode};

    let Some(mode) = Mode::parse(action) else {
        eprintln!("未知 action：{action}（可用 disable / delay / undelay）");
        return 2;
    };
    let Some(name) = name else {
        eprintln!("缺少 --name <启动项名称或ID>");
        return 2;
    };

    let scan = old666_lib::core::startup::scan_deep();
    let Some(item) = scan
        .items
        .iter()
        .find(|i| i.name == name || i.id == name)
        .cloned()
    else {
        eprintln!("没找到启动项：{name}");
        eprintln!("（先用 --startup --json 看清单；服务名是注册表键名）");
        return 2;
    };

    let dry = !allow_write;
    let source_name = item_source_name(&item.source);
    let r = startup_ctl::apply_scoped_with(&[item.clone()], dry, mode);
    println!(
        "{} {}（{}）",
        if dry { "[演练] " } else { "[执行] " },
        action,
        source_name,
    );
    for c in &r.changes {
        if c.ok {
            println!("  ✓ {}  撤销载荷 {}", c.name, c.undo);
        } else {
            println!("  ✗ {}  {}", c.name, c.error.as_deref().unwrap_or("未知错误"));
        }
    }
    if !dry && !r.batch_id.is_empty() {
        println!("批次 ID：{}（还原：--startup-restore {}）", r.batch_id, r.batch_id);
    }
    0
}

fn item_source_name(s: &old666_lib::core::types::StartupSource) -> &'static str {
    use old666_lib::core::types::StartupSource::*;
    match s {
        RunKey => "注册表",
        StartupFolder => "启动文件夹",
        Service => "服务",
        ScheduledTask => "计划任务",
    }
}

/// `--startup-restore <批次ID>` 的实现。
fn startup_restore_main(batch: &str) -> i32 {
    use old666_lib::core::startup_ctl;
    if batch.trim().is_empty() {
        eprintln!("缺少批次 ID");
        return 2;
    }
    let (ok, fail, errs) = startup_ctl::restore(batch);
    println!("还原批次 {batch}：成功 {ok}，失败 {fail}");
    for e in &errs {
        println!("  ✗ {e}");
    }
    if fail == 0 {
        startup_ctl::purge_quarantine(batch);
    }
    if ok > 0 && fail == 0 {
        0
    } else {
        1
    }
}

// ---------------------------------------------------------------------------
// P3 · 场景模式（只读）
// ---------------------------------------------------------------------------

/// `--modes` / `--mode-plan <id>` 的实现：**只读**，不执行任何动作。
///
/// 输出的 JSON 形状与 IPC 的 `mode_list` / `mode_plan` 完全一致 ——
/// 这样前端的无头测试可以直接用真机导出的 payload，
/// 测的是真实契约，而不是手写的假数据。
fn modes_main(json: bool, plan_id: Option<String>, mem_pct_override: Option<u32>) -> i32 {
    use old666_lib::core::{memory, mode_actions, modes, startup};

    let modes_all = modes::load_all();
    let pct = mem_pct_override
        .unwrap_or_else(|| memory::memory_percent().round().max(0.0) as u32);

    if let Some(id) = plan_id {
        let Some(m) = modes::find(&id) else {
            eprintln!("没有找到模式：{id}");
            eprintln!("（先用 --modes 看清单）");
            return 2;
        };
        let scan = startup::scan();
        let plan = modes::plan(&m, pct, &scan.items);
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&plan).unwrap_or_else(|_| "{}".into())
            );
        } else {
            println!("模式：{}", plan.mode_name);
            println!("内存占用 {}%，互斥：{:?}", plan.mem_usage_pct, plan.excludes);
            for r in &plan.rules {
                println!(
                    "  [{}] {:<28} {}  {}",
                    if r.triggered { "命中" } else { "未中" },
                    r.title,
                    r.action,
                    if r.needs_confirm { "（需确认）" } else { "" }
                );
                if !r.targets.is_empty() {
                    for t in r.targets.iter().take(10) {
                        println!("        · {t}");
                    }
                }
            }
        }
        return 0;
    }

    let payload = serde_json::json!({
        "modes": modes_all,
        "active": mode_actions::active_mode_id(),
        "auto_switch": mode_actions::auto_switch(),
        "power_plan": mode_actions::current_power_plan().unwrap_or_default(),
        "mem_usage_pct": pct,
        "focus_assist": mode_actions::focus_assist().unwrap_or(false),
    });

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".into())
        );
        return 0;
    }

    let line = "─".repeat(64);
    println!("\n{line}");
    println!("  老六清净 old666 · P3 场景模式（只读，未做任何改动）");
    println!("{line}");
    println!("  当前生效：{}", mode_actions::active_mode_id());
    println!("  内存占用  : {pct}%");
    for m in &modes_all {
        println!(
            "\n【{}】{}（{} 条规则）{}",
            m.id,
            m.name,
            m.rules.len(),
            if m.builtin { "内置" } else { "自定义" }
        );
        for r in &m.rules {
            println!("    · {:<28} {}", r.title, r.then.name());
        }
    }
    println!("\n{line}\n");
    0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--child") {
        child_sleep();
    }

    // --stress N：跑零误杀压力测试后退出
    if let Some(pos) = args.iter().position(|a| a == "--stress") {
        let rounds: usize = args
            .get(pos + 1)
            .and_then(|v| v.parse().ok())
            .unwrap_or(200);
        let json = args.iter().any(|a| a == "--json");
        // 真实结束链路默认跳过，必须显式 --allow-write
        let allow_write = args.iter().any(|a| a == "--allow-write");
        std::process::exit(stress_main(rounds, json, allow_write));
    }

    // --boot：打印开机时间线（只读）
    if args.iter().any(|a| a == "--boot") {
        std::process::exit(boot_main(args.iter().any(|a| a == "--json")));
    }

    // --svc-probe：只读探测「哪些自动服务改不动」（不修改任何东西）。
    //   用途：解释「为什么点了禁用却报拒绝访问」，并统计受保护服务的比例。
    if args.iter().any(|a| a == "--svc-probe") {
        std::process::exit(svc_probe_main(args.iter().any(|a| a == "--json")));
    }

    // --modes：列出全部场景模式（只读，绝不改动系统）
    // --mode-plan <id>：输出某个模式的编排计划（只读，零副作用）
    // --mem-pct <n>：编排时假定内存占用为 n%（用来构造「条件命中」的计划，
    //   否则本机内存低于阈值时永远编不出会结束进程的计划，测不到确认闸）
    if args.iter().any(|a| a == "--modes") {
        std::process::exit(modes_main(
            args.iter().any(|a| a == "--json"),
            value_of(&args, "--mode-plan"),
            value_of(&args, "--mem-pct").and_then(|v| v.parse::<u32>().ok()),
        ));
    }

    // --startup：打印启动项清单（只读，绝不改动系统）
    // --fast：只跑快速扫描（不补签），用来观察「首屏那一版」长什么样
    if args.iter().any(|a| a == "--startup") {
        let json = args.iter().any(|a| a == "--json");
        let fast = args.iter().any(|a| a == "--fast");
        std::process::exit(startup_main(json, fast));
    }

    // --startup-apply：对单个启动项执行真实变更。
    //   默认仍是演练，必须显式 --allow-write 才会真的动系统。
    //   用途：真机验证延迟启动/禁用/还原链路（GUI 之外的另一条入口）。
    if args.iter().any(|a| a == "--startup-apply") {
        let action = value_of(&args, "--action").unwrap_or_else(|| "disable".into());
        let name = value_of(&args, "--name");
        let allow_write = args.iter().any(|a| a == "--allow-write");
        std::process::exit(startup_apply_main(&action, name.as_deref(), allow_write));
    }

    // --startup-restore <批次ID>：按备份还原一次真实变更。
    if args.iter().any(|a| a == "--startup-restore") {
        let batch = value_of(&args, "--startup-restore").unwrap_or_default();
        std::process::exit(startup_restore_main(&batch));
    }

    let json = args.iter().any(|a| a == "--json");
    let quiet = args.iter().any(|a| a == "--quiet");
    let list_safe = args.iter().any(|a| a == "--list-safe");
    // 写操作（结束进程 / 裁剪）默认关闭，必须显式开启
    let allow_write = args.iter().any(|a| a == "--allow-write");

    let mut rep = Report::default();

    // ================= 环境 =================
    rep.elevated = core::is_elevated();
    rep.debug_privilege = killer::enable_debug_privilege();
    rep.cpu_count = process::cpu_count();
    rep.portable = core::store::is_portable();
    rep.data_dir = core::store::data_dir().display().to_string();
    let (dn, sn, drv) = blacklist::coverage();
    rep.blacklist = (dn, sn, drv);

    // ================= PDH =================
    let sampler = perf::SystemSampler::new();
    rep.pdh_healthy = sampler.is_healthy();
    // PDH 速率计数器需要基线，先睡一下再采
    let (list0, _) = process::enumerate();
    std::thread::sleep(Duration::from_millis(600));
    let threads0: u32 = list0.iter().map(|p| p.thread_count).sum();
    let m = sampler.sample(list0.len() as u32, threads0);
    rep.metrics = m.clone();

    // ================= 枚举性能（10 次取中位与最大）=================
    let mut times: Vec<f64> = Vec::with_capacity(10);
    for _ in 0..10 {
        let (l, ms) = process::enumerate();
        rep.process_count = l.len() as u32;
        times.push(ms);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    rep.enumerate_median_ms = times[times.len() / 2];
    rep.enumerate_max_ms = *times.last().unwrap();

    // ================= 完整扫描（含路径查询）=================
    // 用演练引擎：本 CLI 不需要真实执行能力，少一个 Live 实例就少一分风险
    let mut engine = PurgeEngine::dry();
    engine.scan(true); // 第一轮建立 CPU 基线
    std::thread::sleep(Duration::from_millis(400));
    let (procs, _, stats) = engine.scan(true);
    rep.scan_total_ms = stats.total_ms;
    rep.scan_enumerate_ms = stats.enumerate_ms;
    rep.scan_detail_ms = stats.detail_ms;
    rep.process_count = stats.process_count;

    // 判定分布
    for p in &procs {
        match p.verdict {
            core::Verdict::Safe => rep.verdict_safe += 1,
            core::Verdict::Keep => rep.verdict_keep += 1,
            core::Verdict::Forbidden => rep.verdict_forbidden += 1,
        }
    }
    rep.total_mem = procs.iter().map(|p| p.working_set).sum();
    rep.safe_mem = procs
        .iter()
        .filter(|p| p.verdict == core::Verdict::Safe)
        .map(|p| p.working_set)
        .sum();

    // 判定质量诊断：直接跑一轮演练，把引擎算出的真实计划打出来
    // （不要在报告里重算一遍 —— 两套逻辑必然漂移）
    if list_safe && !json {
        let mut probe = PurgeEngine::dry();
        probe.scan(true);
        probe.scan(true); // 第二轮，满足「多轮确认」
        let plan = probe.purge(None, 50);

        println!("\n【计划结束的进程（共 {} 个，按内存降序）】", plan.killed.len());
        let mut kills = plan.killed.clone();
        kills.sort_by(|a, b| b.freed_bytes.cmp(&a.freed_bytes));
        for k in &kills {
            println!("  {:>7}  {}", k.pid, k.name);
        }

        println!(
            "\n【计划裁剪工作集的进程（共 {} 个，按内存降序）】",
            plan.trimmed.len()
        );
        let mut trims = plan.trimmed.clone();
        trims.sort_by(|a, b| b.freed_bytes.cmp(&a.freed_bytes));
        for t in &trims {
            println!(
                "  {:>7}  {:>9}  {}",
                t.pid,
                perf::human_bytes(t.freed_bytes),
                t.name
            );
        }
        println!("\n  预计可释放合计 : {}", perf::human_bytes(plan.planned_bytes));

        // 服务宿主是裁剪收益的主要来源，把它们的原始数据打出来，
        // 便于判断「一个都没裁到」到底是阈值问题还是环境问题
        let mut hosts: Vec<_> = procs
            .iter()
            .filter(|p| {
                let n = blacklist::normalize(&p.name);
                [
                    "svchost", "dllhost", "wmiprvse", "taskhostw", "spoolsv",
                    "runtimebroker", "fontdrvhost", "conhost", "openconsole",
                    "searchprotocolhost", "searchfilterhost",
                ]
                .contains(&n.as_str())
            })
            .collect();
        hosts.sort_by(|a, b| b.working_set.cmp(&a.working_set));
        println!("\n【服务宿主原始数据（共 {} 个，按内存降序，前 20）】", hosts.len());
        for p in hosts.iter().take(20) {
            println!(
                "  {:>7}  {:>9}  CPU{:>5.1}%  会话{}  托盘={}  {}",
                p.pid,
                perf::human_bytes(p.working_set),
                p.cpu_percent,
                p.session_id,
                if p.has_tray_window { "是" } else { "否" },
                p.name
            );
        }
        println!();
    }

    // ================= 签名校验 =================
    let t = Instant::now();
    let mut signed = 0;
    let mut unsigned = 0;
    for p in procs.iter().take(40) {
        if let Some(path) = p.exe_path.as_deref() {
            match core::signature::verify(path) {
                core::SignatureStatus::Valid(_) => signed += 1,
                core::SignatureStatus::Unsigned => unsigned += 1,
                _ => {}
            }
        }
    }
    rep.sig_check_ms = t.elapsed().as_secs_f64() * 1000.0;
    rep.sig_signed = signed;
    rep.sig_unsigned = unsigned;

    // ================= 写操作（结束进程 / 裁剪）=================
    //
    // 默认**全部跳过**，PoC 只做只读测量。
    //
    // 这是刻意的默认值：结束进程属于破坏性操作，任何自动化流程都不该
    // 在用户没点头的情况下执行。要跑真实链路必须同时满足两个条件：
    // 显式传 `--allow-write`，并且已经就目标清单征得用户确认。
    // 规则详见项目 skill：`.workbuddy-ai/skills/kill-process-confirmation`。
    let me = std::process::id();
    if allow_write {
        let exe = std::env::current_exe().expect("取自身路径");
        let mut spawned: Vec<u32> = Vec::new();
        for _ in 0..3 {
            if let Ok(c) = std::process::Command::new(&exe).arg("--child").spawn() {
                spawned.push(c.id());
                // 注意：这里**不能** `mem::forget(c)`。
                // 泄漏子进程句柄会让进程对象无法回收，之后 OpenProcess 仍会成功，
                // `is_alive` 就永远报「存活」，`still_alive` 指标直接失真。
                // `Child` 的 Drop 只关句柄、不结束进程，让它正常析构即可。
                drop(c);
            }
        }
        std::thread::sleep(Duration::from_millis(500));
        rep.spawned = spawned.len() as u32;

        // 安全闸：从这里到本段结束，killer 只可能动「自己 + 自己拉起的子进程」。
        // 即便后续代码写错 PID，也杀不到任何用户进程。
        let mut allowed: std::collections::HashSet<u32> = spawned.iter().copied().collect();
        allowed.insert(me);
        safety::set_kill_allowlist(Some(allowed));

        let t = Instant::now();
        for pid in &spawned {
            // 名字传空串 → killer 自己查映像名，避免调用方漏校验
            let (method, err) = killer::kill_two_stage(*pid, "", 300);
            if err.is_none() && method != killer::KillMethod::AlreadyGone {
                rep.killed += 1;
            } else if err.is_some() {
                rep.kill_errors.push(format!("PID {pid}: {}", err.unwrap()));
            }
        }
        rep.kill_ms = t.elapsed().as_secs_f64() * 1000.0;
        std::thread::sleep(Duration::from_millis(200));
        rep.still_alive = spawned.iter().filter(|p| killer::is_alive(**p)).count() as u32;

        // 反向验证：闸门确实拦得住。这个 PID 属于真实系统进程，必须被拒。
        rep.gate_blocks_system = killer::force_kill(4, "System").is_err();

        // EmptyWorkingSet：只对自己，不动别的进程
        let before = memory::working_set_of(me).unwrap_or(0);
        match killer::trim_working_set(me, "", before) {
            Ok(freed) => {
                rep.trim_ok = true;
                rep.trim_freed = freed;
            }
            Err(e) => rep.trim_errors.push(e),
        }
        rep.self_ws_before = before;
        rep.self_ws_after = memory::working_set_of(me).unwrap_or(0);

        // 本段结束，撤销 PID 白名单闸
        safety::set_kill_allowlist(None);
    } else {
        rep.write_skipped = true;
    }

    // ================= 净化主流程（演练，零系统改动）=================
    // 注意：这里刻意用 DryRun。P0 报告要的是「判定与收益估算」，
    // 不需要真的去结束用户进程 —— 报告里也不该出现「已结束 N 个」这种
    // 既成事实的描述。
    if !quiet {
        let mut engine = PurgeEngine::with_mode(PurgeMode::DryRun);
        engine.scan(true);
        std::thread::sleep(Duration::from_millis(400));
        let r = engine.purge(None, 300);
        rep.purge_killed = r.killed.iter().filter(|k| k.error.is_none()).count() as u32;
        rep.purge_trimmed = r.trimmed.iter().filter(|k| k.error.is_none()).count() as u32;
        rep.purge_blocked = r.blocked.len() as u32;
        rep.purge_planned = r.planned_bytes;
        rep.purge_ms = r.elapsed_ms;
        rep.purge_summary = core::purge::summarize(&r);
        // 二次校验：确认没有任何系统进程混进结束队列
        for k in &r.killed {
            if safety::check_target(k.pid, Some(&k.name)).is_err() {
                rep.misfires.push(format!("{} (PID {})", k.name, k.pid));
            }
        }
    }

    if json {
        println!("{}", rep.to_json());
    } else {
        rep.print_human();
    }
}

#[derive(Default)]
struct Report {
    elevated: bool,
    debug_privilege: bool,
    cpu_count: u32,
    portable: bool,
    data_dir: String,
    blacklist: (usize, usize, usize),
    pdh_healthy: bool,
    metrics: core::SystemMetrics,

    process_count: u32,
    enumerate_median_ms: f64,
    enumerate_max_ms: f64,
    scan_total_ms: f64,
    scan_enumerate_ms: f64,
    scan_detail_ms: f64,

    verdict_safe: u32,
    verdict_keep: u32,
    verdict_forbidden: u32,
    total_mem: u64,
    safe_mem: u64,

    sig_check_ms: f64,
    sig_signed: u32,
    sig_unsigned: u32,

    spawned: u32,
    killed: u32,
    kill_ms: f64,
    still_alive: u32,
    kill_errors: Vec<String>,
    /// 反向验证：安全闸是否成功拦住了系统进程
    gate_blocks_system: bool,
    /// 写操作段落是否被跳过（默认即为 true）
    write_skipped: bool,

    trim_ok: bool,
    trim_freed: u64,
    trim_errors: Vec<String>,
    self_ws_before: u64,
    self_ws_after: u64,

    purge_killed: u32,
    purge_trimmed: u32,
    purge_blocked: u32,
    /// 演练模式下「预计可释放」的字节数
    purge_planned: u64,
    purge_ms: u64,
    purge_summary: String,
    misfires: Vec<String>,
}

impl Report {
    fn to_json(&self) -> String {
        let s = |v: &str| serde_json::to_string(v).unwrap_or_else(|_| "\"\"".into());
        format!(
            r#"{{
  "env": {{ "elevated": {}, "debug_privilege": {}, "cpu_count": {}, "portable": {}, "data_dir": {} }},
  "blacklist": {{ "deny_names": {}, "critical_services": {}, "critical_drivers": {} }},
  "pdh": {{ "healthy": {}, "cpu_percent": {:.1}, "mem_total": {}, "mem_used": {}, "disk_read_bps": {:.0}, "disk_write_bps": {:.0}, "net_rx_bps": {:.0}, "net_tx_bps": {:.0} }},
  "enumerate": {{ "process_count": {}, "median_ms": {:.3}, "max_ms": {:.3} }},
  "scan": {{ "total_ms": {:.3}, "enumerate_ms": {:.3}, "detail_ms": {:.3} }},
  "verdict": {{ "safe": {}, "keep": {}, "forbidden": {}, "total_mem": {}, "safe_mem": {} }},
  "signature": {{ "checked_ms": {:.3}, "signed": {}, "unsigned": {} }},
  "kill_chain": {{ "skipped": {}, "spawned": {}, "killed": {}, "elapsed_ms": {:.1}, "still_alive": {}, "errors": {}, "gate_blocks_system": {} }},
  "empty_working_set": {{ "ok": {}, "freed_bytes": {}, "ws_before": {}, "ws_after": {}, "errors": {} }},
  "purge_dry_run": {{ "planned_kills": {}, "planned_trims": {}, "blocked": {}, "planned_bytes": {}, "elapsed_ms": {}, "misfires": {} }}
}}"#,
            self.elevated,
            self.debug_privilege,
            self.cpu_count,
            self.portable,
            s(&self.data_dir),
            self.blacklist.0,
            self.blacklist.1,
            self.blacklist.2,
            self.pdh_healthy,
            self.metrics.cpu_percent,
            self.metrics.mem_total,
            self.metrics.mem_used,
            self.metrics.disk_read_bps,
            self.metrics.disk_write_bps,
            self.metrics.net_rx_bps,
            self.metrics.net_tx_bps,
            self.process_count,
            self.enumerate_median_ms,
            self.enumerate_max_ms,
            self.scan_total_ms,
            self.scan_enumerate_ms,
            self.scan_detail_ms,
            self.verdict_safe,
            self.verdict_keep,
            self.verdict_forbidden,
            self.total_mem,
            self.safe_mem,
            self.sig_check_ms,
            self.sig_signed,
            self.sig_unsigned,
            self.write_skipped,
            self.spawned,
            self.killed,
            self.kill_ms,
            self.still_alive,
            json_arr(&self.kill_errors),
            self.gate_blocks_system,
            self.trim_ok,
            self.trim_freed,
            self.self_ws_before,
            self.self_ws_after,
            json_arr(&self.trim_errors),
            self.purge_killed,
            self.purge_trimmed,
            self.purge_blocked,
            self.purge_planned,
            self.purge_ms,
            json_arr(&self.misfires),
        )
    }

    fn print_human(&self) {
        let hb = perf::human_bytes;
        let hr = perf::human_rate;
        let line = "─".repeat(64);

        println!("\n{line}");
        println!("  老六清净 old666 · P0 技术验证基线");
        println!("{line}");

        println!("\n【环境】");
        println!("  管理员权限        : {}", yn(self.elevated));
        println!("  SeDebugPrivilege  : {}", yn(self.debug_privilege));
        println!("  逻辑处理器        : {}", self.cpu_count);
        println!("  便携模式          : {}", yn(self.portable));
        println!("  数据目录          : {}", self.data_dir);
        println!(
            "  黑名单覆盖        : {} 进程 / {} 服务 / {} 驱动",
            self.blacklist.0, self.blacklist.1, self.blacklist.2
        );

        println!("\n【PDH 性能计数器】");
        println!("  可用              : {}", yn(self.pdh_healthy));
        if self.pdh_healthy {
            println!("  CPU               : {:.1}%", self.metrics.cpu_percent);
            println!(
                "  内存              : {} / {}",
                hb(self.metrics.mem_used),
                hb(self.metrics.mem_total)
            );
            println!(
                "  磁盘              : 读 {}  写 {}",
                hr(self.metrics.disk_read_bps),
                hr(self.metrics.disk_write_bps)
            );
            println!(
                "  网络              : 收 {}  发 {}",
                hr(self.metrics.net_rx_bps),
                hr(self.metrics.net_tx_bps)
            );
        }

        println!("\n【枚举性能】");
        println!("  进程数            : {}", self.process_count);
        println!(
            "  枚举中位/最大     : {:.2} ms / {:.2} ms",
            self.enumerate_median_ms, self.enumerate_max_ms
        );
        println!(
            "  完整扫描          : {:.2} ms（枚举 {:.2} + 明细 {:.2}）",
            self.scan_total_ms, self.scan_enumerate_ms, self.scan_detail_ms
        );
        let budget = 200.0;
        println!(
            "  验收 (≤{budget:.0}ms)     : {}",
            if self.scan_total_ms <= budget {
                "通过 ✓"
            } else {
                "未达标 ✗"
            }
        );

        println!("\n【判定分布】");
        println!("  可安全结束        : {}", self.verdict_safe);
        println!("  建议保留          : {}", self.verdict_keep);
        println!("  禁止结束          : {}", self.verdict_forbidden);
        println!(
            "  可释放内存上限    : {} / 共 {}",
            hb(self.safe_mem),
            hb(self.total_mem)
        );

        println!("\n【签名校验】");
        println!(
            "  抽样 40 个耗时    : {:.1} ms",
            self.sig_check_ms
        );
        println!("  有效签名 / 无签名 : {} / {}", self.sig_signed, self.sig_unsigned);

        println!("\n【结束进程链路】");
        if self.write_skipped {
            println!("  状态              : 已跳过（默认只读）");
            println!("  说明              : 结束进程属于破坏性操作，");
            println!("                      需显式传 --allow-write 并经用户确认后才执行。");
            println!("                      规则见 .workbuddy-ai/skills/kill-process-confirmation");
        } else {
            println!("  说明              : 只结束本工具自己拉起的子进程");
            println!("  拉起测试进程      : {}", self.spawned);
            println!("  成功结束          : {}", self.killed);
            println!("  耗时              : {:.1} ms", self.kill_ms);
            println!("  仍在存活          : {}", self.still_alive);
            println!(
                "  安全闸拦截验证    : {}",
                if self.gate_blocks_system {
                    "系统进程被正确拒绝 ✓"
                } else {
                    "未拦住系统进程 ✗"
                }
            );
            if !self.kill_errors.is_empty() {
                println!("  错误              : {:?}", self.kill_errors);
            }
        }

        println!("\n【EmptyWorkingSet 工作集裁剪】");
        if self.write_skipped {
            println!("  状态              : 已跳过（默认只读）");
        } else {
            println!("  说明              : 只裁剪本进程自身");
            println!("  执行成功          : {}", yn(self.trim_ok));
            println!(
                "  自身工作集        : {} → {}（释放 {}）",
                hb(self.self_ws_before),
                hb(self.self_ws_after),
                hb(self.trim_freed)
            );
            if !self.trim_errors.is_empty() {
                println!("  错误              : {:?}", self.trim_errors);
            }
        }

        if !self.purge_summary.is_empty() {
            println!("\n【一键净化 · 演练模式（未对系统做任何改动）】");
            println!("  计划结束进程      : {}", self.purge_killed);
            println!("  计划工作集裁剪    : {}", self.purge_trimmed);
            println!("  安全拦截          : {}", self.purge_blocked);
            println!("  预计可释放内存    : {}", hb(self.purge_planned));
            println!("  耗时              : {} ms", self.purge_ms);
            println!("  结论              : {}", self.purge_summary);
            println!(
                "  误杀系统进程      : {}",
                if self.misfires.is_empty() {
                    "0 ✓".to_string()
                } else {
                    format!("{:?} ✗", self.misfires)
                }
            );
        }

        println!("\n{line}\n");
    }
}

// ---------------------------------------------------------------------------
// P1 零误杀压力测试（全部演练，零副作用）
// ---------------------------------------------------------------------------

/// 跑 `rounds` 轮净化演练，统计误杀。
///
/// 演练部分**完全只读**，零副作用。真实结束链路默认跳过，
/// 只有显式传 `--allow-write` 才会执行 —— 且只针对自己拉起的子进程。
///
/// 返回进程退出码：0 = 全部通过，1 = 有误杀或链路失败。
fn stress_main(rounds: usize, json: bool, allow_write: bool) -> i32 {
    // 数据目录指向临时目录，确保整轮跑完不留任何痕迹（白名单/快照都不会被碰）
    let tmp = std::env::temp_dir().join(format!("old666-stress-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    std::env::set_var("OLD666_DATA_DIR", &tmp);

    let mut misfires: Vec<String> = Vec::new();
    let mut queued = 0usize;
    let mut blocked = 0usize;
    let mut planned: u64 = 0;
    let mut max_ms: u64 = 0;
    let mut total_ms: u64 = 0;

    let mut engine = PurgeEngine::with_mode(PurgeMode::DryRun);
    engine.scan(true); // 建立 CPU 基线

    for i in 0..rounds {
        let r = engine.purge(None, 30);
        total_ms += r.elapsed_ms;
        max_ms = max_ms.max(r.elapsed_ms);
        planned = planned.max(r.planned_bytes);
        queued += r.killed.len();
        blocked += r.blocked.len();

        // 逐条复核：任何进入结束队列的进程都必须通过红线校验
        for k in &r.killed {
            if safety::check_target(k.pid, Some(&k.name)).is_err() {
                misfires.push(format!("第 {} 轮：{} (PID {})", i + 1, k.name, k.pid));
            }
        }
        // 自身永远不该出现在结束队列里
        if r.killed.iter().any(|k| k.pid == std::process::id()) {
            misfires.push(format!("第 {} 轮：把自己排进了结束队列", i + 1));
        }
    }

    // ---- 真实结束链路：默认跳过，只动自己拉起的 3 个子进程 ----
    let mut children: Vec<u32> = Vec::new();
    let mut chain_ok = 0usize;
    let mut chain_err: Vec<String> = Vec::new();
    let mut gate_ok = true;

    if allow_write {
        let exe = std::env::current_exe().expect("取自身路径");
        for _ in 0..3 {
            if let Ok(c) = std::process::Command::new(&exe).arg("--child").spawn() {
                children.push(c.id());
                drop(c); // 见上方说明：不可 mem::forget
            }
        }
        std::thread::sleep(Duration::from_millis(500));

        let mut allowed: std::collections::HashSet<u32> = children.iter().copied().collect();
        allowed.insert(std::process::id());
        safety::set_kill_allowlist(Some(allowed));

        for pid in &children {
            let (m, e) = killer::kill_two_stage(*pid, "", 300);
            if e.is_none() && m != killer::KillMethod::AlreadyGone {
                chain_ok += 1;
            } else if let Some(e) = e {
                chain_err.push(format!("PID {pid}: {e}"));
            }
        }
        // 反向验证：闸门必须拦住真实系统进程
        gate_ok = killer::force_kill(4, "System").is_err();
        safety::set_kill_allowlist(None);
    }

    let avg_ms = total_ms as f64 / rounds.max(1) as f64;
    let pass = misfires.is_empty() && (!allow_write || (chain_ok == children.len() && gate_ok));
    let _ = std::fs::remove_dir_all(&tmp);

    if json {
        println!(
            r#"{{
  "rounds": {rounds},
  "queued_total": {queued},
  "blocked_total": {blocked},
  "misfires": {},
  "max_planned_bytes": {planned},
  "purge_avg_ms": {avg_ms:.2},
  "purge_max_ms": {max_ms},
  "real_chain": {{ "skipped": {}, "spawned": {}, "killed": {}, "errors": {} }},
  "gate_blocks_system": {gate_ok},
  "pass": {pass}
}}"#,
            json_arr(&misfires),
            !allow_write,
            children.len(),
            chain_ok,
            json_arr(&chain_err),
        );
    } else {
        let line = "─".repeat(64);
        println!("\n{line}");
        println!("  老六清净 old666 · P1 零误杀压力测试");
        println!("{line}");
        println!("\n【净化演练】（完全只读，零副作用）");
        println!("  轮数              : {rounds}");
        println!("  累计排队进程      : {queued}");
        println!("  累计安全拦截      : {blocked}");
        println!("  误杀              : {}", misfires.len());
        println!("  单轮耗时 均值/峰值: {avg_ms:.1} ms / {max_ms} ms");
        println!("  预计可释放上限    : {}", perf::human_bytes(planned));
        if !misfires.is_empty() {
            for m in misfires.iter().take(10) {
                println!("    ✗ {m}");
            }
        }

        println!("\n【真实结束链路】");
        if !allow_write {
            println!("  状态              : 已跳过（默认只读）");
            println!("  说明              : 结束进程属于破坏性操作，");
            println!("                      需显式传 --allow-write 并经用户确认后才执行。");
            println!("                      规则见 .workbuddy-ai/skills/kill-process-confirmation");
        } else {
            println!("  说明              : 仅自拉起的子进程");
            println!("  拉起              : {}", children.len());
            println!("  成功结束          : {chain_ok}");
            for e in &chain_err {
                println!("    ✗ {e}");
            }
            println!(
                "  安全闸拦截验证    : {}",
                if gate_ok { "通过 ✓" } else { "失败 ✗" }
            );
        }

        println!(
            "\n  结论              : {}",
            if pass {
                "通过 ✓ 零误杀，且未对系统造成任何改动"
            } else {
                "未通过 ✗"
            }
        );
        println!("{line}\n");
    }

    if pass {
        0
    } else {
        1
    }
}

fn yn(b: bool) -> &'static str {
    if b {
        "是 ✓"
    } else {
        "否"
    }
}

fn json_arr(v: &[String]) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".into())
}

// ---------------------------------------------------------------------------
// P2 · 开机时间线（只读）
// ---------------------------------------------------------------------------

fn boot_main(json: bool) -> i32 {
    use old666_lib::core::boot;

    let t0 = Instant::now();
    let tl = boot::timeline(8);
    let elapsed = t0.elapsed().as_secs_f64() * 1000.0;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&tl).unwrap_or_else(|_| "{}".into())
        );
        return if tl.available { 0 } else { 2 };
    }

    let line = "─".repeat(64);
    println!("\n{line}");
    println!("  老六清净 old666 · P2 开机时间线");
    println!("{line}");

    if !tl.available {
        println!("\n  数据源不可用：{}", tl.note);
        println!("  读取耗时          : {elapsed:.1} ms");
        println!("{line}\n");
        return 2;
    }

    println!(
        "\n  历史 {}/最快 {}/均值 {}/最慢 {}（读取耗时 {:.1} ms）",
        tl.records.len(),
        perf::human_ms(tl.best_ms),
        perf::human_ms(tl.avg_ms),
        perf::human_ms(tl.worst_ms),
        elapsed
    );

    for (i, r) in tl.records.iter().enumerate() {
        println!("\n【第 {} 次 · {}】", i + 1, if r.start_time.is_empty() { "时间未知" } else { &r.start_time });
        println!(
            "  总耗时 {}（主路径 {} + 桌面后台 {}）{}",
            perf::human_ms(r.boot_ms),
            perf::human_ms(r.main_path_ms),
            perf::human_ms(r.post_boot_ms),
            if r.degraded { "  ⚠ 系统标记为「降级」" } else { "" }
        );
        // 分段条形图
        let max = r.phases.iter().map(|p| p.ms).max().unwrap_or(1).max(1);
        for p in &r.phases {
            let width = ((p.ms as f64 / max as f64) * 28.0).round() as usize;
            println!(
                "    {:>10}  {:<10}  {}{}",
                perf::human_ms(p.ms),
                p.label,
                "█".repeat(width.max(1)),
                if p.actionable { "  ← 可优化" } else { "" }
            );
        }
        if !r.culprits.is_empty() {
            println!("  拖慢开机的对象：");
            for c in r.culprits.iter().take(8) {
                println!(
                    "    [{:<7}] {:<34} 拖慢 {}（共 {}）",
                    c.kind,
                    c.name,
                    perf::human_ms(c.degradation_ms),
                    perf::human_ms(c.total_ms)
                );
            }
        }
    }
    println!("\n{line}\n");
    0
}

// ---------------------------------------------------------------------------
// P2 · 启动项清单（只读）
// ---------------------------------------------------------------------------

fn startup_main(json: bool, fast: bool) -> i32 {
    use old666_lib::core::{boot, startup};

    // 第一阶段：快速扫描（不验签），要求首屏足够快
    let t0 = Instant::now();
    let mut scan = startup::scan();
    let scan_ms = t0.elapsed().as_secs_f64() * 1000.0;

    // 第二阶段：补齐签名（这一步要读几百 MB 文件）。--fast 用于只看首屏形态。
    let (verify_ms, n, disable_after) = if fast {
        (0.0, 0, 0)
    } else {
        let t1 = Instant::now();
        let ids: std::collections::HashSet<String> =
            scan.items.iter().map(|i| i.id.clone()).collect();
        let n = startup::verify_pending(&mut scan.items, Some(&ids));
        let after = scan
            .items
            .iter()
            .filter(|i| i.enabled && i.can_disable && i.risk == old666_lib::core::RiskLevel::Safe)
            .count();
        (t1.elapsed().as_secs_f64() * 1000.0, n, after)
    };

    // 把开机归因挂到同名启动项上，便于看清「谁真的拖慢了开机」
    if let Some(r) = boot::latest() {
        startup::attach_impact(&mut scan.items, &r.culprits);
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&scan).unwrap_or_else(|_| "{}".into())
        );
        return 0;
    }
    let line = "─".repeat(64);
    println!("\n{line}");
    println!("  老六清净 old666 · P2 启动项清单（只读，未做任何改动）");
    println!("{line}");

    let total: usize = scan.counts.iter().map(|(_, n)| n).sum();
    println!(
        "\n  合计 {} 项（{} 个条目参与分级），耗时 {:.1} ms，管理员权限{}",
        scan.items.len(),
        total,
        scan_ms,
        if scan.needs_admin {
            "：否，部分项可能枚举不全"
        } else {
            "：是"
        }
    );
    for (k, n) in &scan.counts {
        println!("    {:<16} {}", k, n);
    }
    println!(
        "  可安全禁用候选    : {}（补签前 {}）",
        disable_after, scan.disable_candidates
    );
    if fast {
        println!("  签名补齐          : 已跳过（--fast，观察的是首屏那一版）");
    } else {
        println!("  签名补齐          : {} 条，耗时 {:.1} ms", n, verify_ms);
    }
    for (name, ms) in &scan.stage_ms {
        println!("    · {:<18} {:>5} ms", name, ms);
    }

    let risk_of = |it: &old666_lib::core::StartupItem| match it.risk {
        old666_lib::core::RiskLevel::Safe => 0,
        old666_lib::core::RiskLevel::Caution => 1,
        old666_lib::core::RiskLevel::Unknown => 2,
        old666_lib::core::RiskLevel::Dangerous => 3,
    };

    for src in [
        old666_lib::core::StartupSource::RunKey,
        old666_lib::core::StartupSource::StartupFolder,
        old666_lib::core::StartupSource::Service,
        old666_lib::core::StartupSource::ScheduledTask,
    ] {
        let mut list: Vec<&old666_lib::core::StartupItem> =
            scan.items.iter().filter(|i| i.source == src).collect();
        if list.is_empty() {
            continue;
        }
        list.sort_by_key(|i| (risk_of(i), i.name.to_ascii_lowercase()));
        println!("\n【{}】共 {} 项", src.label(), list.len());
        for it in list {
            let impact = it
                .impact_ms
                .map(|ms| format!("  拖慢开机 {}", perf::human_ms(ms)))
                .unwrap_or_default();
            println!(
                "  [{:<7}] {:<8} {:<30} {}{}",
                it.risk.label(),
                if it.enabled { "已启用" } else { "已禁用" },
                truncate(&it.name, 30),
                if it.can_disable { "" } else { "🔒 " },
                impact
            );
            println!("            理由：{}", it.risk_reason);
            if !it.command.is_empty() {
                println!("            命令：{}", truncate(&it.command, 76));
            }
        }
    }

    println!("\n{line}\n");
    0
}

/// 按字符数截断（中文按 1 个字算，避免按字节切坏 UTF-8）。
fn truncate(s: &str, max_chars: usize) -> String {
    let n = s.chars().count();
    if n <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{head}…")
}
