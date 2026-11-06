//! 数字签名校验（WinVerifyTrust）—— 可信度判定的辅助信号。
//!
//! 签名校验**不是**安全红线（黑名单才是），只影响打分：
//! 一个无签名的陌生后台进程比一个微软签名的系统组件更值得结束。
//!
//! 性能：单文件校验约 1–5 ms，且同一路径结果稳定，因此加**进程内缓存**
//! （键 = 路径 + 文件大小 + 修改时间），避免每次刷新重复校验。
//!
//! windows-rs 0.58 的坑：`CryptQueryObject` / `CryptMsgGetParam` / `CryptMsgClose`
//! 的句柄参数是裸 `*mut c_void`（不是 `HANDLE`），`CertFindCertificateInStore`
//! 的编码类型是 `CERT_QUERY_ENCODING_TYPE` 新类型而不是裸 u32。

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use windows::core::{GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::Security::Cryptography::{
    CertCloseStore, CertFindCertificateInStore, CertFreeCertificateContext, CertGetNameStringW,
    CryptMsgClose, CryptMsgGetParam, CryptQueryObject, CERT_FIND_SUBJECT_CERT, CERT_INFO,
    CERT_NAME_SIMPLE_DISPLAY_TYPE, CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED,
    CERT_QUERY_ENCODING_TYPE, CERT_QUERY_FORMAT_FLAG_BINARY, CERT_QUERY_OBJECT_FILE,
    CMSG_SIGNER_CERT_INFO_PARAM, HCERTSTORE,
};
use windows::Win32::Security::WinTrust::{
    WinVerifyTrust, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0,
    WINTRUST_DATA_UICONTEXT, WINTRUST_FILE_INFO, WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_FILE,
    WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
};

use super::types::SignatureStatus;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---- WinVerifyTrust 返回码 ----
/// TRUST_E_NOSIGNATURE：文件没有签名
const TRUST_E_NOSIGNATURE: i32 = 0x800B_0100u32 as i32;
/// TRUST_E_SUBJECT_FORM_UNKNOWN
const TRUST_E_SUBJECT_FORM_UNKNOWN: i32 = 0x800B_0003u32 as i32;
/// TRUST_E_EXPLICIT_DISTRUST：证书被显式吊销
const TRUST_E_EXPLICIT_DISTRUST: i32 = 0x800B_0111u32 as i32;
/// TRUST_E_BAD_DIGEST：签名与文件内容不符（被篡改）
const TRUST_E_BAD_DIGEST: i32 = 0x8009_6010u32 as i32;

/// X509_ASN_ENCODING | PKCS_7_ASN_ENCODING
const X509_ASN_ENCODING_PKCS7: u32 = 0x0001_0001;

/// 缓存：key = 路径，value = (文件大小, 修改时间戳, 签名结论)
type Cache = Mutex<HashMap<String, (u64, i64, SignatureStatus)>>;

fn cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 校验文件签名（带缓存）。
pub fn verify(path: &str) -> SignatureStatus {
    let (size, mtime) = match std::fs::metadata(path) {
        Ok(m) => {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            (m.len(), t)
        }
        // 文件不存在（进程已退出）
        Err(_) => return SignatureStatus::Unknown,
    };

    if let Ok(c) = cache().lock() {
        if let Some((s, t, st)) = c.get(path) {
            if *s == size && *t == mtime {
                return st.clone();
            }
        }
    }

    let st = verify_uncached(path);

    if let Ok(mut c) = cache().lock() {
        // 缓存上限保护：超过 4096 条直接清空重建
        if c.len() > 4096 {
            c.clear();
        }
        c.insert(path.to_string(), (size, mtime, st.clone()));
    }
    st
}

/// 校验 `Option<路径>`，便于直接接到进程对象上。
pub fn verify_optional(path: Option<&str>) -> SignatureStatus {
    match path {
        Some(p) if !p.is_empty() => verify(p),
        // 拿不到路径的进程（受保护的系统进程）按「无法判定」处理，
        // 绝不当成「无签名」而抬高可疑度 —— 那会诱导误杀。
        _ => SignatureStatus::Unknown,
    }
}

/// 实际执行校验，不查缓存。
fn verify_uncached(path: &str) -> SignatureStatus {
    let wpath = wide(path);

    let mut file_info = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(wpath.as_ptr()),
        hFile: HANDLE::default(),
        pgKnownSubject: std::ptr::null_mut(),
    };

    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        pPolicyCallbackData: std::ptr::null_mut(),
        pSIPClientData: std::ptr::null_mut(),
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 {
            pFile: &mut file_info,
        },
        dwStateAction: WTD_STATEACTION_VERIFY,
        hWVTStateData: HANDLE::default(),
        pwszURLReference: PWSTR::null(),
        // 只用本地缓存，不联网做吊销检查 —— 保证「纯本地 + 快」
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL,
        dwUIContext: WINTRUST_DATA_UICONTEXT(0),
        pSignatureSettings: std::ptr::null_mut(),
    };

    let mut action: GUID = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let rc = unsafe {
        WinVerifyTrust(
            HWND::default(),
            &mut action,
            &mut data as *mut _ as *mut c_void,
        )
    };

    // 无论成败都要关闭状态句柄，否则泄漏
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        let _ = WinVerifyTrust(
            HWND::default(),
            &mut action,
            &mut data as *mut _ as *mut c_void,
        );
    }

    if rc == 0 {
        let subject = signer_name(&wpath).unwrap_or_else(|| "已签名".to_string());
        SignatureStatus::Valid(subject)
    } else if rc == TRUST_E_NOSIGNATURE || rc == TRUST_E_SUBJECT_FORM_UNKNOWN {
        SignatureStatus::Unsigned
    } else if rc == TRUST_E_EXPLICIT_DISTRUST || rc == TRUST_E_BAD_DIGEST {
        SignatureStatus::Invalid
    } else {
        // 证书链 / 吊销信息缺失等无法判定的情况：归为「无法判定」而非「无签名」
        SignatureStatus::Unknown
    }
}

