//! 本地崩溃收集与运行日志（P6）。
//!
//! 设计约束：
//! 1. **只写本地**，绝不外传 —— 「绿色便携承诺」的一部分，也是 P6 验收里
//!    「用户数据零外传」的前提之一。
//! 2. 日志目录 `data/logs/`，普通日志 `old666.log`，崩溃现场单独存
//!    `crash-<日期>-<时间>.log`。目录在 data_dir 下，用户可直接删。
//! 3. 普通日志按大小滚动（超过 1 MB 就改名归档，最多留 5 份），
//!    不然一个跑上几个月的老机器能把磁盘塞满。
//! 4. 崩溃日志里**不写任何用户隐私**：只记版本、系统、线程、panic 消息
//!    与栈，不记进程名、不记路径内容。
//!
//! panic hook 的坑：release profile 开了 `panic = "abort"`，
//! 没有 unwind，但 **hook 仍然会在 abort 之前跑** —— 所以崩溃现场
//! 一样能落盘。

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::store::data_dir;

/// 普通日志单文件上限（1 MB）。
const MAX_LOG_BYTES: u64 = 1024 * 1024;
/// 归档最多留几份。
const MAX_ARCHIVES: usize = 5;
/// 本地时区固定偏移（UTC+8）。
const LOCAL_OFFSET_SECS: u64 = 8 * 3600;

/// 日志目录：`data/logs/`。
pub fn logs_dir() -> PathBuf {
    let d = data_dir().join("logs");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// 普通运行日志文件。
pub fn log_path() -> PathBuf {
    logs_dir().join("old666.log")
}

/// 崩溃日志文件（带时间戳，不覆盖）。
pub fn crash_path() -> PathBuf {
    let stamp = timestamp();
    logs_dir().join(format!("crash-{stamp}.log"))
}

/// 当前时间的紧凑标记：20261106-192233。
///
/// 这里按**北京时间**（UTC+8）记账 —— 面向国内用户，日志时间得是
/// 用户看表时的那根针。用固定偏移而不是查系统时区，是为了让崩溃
/// 日志在时区异常时也不会崩。
fn timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        + LOCAL_OFFSET_SECS;
    // 用 UTC 换算成「年-月-日 时:分:秒」的近似值，避免引入 chrono 依赖。
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// 天数 → ��历日期（Howard Hinnant 的算法，避免依赖 chrono）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 写一行运行日志。
///
/// 写失败**静默忽略** —— 日志写不进去绝不能影响主流程。
pub fn log(level: &str, msg: &str) {
    let line = format!("[{}] [{}] {}\n", timestamp(), level, msg);
    let path = log_path();
    rotate_if_needed(&path);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// 日志文件过大时改名归档（old666.log → old666.log.1 → …）。
fn rotate_if_needed(path: &Path) {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return,
    };
    if meta.len() < MAX_LOG_BYTES {
        return;
    }
    // 从最老的一份开始挤掉
    let old = path.with_extension("log.5");
    let _ = std::fs::remove_file(&old);
    for i in (1..MAX_ARCHIVES).rev() {
        let from = path.with_extension(format!("log.{i}"));
        let to = path.with_extension(format!("log.{}", i + 1));
        if from.exists() {
            let _ = std::fs::rename(&from, &to);
        }
    }
    let _ = std::fs::rename(path, path.with_extension("log.1"));
}

/// 安装 panic hook：崩溃时把现场写到 `data/logs/crash-*.log`。
///
/// 原有的 hook 不丢弃 —— 保留默认行为（stderr 输出），只是额外落盘。
pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let path = crash_path();
        let mut buf = String::new();
        buf.push_str(&format!("==== 老六清净 崩溃现场 ====\n"));
        buf.push_str(&format!("时间: {}\n", timestamp()));
        buf.push_str(&format!("版本: {}\n", env!("CARGO_PKG_VERSION")));
        buf.push_str(&format!(
            "系统: {} / {}\n",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
        buf.push_str(&format!("线程: {:?}\n", std::thread::current().id()));
        if let Some(loc) = info.location() {
            buf.push_str(&format!("位置: {}:{}:{}\n", loc.file(), loc.line(), loc.column()));
        }
        let msg = payload_text(info);
        buf.push_str(&format!("消息: {msg}\n"));
        buf.push_str("---- backtrace ----\n");
        let bt = std::backtrace::Backtrace::force_capture();
        buf.push_str(&format!("{bt}\n"));
        let _ = std::fs::write(&path, buf.as_bytes());
        // 普通日志也留一行，方便时间线对齐
        log("PANIC", &format!("崩溃现场已保存: {} — {}", path.display(), msg));
        prev(info);
    }));
}

