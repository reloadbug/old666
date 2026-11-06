//! P3 · 自动切换的触发判定。
//!
//! 这一层刻意**只做一件事**：判断「当前该不该把生效模式切成某个模式」。
//!
//! 它**不执行任何动作** —— 不结束进程、不裁剪内存、不改电源计划。
//! 自动切换的语义是「我提醒你进入了某个场景 / 系统到了某个压力点」，
//! 真正的动作仍然必须由用户点「按计划执行」并逐次确认。
//!
//! 这条边界很重要：如果自动切换顺手把动作做了，那
//! 「所有结束进程的操作都要用户逐次确认」这条红线就被绕过去了。

use super::memory;
use super::mode_actions::{active_mode_id, auto_switch};
use super::killer;

/// 判定结果。
#[derive(PartialEq, Debug, Clone)]
pub enum Decision {
    /// 不需要切换。
    None,
    /// 切到指定模式。
    Switch(String),
}

/// 判定当前是否应该切换模式。
///
/// * `mem_above_pct` —— 内存占用超过该百分比时切到 `mem_above_mode`。
/// * `foreground_process` —— 前台进程名（exe 名，不含扩展名）匹配时
///   切到 `foreground_mode`。
///
/// 优先级：前台程序匹配 > 内存超限。理由是「我正在打游戏」比
/// 「内存占用 85%」更能说明我处于哪个场景。
pub fn evaluate() -> Decision {
    let cfg = auto_switch();
    if !cfg.enabled {
        return Decision::None;
    }

    // ---- 前台程序 ----
    if let (Some(proc), Some(target)) = (
        cfg.foreground_process.as_deref(),
        cfg.foreground_mode.as_deref(),
    ) {
        if !proc.trim().is_empty() && foreground_matches(proc) {
            if target != active_mode_id() {
                return Decision::Switch(target.to_string());
            }
            // 已经在这个模式上了 —— 内存条件再判断一次，但要保证不会
            // 把它切走（同一个目标）。
            if let Some(t) = mem_target(&cfg) {
                if t != active_mode_id() {
                    return Decision::Switch(t);
                }
            }
            return Decision::None;
        }
    }

    // ---- 内存超限 ----
    if let Some(t) = mem_target(&cfg) {
        if t != active_mode_id() {
            return Decision::Switch(t);
        }
    }

    Decision::None
}

/// 内存条件命中时返回目标模式 id。
fn mem_target(cfg: &super::mode_actions::AutoSwitch) -> Option<String> {
    let target = cfg.mem_above_mode.as_deref()?;
    if target.trim().is_empty() {
        return None;
    }
    let pct = cfg.mem_above_pct.unwrap_or(85);
    let cur = memory::memory_percent();
    if cur >= pct as f32 {
        Some(target.to_string())
    } else {
        None
    }
}

/// 进程名归一化：小写、去掉 `.exe` 扩展名。
///
/// 注意顺序：**先小写再剥扩展名** —— `trim_end_matches(".exe")`
/// 是大小写敏感的，`Game.EXE` 不会被剥掉。
fn normalize_proc_name(s: &str) -> String {
    s.trim()
        .to_ascii_lowercase()
        .trim_end_matches(".exe")
        .to_string()
}

/// 当前前台进程名是否匹配（不区分大小写，忽略 `.exe` 扩展名）。
fn foreground_matches(want: &str) -> bool {
    let want = normalize_proc_name(want);
    if want.is_empty() {
        return false;
    }
    let pid = super::process::foreground_pid();
    if pid == 0 {
        return false;
    }
    let Some(name) = killer::image_name(pid) else {
        return false;
    };
    let name = normalize_proc_name(&name);
    name == want || name.ends_with(&format!("\\{want}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_config_never_switches() {
        // 默认配置（enabled=false）必须永远返回 None。
        let cfg = auto_switch();
        assert!(!cfg.enabled, "自动切换默认必须是关闭的");
    }

    #[test]
    fn foreground_match_ignores_case_and_exe_suffix() {
        // 只测归一化逻辑本身，不碰真实前台窗口。
        let want = "Game.EXE";
        let normalized = normalize_proc_name(want);
        assert_eq!(normalized, "game");
        // 归一化顺序：先小写再剥扩展名，否则 ".EXE" 剥不掉。
        assert_eq!(normalize_proc_name("  game.exe  "), "game");
    }

    #[test]
    fn decision_is_debuggable() {
        assert_eq!(Decision::None, Decision::None);
        assert_ne!(Decision::None, Decision::Switch("game".into()));
    }
}
