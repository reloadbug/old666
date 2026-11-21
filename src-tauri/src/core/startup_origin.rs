//! 启动项**归属**判定：系统自带 / 安装的程序 / 用户目录 / 位置待确认。
//!
//! ## 为什么按路径判，不按名字判
//!
//! 名字可以随意伪装 —— 第三方厂商把自家 DLL 塞进 `C:\Windows\System32\` 的
//! **子目录**（`System32\360DrvMgr\DrvMgrService.dll`）时，从名字看像是
//! Windows 自己的东西，实际是第三方程序。路径骗不了人，所以这里只认路径。
//!
//! ## 目录规则
//!
//! | 落在哪 | 判成 | 含义 |
//! |---|---|---|
//! | `%windir%\System32` / `SysWOW64` / `WinSxS` / `DriverStore` **直接**下的文件 | 系统自带 | Windows 自己的组件 |
//! | `%ProgramFiles%` / `%ProgramFiles(x86)%` / `%ProgramData%` | 安装的程序 | 装软件带进来的 |
//! | `%UserProfile%` / `%AppData%` / `%LocalAppData%` | 用户目录 | 多半是用户自己装的东西 |
//! | 其他（`C:\Tools`、`D:\Apps`…） | 位置待确认 | 说不清，程序不应自动改它 |
//!
//! ## 与 `startup_risk` 的分工
//!
//! `startup_risk` 管「禁用它会怎样」（能不能动），这里管「这是谁的东西」
//! （属于谁）。两者互不替代：系统自带不等于不能动（`SysMain`、`DiagTrack`
//! 都是系统自带的，但可以禁用）；第三方也不等于都能动（安全软件、反作弊必须留）。
//!
//! **用户目录默认不自动禁用** —— 用户自己装的东西多半是他想用的，
//! 程序不该替他决定。

use crate::core::types::StartupOrigin;

/// 判定一个可执行文件路径的归属。
///
/// 传入的应当是**展开过环境变量的绝对路径**（`%ProgramFiles%` 已替换成实际路径）。
/// 路径为空或无法判断时返回 `Unknown`。
pub fn classify(path: &str) -> StartupOrigin {
    let p = path.trim();
    if p.is_empty() {
        return StartupOrigin::Unknown;
    }
    let lower = p.replace('/', "\\").to_ascii_lowercase();

    // 系统自带：直接落在系统目录下的文件（不能有子目录）
    if is_system_dir(&lower) {
        return StartupOrigin::System;
    }

    // 用户目录
    if let Some(profile) = user_profile_lower() {
        if lower.starts_with(&profile) {
            return StartupOrigin::User;
        }
    }
    if let Some(appdata) = env_path_lower("APPDATA") {
        if lower.starts_with(&appdata) {
            return StartupOrigin::User;
        }
    }
    if let Some(local) = env_path_lower("LOCALAPPDATA") {
        if lower.starts_with(&local) {
            return StartupOrigin::User;
        }
    }

    // 安装目录
    for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432", "ProgramData"] {
        if let Some(base) = env_path_lower(var) {
            if lower.starts_with(&base) {
                return StartupOrigin::ThirdParty;
            }
        }
    }

    StartupOrigin::Unknown
}

/// 路径是否**直接**位于系统目录（`System32` / `SysWOW64` / `WinSxS` / `DriverStore`）下。
///
/// 「直接」是关键：`System32\360DrvMgr\DrvMgrService.dll` 是第三方厂商塞进
/// 系统目录的子目录，不能算系统自带。
fn is_system_dir(lower: &str) -> bool {
    let Some(windir) = env_path_lower("SystemRoot").or_else(|| env_path_lower("WINDIR")) else {
        return false;
    };
    if !lower.starts_with(&windir) {
        return false;
    }
    let tail = &lower[windir.len()..];
    let tail = tail.trim_start_matches('\\');
    for dir in ["system32\\", "syswow64\\", "winsxs\\", "driverstore\\"] {
        if let Some(rest) = tail.strip_prefix(dir) {
            if !rest.is_empty() && !rest.contains('\\') {
                return true;
            }
        }
    }
    false
}

/// 读一个环境变量并归一成小写、带尾部反斜杠的路径前缀。
fn env_path_lower(var: &str) -> Option<String> {
    let v = std::env::var(var).ok()?;
    if v.trim().is_empty() {
        return None;
    }
    let mut s = v.replace('/', "\\").trim_end_matches('\\').to_ascii_lowercase();
    s.push('\\');
    Some(s)
}

fn user_profile_lower() -> Option<String> {
    env_path_lower("USERPROFILE").or_else(|| {
        std::env::var("HOMEDRIVE")
            .ok()
            .zip(std::env::var("HOMEPATH").ok())
            .map(|(d, p)| format!("{}{}", d, p).replace('/', "\\"))
            .map(|s| {
                let mut l = s.trim_end_matches('\\').to_ascii_lowercase();
                l.push('\\');
                l
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_path_is_unknown() {
        assert_eq!(classify(""), StartupOrigin::Unknown);
        assert_eq!(classify("   "), StartupOrigin::Unknown);
    }

    #[test]
    fn program_files_is_third_party() {
        let base = std::env::var("ProgramFiles").unwrap_or(r"C:\Program Files".into());
        let p = format!(r"{}\Steam\steam.exe", base);
        assert_eq!(classify(&p), StartupOrigin::ThirdParty);
    }

    #[test]
    fn user_profile_is_user_owned() {
        let base = std::env::var("USERPROFILE").unwrap_or(r"C:\Users\Default".into());
        let p = format!(r"{}\MyTool\tool.exe", base);
        assert_eq!(classify(&p), StartupOrigin::User);
    }

    #[test]
    fn system32_root_file_is_system() {
        let windir = std::env::var("SystemRoot").unwrap_or(r"C:\Windows".into());
        let p = format!(r"{}\System32\lsass.exe", windir);
        assert_eq!(classify(&p), StartupOrigin::System);
    }

    #[test]
    fn system32_subdirectory_is_not_system() {
        // 第三方厂商把自家模块塞进 System32 的子目录 —— 名字像系统，实际是第三方
        let windir = std::env::var("SystemRoot").unwrap_or(r"C:\Windows".into());
        let p = format!(r"{}\System32\360DrvMgr\DrvMgrService.dll", windir);
        assert_ne!(classify(&p), StartupOrigin::System);
    }

    #[test]
    fn unknown_location_is_unknown() {
        assert_eq!(classify(r"D:\Tools\thing.exe"), StartupOrigin::Unknown);
    }
}
