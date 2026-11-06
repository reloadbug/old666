//! 更新检查（P6 · 自动更新机制，**可关闭，默认关闭**）。
//!
//! 硬约束（都来自「绿色便携 + 用户数据零外传」）：
//! 1. **默认关闭**。用户不主动打开开关，就一次网络请求都不发。
//! 2. **只读、只提示**。检查更新只是拉一个版本清单做对比，
//!    **绝不自动下载、绝不自动安装** —— 更新永远由用户自己决定。
//! 3. 请求里**不带任何用户标识**：只有固定的 URL，不带机器信息、不带
//!    用户名、不带任何参数。
//! 4. 走 WinHTTP 的系统 TLS 栈，不引入第三方 HTTP 依赖。

use std::ffi::c_void;

use serde::{Deserialize, Serialize};
use windows::core::PCWSTR;
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, WINHTTP_FLAG_SECURE,
};

use super::store::data_dir;

/// 默认更新源：官方仓库里的版本清单。
pub const DEFAULT_UPDATE_URL: &str =
    "https://raw.githubusercontent.com/reloadbug/old666/main/update.json";

/// 请求超时（毫秒）。全都很短 —— 更新检查不该让程序卡住。
const TIMEOUT_MS: i32 = 4000;

/// 更新清单（远端 JSON）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Manifest {
    /// 最新版本号，如 "0.1.4"。
    pub version: String,
    /// 下载地址（**只显示，不自动下载**）。
    #[serde(default)]
    pub download_url: String,
    /// 更新说明（纯文本，可多行）。
    #[serde(default)]
    pub notes: String,
    /// 是否是预发布版。
    #[serde(default)]
    pub prerelease: bool,
}

/// 更新设置（存 `data/update.json`）。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UpdateConfig {
    /// 是否自动检查更新。**默认 false**。
    pub enabled: bool,
    /// 更新源地址。空 = 用默认地址。
    #[serde(default)]
    pub url: String,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
        }
    }
}

/// 检查结果（给 UI）。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UpdateStatus {
    /// 当前运行版本。
    pub current: String,
    /// 远端最新版本（拿不到就是空）。
    pub latest: String,
    /// 是否有新版本。
    pub has_update: bool,
    /// 远端下载地址（只显示，不自动下载）。
    pub download_url: String,
    /// 更新说明。
    pub notes: String,
    /// 检查失败时的原因（成功则空）。
    pub error: String,
    /// 是否被「关闭开关」挡下了 —— 这是默认路径。
    pub skipped: bool,
}

impl UpdateStatus {
    fn skipped_now(current: String) -> Self {
        Self {
            current,
            latest: String::new(),
            has_update: false,
            download_url: String::new(),
            notes: String::new(),
            error: String::new(),
            skipped: true,
        }
    }
}

fn config_path() -> std::path::PathBuf {
    data_dir().join("update.json")
}

