//! 全局安全闸 —— 保证「自测 / 演练」永远不会动到真实系统进程。
//!
//! 这一层存在的理由很具体：P1 的核心验收是「**200 次净化零误杀**」，
//! 而验证这个结论如果靠真的去杀进程，测试本身就成了风险源。
//! 所以把「判定」与「执行」彻底拆开：
//!
//! | 闸 | 作用 | 生效范围 |
//! |---|---|---|
//! | `check_target` | 内核 PID + 硬黑名单进程名 | 所有路径（含生产） |
//! | `kill_allowlist` | 只放行指定 PID（一般 = 自己拉起的测试进程） | 由测试/演练设置 |
//! | `dry_run` | 任何写操作直接拒绝，物理上不碰系统 | 测试构建默认开启 |
//!
//! `dry_run` 的默认值：`cfg!(test)` 为真时**默认开启** —— 也就是说
//! `cargo test` 跑出来的任何用例都不可能结束真实进程，这是编译期就定下的
//! 约束，不依赖写测试的人记得加保护。生产构建默认关闭；
//! 任何构建下都可以用环境变量 `OLD666_DRY_RUN=1` 强制开启（CI / 演示用）。

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use super::blacklist;

// ---------------------------------------------------------------------------
// 熔断开关
// ---------------------------------------------------------------------------

fn default_dry_run() -> bool {
    if cfg!(test) {
        return true;
    }
    matches!(
        std::env::var("OLD666_DRY_RUN").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes")
    )
}

static DRY_RUN: OnceLock<bool> = OnceLock::new();

/// 熔断是否生效。首次调用时确定，之后不再改变。
pub fn is_dry_run() -> bool {
    *DRY_RUN.get_or_init(default_dry_run)
}

/// 强制设置熔断（仅在尚未被读取过时有效）。
///
/// 之所以不让它在运行期随便翻转：一个正在跑净化任务的进程，
/// 不应该有任何代码路径能中途把安全闸关掉。
pub fn set_dry_run(on: bool) {
    let _ = DRY_RUN.set(on);
}

// ---------------------------------------------------------------------------
// PID 白名单闸
// ---------------------------------------------------------------------------

static ALLOWLIST: OnceLock<Mutex<Option<HashSet<u32>>>> = OnceLock::new();

fn allowlist() -> &'static Mutex<Option<HashSet<u32>>> {
    ALLOWLIST.get_or_init(|| Mutex::new(None))
}

/// 设置「只允许操作这些 PID」的闸门。传 `None` 表示解除限制。
pub fn set_kill_allowlist(pids: Option<HashSet<u32>>) {
    if let Ok(mut g) = allowlist().lock() {
        *g = pids;
    }
}

fn allowlisted(pid: u32) -> bool {
    match allowlist().lock() {
        Ok(g) => match &*g {
            None => true,
            Some(set) => set.contains(&pid),
        },
        // 锁中毒时保守放行（正常路径下不会发生），真正的兜底在 check_execute
        Err(_) => true,
    }
}

// ---------------------------------------------------------------------------
// 裁决
// ---------------------------------------------------------------------------

/// 目标是否触碰安全红线（内核进程 / 硬黑名单进程名）。
///
/// **生产路径也走这一关**，是「执行前二次校验」的最底层实现。
pub fn check_target(pid: u32, name: Option<&str>) -> Result<(), String> {
    if blacklist::is_protected_pid(pid) {
        return Err("受保护的内核进程".into());
    }
    if let Some(n) = name {
        if !n.is_empty() && blacklist::is_denied_name(n) {
            return Err(format!("「{n}」是系统关键进程，已拦截"));
        }
    }
    Ok(())
}

/// 能否真正执行写操作（结束 / 裁剪）。
///
/// 在 `check_target` 之上叠加白名单闸与熔断闸。`killer` 的每个写操作
/// 都必须先过这里 —— 这是「忘了在上层写校验」也不会出事的原因。
pub fn check_execute(pid: u32, name: Option<&str>) -> Result<(), String> {
    check_target(pid, name)?;
    if !allowlisted(pid) {
        return Err("安全闸：该进程不在允许操作范围内".into());
    }
    if is_dry_run() {
        return Err("演练模式：已阻止实际执行".into());
    }
    Ok(())
}

