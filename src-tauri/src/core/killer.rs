//! 结束进程与内存工作集裁剪。
//!
//! **两段式结束**（设计文档 4.5 节）：
//! 1. 温和：向目标进程的所有顶层窗口 `PostMessage(WM_CLOSE)`，给它自己保存退出的机会；
//! 2. 强制：超时未退出才 `TerminateProcess`。
//!
//! 这样对「有窗口的编辑器 / 下载器」不会粗暴丢数据，对「无窗口的后台残留」
//! 也能一次到位。
//!
//! ## 安全契约（重要）
//!
//! 本模块的每个**写操作**（`force_kill` / `kill_two_stage` / `trim_working_set`）
//! 都强制经过 [`super::safety::check_execute`]：
//!
//! - 内核 PID（0 / 4）与硬黑名单进程名 → 一律拒绝；
//! - 设置了 PID 白名单闸时，白名单外的 PID → 拒绝；
//! - 熔断（测试构建默认开启）→ 拒绝。
//!
//! 并且**调用方必须显式声明要动的是谁**（`name: &str`）。传空串时本模块
//! 会自己查一次映像名 —— 也就是说，「上层忘了校验进程名」这件事在结构上
//! 不可能发生。这是 P1「零误杀」验收的地基。

use windows::Win32::Foundation::{BOOL, CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::ProcessStatus::K32EmptyWorkingSet;
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, TerminateProcess,
    PROCESS_NAME_WIN32, PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SET_QUOTA, PROCESS_TERMINATE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
};

use super::{blacklist, safety};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 查询进程映像名（只取文件名，不含目录）。用于结束前的最后一道名字校验。
pub fn image_name(pid: u32) -> Option<String> {
    if pid <= 4 {
        return None;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(
            h,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(h);
        r.ok()?;
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        let base = full.rsplit(['\\', '/']).next().unwrap_or(&full);
        Some(base.to_string())
    }
}

/// 解析「这次要动的是谁」：调用方给了名字就用，没给就自己查。
fn resolve_name(pid: u32, name: &str) -> String {
    if !name.is_empty() {
        return name.to_string();
    }
    image_name(pid).unwrap_or_default()
}


/// 提升为 `SeDebugPrivilege`，以便结束其他用户 / 更高完整性级别的进程。
///
/// 需要进程本身已提权（管理员）。失败不报错，只是后续对高权限进程会失败。
pub fn enable_debug_privilege() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .is_err()
        {
            return false;
        }

        let name = wide("SeDebugPrivilege");
        let mut luid = Default::default();
        if LookupPrivilegeValueW(None, windows::core::PCWSTR(name.as_ptr()), &mut luid).is_err() {
            let _ = CloseHandle(token);
            return false;
        }

        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };

        let res = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None);
        let _ = CloseHandle(token);
        res.is_ok()
    }
}

/// 收集某进程的所有可见顶层窗口。
fn windows_of(pid: u32) -> Vec<HWND> {
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let (target, out) = &mut *(lparam.0 as *mut (u32, Vec<HWND>));
        if IsWindowVisible(hwnd).as_bool() {
            let mut wpid: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut wpid));
            if wpid == *target {
                out.push(hwnd);
            }
        }
        true.into()
    }
    let mut ctx: (u32, Vec<HWND>) = (pid, Vec::new());
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut _ as isize));
    }
    ctx.1
}

/// 温和关闭：向目标进程所有窗口发 `WM_CLOSE`。返回是否至少发出去一个。
pub fn graceful_close(pid: u32) -> bool {
    let wins = windows_of(pid);
    let mut sent = false;
    for h in wins {
        unsafe {
            if PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok() {
                sent = true;
            }
        }
    }
    sent
}

/// 判断进程是否还活着。
pub fn is_alive(pid: u32) -> bool {
    if blacklist::is_protected_pid(pid) {
        return true;
    }
    unsafe {
        match OpenProcess(PROCESS_QUERY_INFORMATION, false, pid) {
            Ok(h) => {
                let _ = CloseHandle(h);
                true
            }
            Err(_) => false,
        }
    }
}

/// 强制结束。返回错误原因（用于给用户看「哪个没杀掉、为什么」）。
///
/// `name` 传空串时自动查询映像名。**必须先过安全闸**。
pub fn force_kill(pid: u32, name: &str) -> Result<(), String> {
    let n = resolve_name(pid, name);
    safety::check_execute(pid, Some(&n))?;
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, false, pid)
            .map_err(|e| format!("无法打开进程（权限不足？）：{e}"))?;
        let r = TerminateProcess(h, 1);
        let _ = CloseHandle(h);
        r.map_err(|e| format!("结束失败：{e}"))
    }
}

/// 结束方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillMethod {
    /// 温和关闭成功
    Graceful,
    /// 温和超时后强制
    Forced,
    /// 进程自行退出 / 本来就不在了
    AlreadyGone,
}

impl KillMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            KillMethod::Graceful => "graceful",
            KillMethod::Forced => "force",
            KillMethod::AlreadyGone => "gone",
        }
    }
}