/// 读更新设置。
pub fn config() -> UpdateConfig {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 写更新设置。
pub fn save_config(cfg: &UpdateConfig) -> Result<(), String> {
    let p = config_path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    let s = serde_json::to_string_pretty(cfg).map_err(|e| format!("序列化失败：{e}"))?;
    std::fs::write(&p, s).map_err(|e| format!("写入失败：{e}"))
}

/// 实际生效的更新源地址。
pub fn effective_url(cfg: &UpdateConfig) -> String {
    let u = cfg.url.trim();
    if u.is_empty() {
        DEFAULT_UPDATE_URL.to_string()
    } else {
        u.to_string()
    }
}

/// 比较版本号（点分数字）。返回 true 表示 `a` 比 `b` 新。
///
/// 只比数字段：`0.1.10` > `0.1.9`（按字符串比会错）。
pub fn is_newer(a: &str, b: &str) -> bool {
    let pa: Vec<u64> = a
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<u64>().unwrap_or(0))
        .collect();
    let pb: Vec<u64> = b
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<u64>().unwrap_or(0))
        .collect();
    let n = pa.len().max(pb.len());
    for i in 0..n {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// 执行一次检查。
///
/// * 开关关闭 → 直接返回 `skipped`，**一个字节都不发出去**。
/// * 开关打开 → 拉一次清单，比对版本号。
pub fn check(cfg: &UpdateConfig) -> UpdateStatus {
    let current = env!("CARGO_PKG_VERSION").to_string();
    if !cfg.enabled {
        return UpdateStatus::skipped_now(current);
    }
    let url = effective_url(cfg);
    match fetch_manifest(&url) {
        Ok(m) => {
            let has_update = is_newer(&m.version, &current);
            UpdateStatus {
                current,
                latest: m.version,
                has_update,
                download_url: m.download_url,
                notes: m.notes,
                error: String::new(),
                skipped: false,
            }
        }
        Err(e) => UpdateStatus {
            current,
            latest: String::new(),
            has_update: false,
            download_url: String::new(),
            notes: String::new(),
            error: e,
            skipped: false,
        },
    }
}

/// 拉取并解析远端清单（只读 GET）。
fn fetch_manifest(url: &str) -> Result<Manifest, String> {
    let body = http_get(url)?;
    serde_json::from_str(&body).map_err(|e| format!("清单格式不对：{e}"))
}

/// 极简 HTTPS GET（WinHTTP）。
fn http_get(url: &str) -> Result<String, String> {
    let (host, path) = split_url(url)?;
    let w_host = wide(&host);
    let w_path = wide(&path);
    let w_agent = wide("old666-updater");
    let w_get = wide("GET");

    unsafe {
        let session = WinHttpOpen(
            PCWSTR(w_agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if session.is_null() {
            return Err("打开网络会话失败".into());
        }

        let _ = WinHttpSetTimeouts(
            session,
            TIMEOUT_MS,
            TIMEOUT_MS,
            TIMEOUT_MS,
            TIMEOUT_MS,
        );

        let connect = WinHttpConnect(session, PCWSTR(w_host.as_ptr()), 443, 0);
        if connect.is_null() {
            let _ = WinHttpCloseHandle(session);
            return Err("连接服务器失败".into());
        }

        let request = WinHttpOpenRequest(
            connect,
            PCWSTR(w_get.as_ptr()),
            PCWSTR(w_path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if request.is_null() {
            let _ = WinHttpCloseHandle(connect);
            let _ = WinHttpCloseHandle(session);
            return Err("发起请求失败".into());
        }

        let result = (|| -> Result<String, String> {
            WinHttpSendRequest(request, None, None, 0, 0, 0)
                .map_err(|e| format!("发送失败：{e}"))?;
            WinHttpReceiveResponse(request, std::ptr::null_mut())
                .map_err(|e| format!("接收失败：{e}"))?;

            let mut out: Vec<u8> = Vec::new();
            loop {
                let mut avail: u32 = 0;
                WinHttpQueryDataAvailable(request, &mut avail)
                    .map_err(|e| format!("查询数据长度失败：{e}"))?;
                if avail == 0 {
                    break;
                }
                let mut buf = vec![0u8; avail as usize];
                let mut read: u32 = 0;
                if WinHttpReadData(request, buf.as_mut_ptr() as *mut c_void, avail, &mut read).is_err() {
                    break;
                }
                if read == 0 {
                    break;
                }
                buf.truncate(read as usize);
                out.extend_from_slice(&buf);
                if out.len() > 512 * 1024 {
                    break; // 清单不可能这么大，防御性上限
                }
            }
            String::from_utf8(out).map_err(|e| format!("响应不是合法文本：{e}"))
        })();

        let _ = WinHttpCloseHandle(request);
        let _ = WinHttpCloseHandle(connect);
        let _ = WinHttpCloseHandle(session);
        result
    }
}

/// 拆 URL：`https://host/path` → (`host`, `/path`)。
fn split_url(url: &str) -> Result<(String, String), String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| "只支持 https 地址".to_string())?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if host.is_empty() {
        return Err("地址里没有主机名".into());
    }
    Ok((host.to_string(), path.to_string()))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_disabled() {
        let c = UpdateConfig::default();
        assert!(!c.enabled, "自动检查更新必须默认关闭");
        assert_eq!(effective_url(&c), DEFAULT_UPDATE_URL);
    }

    #[test]
    fn version_compare_is_numeric_not_lexicographic() {
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.2.0", "0.1.99"));
        assert!(!is_newer("0.1.3", "0.1.3"));
        assert!(!is_newer("0.1.3", "0.1.4"));
        assert!(!is_newer("0.1.3", "1.0.0"));
    }

    #[test]
    fn disabled_config_never_hits_the_network() {
        let st = check(&UpdateConfig::default());
        assert!(st.skipped, "关闭时必须直接跳过");
        assert!(!st.has_update);
        assert!(st.error.is_empty());
    }

    #[test]
    fn url_splitting_handles_paths() {
        let (h, p) = split_url("https://example.com/a/b.json").unwrap();
        assert_eq!(h, "example.com");
        assert_eq!(p, "/a/b.json");

        let (h, p) = split_url("https://example.com").unwrap();
        assert_eq!(h, "example.com");
        assert_eq!(p, "/");
    }

    #[test]
    fn non_https_url_is_rejected() {
        assert!(split_url("http://example.com").is_err());
        assert!(split_url("ftp://example.com").is_err());
    }

    #[test]
    fn custom_url_overrides_default() {
        let c = UpdateConfig {
            enabled: true,
            url: "https://example.com/u.json".into(),
        };
        assert_eq!(effective_url(&c), "https://example.com/u.json");
    }

    #[test]
    fn manifest_parses_minimal_json() {
        let m: Manifest = serde_json::from_str(r#"{"version":"0.1.4"}"#).unwrap();
        assert_eq!(m.version, "0.1.4");
        assert!(m.download_url.is_empty());
    }
}