/// 能否执行**工作集裁剪**。
///
/// 比 [`check_execute`] 宽松：裁剪**不终止进程**，只把无人访问的物理页
/// 换出到页面文件，进程下次访问时系统会自动换回。这正是 Windows 自身
/// 在内存压力下的标准行为，因此不受「进程名硬黑名单」限制 ——
/// `svchost.exe` 这类服务宿主恰恰是裁剪收益最大的对象。
///
/// 但内核伪进程（PID ≤ 4）与熔断闸依然生效。
pub fn check_trim(pid: u32) -> Result<(), String> {
    if blacklist::is_protected_pid(pid) {
        return Err("受保护的内核进程".into());
    }
    if !allowlisted(pid) {
        return Err("安全闸：该进程不在允许操作范围内".into());
    }
    if is_dry_run() {
        return Err("演练模式：已阻止实际执行".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 启动项变更闸（P2）
// ---------------------------------------------------------------------------

/// 启动项变更的「只允许操作这些条目」闸门。
///
/// 与 PID 白名单闸同构：调用方必须先 `set_startup_allowlist(Some(ids))`
/// 声明本次要动哪几条，改完立刻 `set_startup_allowlist(None)` 撤销。
/// 这样即使 UI 状态与系统状态不同步，也不可能改到清单之外的东西。
static STARTUP_ALLOWLIST: OnceLock<Mutex<Option<HashSet<String>>>> = OnceLock::new();

fn startup_allowlist() -> &'static Mutex<Option<HashSet<String>>> {
    STARTUP_ALLOWLIST.get_or_init(|| Mutex::new(None))
}

/// 设置启动项变更白名单。传 `None` 表示解除限制。
pub fn set_startup_allowlist(ids: Option<HashSet<String>>) {
    if let Ok(mut g) = startup_allowlist().lock() {
        *g = ids;
    }
}

fn startup_allowed(id: &str) -> bool {
    match startup_allowlist().lock() {
        Ok(g) => match &*g {
            None => true,
            Some(set) => set.contains(id),
        },
        Err(_) => true,
    }
}

/// 能否对某个启动项执行变更。
///
/// 启动项变更**不终止任何进程**，因此不受进程黑名单约束；
/// 但它会改变系统下次开机行为，所以熔断闸与白名单闸一个都不能少。
pub fn check_startup_change(item_id: &str) -> Result<(), String> {
    if item_id.trim().is_empty() {
        return Err("启动项 ID 为空，拒绝执行".into());
    }
    if !startup_allowed(item_id) {
        return Err("安全闸：该启动项不在本次允许变更的清单内".into());
    }
    if is_dry_run() {
        return Err("演练模式：已阻止实际变更".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_is_dry_run_by_default() {
        // 这条断言是整个安全设计的基石：测试构建下熔断必须默认开启
        assert!(is_dry_run(), "cfg(test) 构建必须默认熔断");
    }

    #[test]
    fn kernel_pids_always_refused() {
        assert!(check_target(0, None).is_err());
        assert!(check_target(4, None).is_err());
        assert!(check_execute(4, None).is_err());
    }

    #[test]
    fn denied_name_refused() {
        for n in ["lsass.exe", "csrss.exe", "wininit.exe", "services.exe", "svchost.exe"] {
            assert!(check_target(12345, Some(n)).is_err(), "{n} 应被拦截");
        }
    }

    #[test]
    fn ordinary_process_passes_target_check() {
        assert!(check_target(12345, Some("notepad.exe")).is_ok());
    }

    #[test]
    fn execute_blocked_under_dry_run() {
        // 目标检查通过，但熔断生效 → 仍然拒绝执行
        assert!(check_execute(12345, Some("notepad.exe")).is_err());
    }

    #[test]
    fn startup_change_blocked_under_dry_run() {
        // 空 ID 直接拒绝，连白名单都不用查
        assert!(check_startup_change("").is_err());
        assert!(check_startup_change("   ").is_err());
        // 合法 ID 在测试构建下也必须被熔断挡住
        assert!(check_startup_change("0123456789abcdef").is_err());
    }

    #[test]
    fn startup_allowlist_restricts_ids() {
        let mut set = HashSet::new();
        set.insert("allowed_id".to_string());
        set_startup_allowlist(Some(set));
        // 白名单闸先于熔断闸生效，因此这里拿到的是「不在清单内」而不是「演练模式」
        let err = check_startup_change("other_id").unwrap_err();
        assert!(err.contains("不在本次允许变更的清单内"), "实际：{err}");
        set_startup_allowlist(None);
        let err = check_startup_change("other_id").unwrap_err();
        assert!(err.contains("演练模式"), "撤销白名单后应只剩熔断闸，实际：{err}");
    }
}