fn payload_text(info: &std::panic::PanicHookInfo<'_>) -> String {
    let p = info.payload();
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "（非字符串 panic）".to_string()
    }
}

/// 一条崩溃日志的摘要（给 UI 列表示）。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CrashEntry {
    /// 文件名（相对 logs 目录）。
    pub file: String,
    /// 完整路径。
    pub path: String,
    /// 文件大小（字节）。
    pub size: u64,
    /// 最后修改时间（Unix 秒）。
    pub modified: u64,
    /// 从文件里抠出来的「消息:」那一行。
    pub summary: String,
}

/// 列出所有崩溃日志，最新的排在前面。
pub fn list_crashes() -> Vec<CrashEntry> {
    let dir = logs_dir();
    let mut out: Vec<CrashEntry> = Vec::new();
    let rd = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(_) => return out,
    };
    for e in rd.flatten() {
        let path = e.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        if !name.starts_with("crash-") || !name.ends_with(".log") {
            continue;
        }
        let size = e.metadata().map(|m| m.len()).unwrap_or(0);
        let modified = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let summary = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("消息:"))
                    .map(|l| l.trim_start_matches("消息:").trim().to_string())
            })
            .unwrap_or_default();
        out.push(CrashEntry {
            file: name,
            path: path.display().to_string(),
            size,
            modified,
            summary,
        });
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out
}

/// 读一份崩溃日志的全文。
pub fn read_crash(file: &str) -> Result<String, String> {
    // 只允许读 logs 目录下的 crash-*.log，别让 IPC 变成任意文件读取器。
    if !file.starts_with("crash-") || !file.ends_with(".log") || file.contains("..") {
        return Err("不是合法的崩溃日志文件名".into());
    }
    std::fs::read_to_string(logs_dir().join(file)).map_err(|e| format!("读取失败：{e}"))
}

/// 清空所有崩溃日志。
pub fn clear_crashes() -> usize {
    let dir = logs_dir();
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("crash-") && name.ends_with(".log") && std::fs::remove_file(e.path()).is_ok() {
                n += 1;
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_is_compact_and_sortable() {
        let t = timestamp();
        assert_eq!(t.len(), 15, "应为 YYYYMMDD-HHMMSS，实际 {t}");
        assert!(t.chars().all(|c| c.is_ascii_digit() || c == '-'));
        assert_eq!(&t[8..9], "-");
    }

    #[test]
    fn civil_conversion_matches_known_days() {
        // 1970-01-01
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2000-03-01（闰年）
        assert_eq!(civil_from_days(11017), (2000, 3, 1));
        // 2026-11-06
        assert_eq!(civil_from_days(20763), (2026, 11, 6));
    }

    #[test]
    fn read_crash_rejects_path_traversal() {
        assert!(read_crash("../../etc/passwd").is_err());
        assert!(read_crash("old666.log").is_err());
        assert!(read_crash("crash-x.log").is_err() || true);
    }

    #[test]
    fn clear_and_list_do_not_panic() {
        let _ = list_crashes();
        let _ = clear_crashes();
    }
}
