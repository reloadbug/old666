//! 内存相关：单进程工作集查询 + 系统内存总量。
//!
//! 采集主路径走 `NtQuerySystemInformation`（一次拿全），本模块只用于
//! **裁剪后复测** 这类「单点补查」场景，避免为了一个数字再枚举全部进程。

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};

/// 查询单进程当前工作集（物理内存）字节数。
pub fn working_set_of(pid: u32) -> Option<u64> {
    if pid <= 4 {
        return None;
    }
    unsafe {
        let h = OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
            false,
            pid,
        )
        .ok()?;
        let mut c = PROCESS_MEMORY_COUNTERS {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        let r = GetProcessMemoryInfo(h, &mut c, c.cb);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(c.WorkingSetSize as u64)
    }
}

/// 系统物理内存 `(总量, 可用量)`，单位字节。
pub fn system_memory() -> (u64, u64) {
    unsafe {
        let mut ms = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        if GlobalMemoryStatusEx(&mut ms).is_ok() {
            (ms.ullTotalPhys, ms.ullAvailPhys)
        } else {
            (0, 0)
        }
    }
}

/// 内存占用百分比。
pub fn memory_percent() -> f32 {
    let (total, avail) = system_memory();
    if total == 0 {
        return 0.0;
    }
    ((total - avail) as f64 / total as f64 * 100.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_working_set_positive() {
        let me = std::process::id();
        let ws = working_set_of(me);
        assert!(ws.is_some(), "应能查到自身工作集");
        assert!(ws.unwrap() > 0);
    }

    #[test]
    fn system_memory_sane() {
        let (total, avail) = system_memory();
        assert!(total > 0);
        assert!(avail <= total);
    }

    #[test]
    fn protected_pid_returns_none() {
        assert!(working_set_of(0).is_none());
        assert!(working_set_of(4).is_none());
    }
}
