//! 核心服务层 —— 老六清净的全部系统能力都在这里，与 UI 完全解耦。
//!
//! 分层（对应设计文档五层架构的第 3 层）：
//! ```text
//! types      共享数据契约
//! blacklist  安全红线：系统关键进程/服务/驱动硬黑名单
//! process    NtQuerySystemInformation 进程枚举 + 采样器
//! perf       PDH 性能计数器（CPU/磁盘/网络）
//! memory     GlobalMemoryStatusEx / GetProcessMemoryInfo
//! signature  WinVerifyTrust 数字签名校验
//! idle       六维闲置判定引擎（核心智力）
//! safety     全局安全闸（熔断 / PID 白名单 / 红线校验）
//! killer     两段式结束 + EmptyWorkingSet 裁剪
//! store      白名单与快照持久化（绿色便携，不写注册表）
//! purge      净化主流程编排
//! boot       开机耗时归因（读官方 Diagnostics-Performance 日志）
//! startup    启动项枚举（注册表 / 启动文件夹 / 服务 / 计划任务）
//! startup_risk 启动项风险分级规则库
//! startup_ctl  启动项禁用 / 延迟 / 删除 与还原执行器
//! startup_lists 启动项白名单 / 黑名单
//! knowledge  启动项知识库（内置 140+ 条 + 用户可编辑补充）
//! ```

pub mod blacklist;
pub mod boot;
pub mod idle;
pub mod killer;
pub mod knowledge;
pub mod memory;
pub mod perf;
pub mod process;
pub mod purge;
pub mod safety;
pub mod signature;
pub mod startup;
pub mod startup_ctl;
pub mod startup_lists;
pub mod startup_risk;
pub mod store;
pub mod types;

pub use purge::PurgeEngine;
pub use types::*;

/// 当前进程是否以管理员（提升）权限运行。
///
/// P1 的「结束进程」链路对高完整性级别进程需要提权，
/// UI 需要据此提示用户「以管理员身份重开可净化更多进程」。
pub fn is_elevated() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = windows::Win32::Foundation::HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut ret_len: u32 = 0;
        let r = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut std::ffi::c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        );
        let _ = CloseHandle(token);
        r.is_ok() && elevation.TokenIsElevated != 0
    }
}

/// 请求 UAC 提权（重启自身）。
///
/// 返回 `Ok(())` 表示已成功拉起提权实例（当前进程应随即退出）。
/// 用户点了「否」时返回 `Err`，UI 据此保持原状而不是假装成功。
pub fn relaunch_elevated() -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let exe = std::env::current_exe().map_err(|e| format!("取自身路径失败：{e}"))?;
    let mut w: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();

    let verb: Vec<u16> = "runas\0".encode_utf16().collect();
    let r = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW 返回值 > 32 表示成功
    if r.0 as usize > 32 {
        Ok(())
    } else {
        w.clear();
        Err("提权被取消或失败".into())
    }
}