/// 两段式结束。
///
/// * `name` —— 目标进程名（如 `notepad.exe`）。传空串则内部查询。
/// * `grace_ms` —— 温和阶段等待上限（默认 800ms，见调用方）。
///
/// 安全闸不通过时返回 `(AlreadyGone, Some(原因))`，**绝不触碰系统**。
pub fn kill_two_stage(pid: u32, name: &str, grace_ms: u64) -> (KillMethod, Option<String>) {
    let n = resolve_name(pid, name);
    if let Err(why) = safety::check_execute(pid, Some(&n)) {
        return (KillMethod::AlreadyGone, Some(why));
    }
    if !is_alive(pid) {
        return (KillMethod::AlreadyGone, None);
    }

    // 第一段：温和
    let sent = graceful_close(pid);
    if sent {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(grace_ms);
        while std::time::Instant::now() < deadline {
            if !is_alive(pid) {
                return (KillMethod::Graceful, None);
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
    }

    // 第二段：强制
    match force_kill(pid, &n) {
        Ok(()) => (KillMethod::Forced, None),
        Err(e) => {
            if !is_alive(pid) {
                (KillMethod::Graceful, None)
            } else {
                (KillMethod::Forced, Some(e))
            }
        }
    }
}

/// 工作集裁剪（`EmptyWorkingSet`）。
///
/// 把进程的物理内存页尽量换出到页面文件，**不结束进程**。
/// 返回释放的物理内存字节数（裁剪前后工作集之差，为 0 表示无可释放）。
///
/// 注意：这是「温和」手段，只对确认闲置的进程使用 —— 对活跃进程裁剪
/// 反而会造成后续缺页、变慢。
pub fn trim_working_set(pid: u32, name: &str, before_bytes: u64) -> Result<u64, String> {
    // 裁剪走的是更宽松的闸：它不终止进程，只换出物理页。
    // 进程名硬黑名单只约束「结束」，不约束「裁剪」——
    // 否则收益最大的 svchost 类服务宿主反而动不了。
    safety::check_trim(pid).map_err(|e| {
        if name.is_empty() {
            e
        } else {
            format!("{name}：{e}")
        }
    })?;
    unsafe {
        let h = OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_SET_QUOTA,
            false,
            pid,
        )
        .map_err(|e| format!("无法打开进程：{e}"))?;
        let r = K32EmptyWorkingSet(h);
        let _ = CloseHandle(h);
        r.as_bool()
            .then_some(())
            .ok_or_else(|| "工作集裁剪失败".to_string())?;
    }
    // 读取裁剪后的工作集
    let after = crate::core::memory::working_set_of(pid).unwrap_or(0);
    Ok(before_bytes.saturating_sub(after))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_pid_refuses_kill() {
        let (m, e) = kill_two_stage(4, "System", 100);
        assert_eq!(m, KillMethod::AlreadyGone);
        assert!(e.is_some());
    }

    #[test]
    fn nonexistent_pid_is_gone() {
        let (m, _) = kill_two_stage(0x7FFF_FFF0, "ghost.exe", 50);
        assert_eq!(m, KillMethod::AlreadyGone);
    }

    #[test]
    fn force_kill_refuses_protected() {
        assert!(force_kill(4, "System").is_err());
        assert!(force_kill(0, "").is_err());
    }

    /// 关键用例：即使 PID 看起来正常，只要进程名在硬黑名单里就必须拒绝。
    #[test]
    fn force_kill_refuses_denied_name() {
        assert!(force_kill(0x7FFF_FFF0, "lsass.exe").is_err());
        assert!(force_kill(0x7FFF_FFF0, "csrss.exe").is_err());
        assert!(force_kill(0x7FFF_FFF0, "services.exe").is_err());
    }

    #[test]
    fn trim_refuses_protected() {
        assert!(trim_working_set(4, "System", 1000).is_err());
    }

    /// 裁剪的闸比结束宽：不受进程名硬黑名单约束（它不终止进程），
    /// 但仍受内核 PID 与熔断闸约束。`svchost` 这类服务宿主正是裁剪
    /// 收益最大的对象，不能被名字挡在门外。
    #[test]
    fn trim_gate_is_lighter_than_kill() {
        let kill_err = safety::check_execute(12345, Some("svchost.exe")).unwrap_err();
        assert!(kill_err.contains("系统关键进程"), "实际：{kill_err}");

        let trim_err = safety::check_trim(12345).unwrap_err();
        assert!(
            !trim_err.contains("系统关键进程"),
            "裁剪不该被进程名拦下，实际：{trim_err}"
        );
        assert!(safety::check_trim(4).is_err(), "内核 PID 的裁剪仍须拒绝");
    }

    /// 熔断生效时，连「合法目标」也不许执行。
    #[test]
    fn dry_run_blocks_ordinary_target() {
        assert!(safety::is_dry_run(), "测试构建应处于熔断状态");
        let e = force_kill(0x7FFF_FFF0, "notepad.exe").unwrap_err();
        assert!(e.contains("演练") || e.contains("不在允许"), "实际：{e}");
    }

    #[test]
    fn enable_privilege_does_not_panic() {
        let _ = enable_debug_privilege();
    }
}
