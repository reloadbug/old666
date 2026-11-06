//! 老六清净 old666 —— 核心库。
//!
//! 这个 crate 同时产出三种产物：
//! - `old666_lib`（lib）—— 核心服务层 + Tauri 命令层，供 GUI 与测试复用；
//! - `old666`（bin）—— Tauri GUI 应用；
//! - `old666-poc`（bin）—— P0 性能基线 CLI，无需 GUI 即可跑枚举/采集/净化。
//!
//! 之所以把核心逻辑放在 lib 而不是 main，是为了让 `cargo test` 能直接
//! 覆盖系统调用路径 —— P1 的「零误杀」验收就是靠 `core::purge` 的测试跑的。

use tauri::Manager;

pub mod core;
pub mod ipc;

pub use core::PurgeEngine;

/// 启动时刻。放在 `run()` 的第一行，用于测量「冷启动到可交互」。
///
/// 注意这**不包含** exe 加载与 DLL 重定位的时间（那发生在 main 之前），
/// 所以 P0 报告里同时给出「进程启动 → 窗口出现」的端到端测量值。
pub static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// 启动至今的毫秒数。
pub fn since_start_ms() -> f64 {
    START
        .get()
        .map(|t| t.elapsed().as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

/// 启动 GUI 应用。
///
/// 注意：`generate_context!` 会读 `tauri.conf.json` 与 `icons/`，
/// 因此这个函数只能在 bin 里调用，不能进单元测试。
pub fn run() {
    let _ = START.set(std::time::Instant::now());
    let bench = std::env::args().any(|a| a == "--bench-startup");

    tauri::Builder::default()
        // 单实例保护：第二个实例不再静默退出（退出码 0，看起来像"点了没反应"），
        // 而是把已有窗口拉到前台。必须最先注册。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            eprintln!("[old666] 检测到第二实例，尝试激活已有窗口");
            match app.get_webview_window("main") {
                Some(w) => {
                    let _ = w.unminimize();
                    let _ = w.show();
                    let _ = w.set_focus();
                    eprintln!("[old666] 已有窗口已激活");
                }
                None => eprintln!("[old666] 警告：未找到 main 窗口"),
            }
        }))
        .manage(ipc::AppState::default())
        .setup(move |app| {
            // 尽早拿到 SeDebugPrivilege，后续结束高权限进程才不会失败
            let elevated = core::is_elevated();
            let dbg = core::killer::enable_debug_privilege();
            let pdh = core::perf::SystemSampler::new().is_healthy();
            let ms = since_start_ms();
            eprintln!(
                "[old666] 启动完成 {ms:.1} ms ｜ 管理员={elevated} SeDebugPrivilege={dbg} PDH={pdh}"
            );

            // P5 · 常驻监控：配置默认开着，开机就跑起来。
            // 采样在后台线程里，5 秒一轮，开销远低于 0.5% CPU。
            // OLD666_NO_MONITOR=1 可临时关掉（用于对照测量）。
            let hub = app.state::<ipc::AppState>().monitor.clone();
            let no_monitor = std::env::var("OLD666_NO_MONITOR")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            if !no_monitor && hub.config().enabled {
                hub.start();
            }

            // P5 · 悬浮球：上次是显示状态就把它摆回来
            let fb = core::floatball::FloatBallState::load();
            if fb.visible {
                let handle = app.handle().clone();
                let size = fb.size;
                let (x, y) = (fb.x, fb.y);
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    let _ = ipc::create_floatball_window(&handle, size, x, y);
                });
            }

            if bench {
                // 基准模式：打完时间就退出，供 scripts 反复测量取中位数
                println!("{ms:.1}");
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(80));
                    handle.exit(0);
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ipc::scan_processes,
            ipc::purge_processes,
            ipc::purge_summary,
            ipc::whitelist_list,
            ipc::whitelist_add,
            ipc::whitelist_remove,
            ipc::snapshot_list,
            ipc::snapshot_rollback,
            ipc::env_info,
            // P2 · 开机加速
            ipc::boot_timeline,
            ipc::startup_scan,
            ipc::startup_verify,
            ipc::startup_scan_deep,
            ipc::startup_plan,
            ipc::startup_apply,
            ipc::startup_restore,
            ipc::startup_batches,
            // P2 · 启动项知识库与名单
            ipc::knowledge_stats,
            ipc::knowledge_save,
            ipc::knowledge_remove,
            ipc::startup_lists,
            ipc::startup_list_add,
            ipc::startup_list_remove,
            // P3 · 场景模式
            ipc::mode_list,
            ipc::mode_plan,
            ipc::mode_apply,
            ipc::mode_set_active,
            ipc::mode_auto_get,
            ipc::mode_auto_check,
            ipc::mode_auto_set,
            ipc::mode_custom_save,
            ipc::mode_custom_delete,
            // P4 · 一键体检 + 垃圾清理
            ipc::checkup_run,
            ipc::junk_scan,
            ipc::junk_delete,
            ipc::checkup_fix,
            // P5 · 常驻监控
            ipc::monitor_snapshot,            ipc::monitor_configure,
            ipc::monitor_toggle,
            ipc::monitor_clear_alerts,
            // P5 · 悬浮球
            ipc::floatball_state,
            ipc::floatball_save,
            ipc::floatball_show,
            // P5 · 窗口空闲探测（前端据此暂停高频扫描）
            ipc::window_idle,
        ])
        .run(tauri::generate_context!())
        .expect("启动 Tauri 应用失败");
}