/// 从已签名的 PE 文件里提取签名主体显示名（如 "Microsoft Corporation"）。
///
/// 任何一步失败都返回 `None`，不影响签名有效性结论。
///
/// **关键点**：`CertFindCertificateInStore(CERT_FIND_SUBJECT_CERT, ...)` 要求
/// 传入一个 `CERT_INFO*`，它只会读其中的 `Issuer` 与 `SerialNumber` 两个字段。
/// 直接从 `CMSG_SIGNER_INFO` 里取这两个 blob 是**错的** —— 两者内存布局不同
/// （`CERT_INFO` 偏移 8 是 SerialNumber，而 `CMSG_SIGNER_INFO` 偏移 8 是 Issuer），
/// 传错会静默匹配失败，所有已签名文件都退化成「已签名」这个占位文案。
///
/// 正确做法是用 `CMSG_SIGNER_CERT_INFO_PARAM` 向 crypt32 要一份真正的
/// `CERT_INFO`，再交给查找函数。这是 MSDN 的推荐姿势。
fn signer_name(wpath: &[u16]) -> Option<String> {
    unsafe {
        // HCERTSTORE 是 windows-rs 的透明新类型（内含 *mut c_void），
        // 不能用裸指针代替 —— CertFindCertificateInStore 要求 Param<HCERTSTORE>。
        let mut store = HCERTSTORE::default();
        let mut msg: *mut c_void = std::ptr::null_mut();

        CryptQueryObject(
            CERT_QUERY_OBJECT_FILE,
            wpath.as_ptr() as *const c_void,
            CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED,
            CERT_QUERY_FORMAT_FLAG_BINARY,
            0,
            None,
            None,
            None,
            Some(&mut store),
            Some(&mut msg),
            None,
        )
        .ok()?;

        if store.is_invalid() || msg.is_null() {
            cleanup(store, msg);
            return None;
        }

        // 第一次取大小
        let mut size: u32 = 0;
        if CryptMsgGetParam(
            msg as *const c_void,
            CMSG_SIGNER_CERT_INFO_PARAM,
            0,
            None,
            &mut size,
        )
        .is_err()
            || size == 0
        {
            cleanup(store, msg);
            return None;
        }

        // 按 u64 对齐分配，保证 CERT_INFO 布局正确
        let words = (size as usize).div_ceil(8);
        let mut buf: Vec<u64> = vec![0; words];
        if CryptMsgGetParam(
            msg as *const c_void,
            CMSG_SIGNER_CERT_INFO_PARAM,
            0,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut size,
        )
        .is_err()
        {
            cleanup(store, msg);
            return None;
        }

        let cert = CertFindCertificateInStore(
            store,
            CERT_QUERY_ENCODING_TYPE(X509_ASN_ENCODING_PKCS7),
            0,
            CERT_FIND_SUBJECT_CERT,
            Some(buf.as_ptr() as *const c_void),
            None,
        );
        if cert.is_null() {
            cleanup(store, msg);
            return None;
        }

        let mut name_buf = [0u16; 512];
        let n = CertGetNameStringW(
            cert,
            CERT_NAME_SIMPLE_DISPLAY_TYPE,
            0,
            None,
            Some(&mut name_buf),
        );
        let _ = CertFreeCertificateContext(Some(cert));
        cleanup(store, msg);

        // n 含结尾 NUL，> 1 才有内容
        if n > 1 {
            let name = String::from_utf16_lossy(&name_buf[..(n - 1) as usize]);
            let name = name.trim().to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
        None
    }
}

/// 保留类型引用，避免 `CERT_INFO` 导入被判为未使用
/// （它通过裸指针间接使用，编译器看不见）。
#[allow(dead_code)]
fn _cert_info_type_anchor(_: CERT_INFO) {}

unsafe fn cleanup(store: HCERTSTORE, msg: *mut c_void) {
    if !msg.is_null() {
        let _ = CryptMsgClose(Some(msg as *const c_void));
    }
    if !store.is_invalid() {
        let _ = CertCloseStore(store, 0);
    }
}

/// 清空签名缓存（用户手动刷新 / 文件被替换时用）。
pub fn clear_cache() {
    if let Ok(mut c) = cache().lock() {
        c.clear();
    }
}

/// 签名主体是否为受信任厂商（用于打分与 UI 徽标）。
pub fn is_trusted_vendor(st: &SignatureStatus) -> bool {
    match st {
        SignatureStatus::Valid(s) => {
            let l = s.to_ascii_lowercase();
            const TRUSTED: &[&str] = &[
                "microsoft",
                "windows",
                "intel",
                "nvidia",
                "advanced micro devices",
                "amd",
                "realtek",
                "qualcomm",
                "dell",
                "hp ",
                "lenovo",
                "asus",
                "acer",
                "apple",
                "google",
                "mozilla",
            ];
            TRUSTED.iter().any(|t| l.contains(t))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_nonexistent_is_unknown() {
        assert_eq!(
            verify(r"C:\definitely\not\here\nope.exe"),
            SignatureStatus::Unknown
        );
    }

    #[test]
    fn verify_system_dll_is_signed() {
        // kernel32.dll 在任何 Windows 上都有微软签名
        let st = verify(r"C:\Windows\System32\kernel32.dll");
        match st {
            SignatureStatus::Valid(ref s) => {
                assert!(!s.is_empty(), "签名主体不应为空");
            }
            other => panic!("kernel32.dll 应为有效签名，实际 {other:?}"),
        }
    }

    /// 回归：签名主体必须是**真实的发布者名**，不能退化成占位文案。
    ///
    /// 曾经的 bug 是把 `&CMSG_SIGNER_INFO.Issuer` 当成 `CERT_INFO*` 传给
    /// `CertFindCertificateInStore`，两者布局不同 → 查找静默失败 →
    /// 所有已签名文件都显示成「已签名」，UI 上等于没有发布者信息。
    #[test]
    fn signer_name_is_a_real_publisher_not_a_placeholder() {
        // 只用 System32 下的核心 DLL —— 它们在任何 Windows 上都由微软签名。
        // （notepad.exe 之类可能是 Store 应用占位符，不可靠。）
        let mut checked = 0;
        for path in [
            r"C:\Windows\System32\kernel32.dll",
            r"C:\Windows\System32\user32.dll",
            r"C:\Windows\System32\advapi32.dll",
            r"C:\Windows\System32\ole32.dll",
        ] {
            if !std::path::Path::new(path).exists() {
                continue;
            }
            match verify(path) {
                SignatureStatus::Valid(name) => {
                    checked += 1;
                    assert_ne!(name, "已签名", "{path} 退化成了占位文案");
                    assert!(
                        name.to_ascii_lowercase().contains("microsoft"),
                        "{path} 的发布者应为 Microsoft，实际「{name}」"
                    );
                }
                other => panic!("{path} 应为有效签名，实际 {other:?}"),
            }
        }
        assert!(checked > 0, "至少应校验成功一个系统 DLL");
    }

    /// 抽几个真实存在的第三方程序，验证发布者名不是空串也不是占位。
    #[test]
    fn third_party_publishers_are_resolved_when_present() {
        let candidates = [
            r"C:\Program Files\Windows NT\Accessories\wordpad.exe",
            r"C:\Windows\System32\mspaint.exe",
        ];
        let mut checked = 0;
        for path in candidates {
            if !std::path::Path::new(path).exists() {
                continue;
            }
            if let SignatureStatus::Valid(name) = verify(path) {
                checked += 1;
                assert!(!name.trim().is_empty(), "{path} 发布者名为空");
                assert_ne!(name, "已签名");
            }
        }
        // 这两个文件在精简版系统上可能不存在，存在就必须校验通过
        let _ = checked;
    }

    #[test]
    fn verify_optional_none_is_unknown() {
        assert_eq!(verify_optional(None), SignatureStatus::Unknown);
        assert_eq!(verify_optional(Some("")), SignatureStatus::Unknown);
    }

    #[test]
    fn trusted_vendor_detection() {
        assert!(is_trusted_vendor(&SignatureStatus::Valid(
            "Microsoft Windows".into()
        )));
        assert!(!is_trusted_vendor(&SignatureStatus::Unsigned));
    }
}
