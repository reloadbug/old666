//! P4 · 垃圾文件扫描与安全删除。
//!
//! ## 三条不可逾越的红线
//!
//! 1. **只扫白名单目录。** 扫描范围是一份写死的目录清单，
//!    永远不遍历用户文档、照片、桌面、下载目录。
//!    目录清单之外的路径 —— 一律不看、一律不动。
//! 2. **删除一律走回收站。** 用 `SHFileOperationW` 的 `FO_DELETE` +
//!    `FOF_ALLOWUNDO`，文件进回收站而不是被物理抹掉。
//!    用户后悔了可以自己从回收站捞回来。
//! 3. **默认只扫描不删除。** 扫描是只读的；删除要单独一步，
//!    而且前端必须逐项列出清单让用户看清。
//!
//! 顺带一提：工作集裁剪（`EmptyWorkingSet`）不算清理 ——
//! 它不终止进程，也不删文件，属于 P1 的能力。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 一类垃圾。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JunkKind {
    /// 用户临时文件
    UserTemp,
    /// 系统临时文件
    WindowsTemp,
    /// 缩略图 / 图标缓存
    ThumbnailCache,
    /// Windows 更新残留
    UpdateResidue,
    /// 浏览器 / 应用崩溃转储
    CrashDump,
    /// 回收站（只统计，不清空 —— 清空回收站不可逆）
    RecycleBin,
}

impl JunkKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            JunkKind::UserTemp => "user_temp",
            JunkKind::WindowsTemp => "windows_temp",
            JunkKind::ThumbnailCache => "thumbnail_cache",
            JunkKind::UpdateResidue => "update_residue",
            JunkKind::CrashDump => "crash_dump",
            JunkKind::RecycleBin => "recycle_bin",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            JunkKind::UserTemp => "用户临时文件",
            JunkKind::WindowsTemp => "系统临时文件",
            JunkKind::ThumbnailCache => "缩略图缓存",
            JunkKind::UpdateResidue => "更新残留",
            JunkKind::CrashDump => "崩溃转储文件",
            JunkKind::RecycleBin => "回收站内容",
        }
    }

    /// 这一类能不能安全删除。
    ///
    /// 回收站内容**不能**通过我们的方式清空 —— 那会绕过回收站本身，
    /// 不可逆。所以它只统计不删。
    pub fn deletable(&self) -> bool {
        !matches!(self, JunkKind::RecycleBin)
    }
}

/// 一个待清理项。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct JunkItem {
    /// 完整路径
    pub path: String,
    pub kind: JunkKind,
    /// 文件大小（字节）；目录为其中所有文件之和
    pub size: u64,
    /// 最后修改时间（Unix 秒）
    pub modified: u64,
}

impl JunkItem {
    pub fn name(&self) -> String {
        Path::new(&self.path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.clone())
    }
}

/// 扫描结果。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct JunkScan {
    pub items: Vec<JunkItem>,
    /// 按类别汇总：`(类别, 文件数, 字节数)`
    pub groups: Vec<(String, usize, u64)>,
    /// 总字节数
    pub total_bytes: u64,
    /// 扫描耗时（毫秒）
    pub elapsed_ms: u64,
    /// 扫描期间被跳过的目录（权限不足等），人话说明
    pub skipped: Vec<String>,
}

/// 删除结果。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct JunkDeleteResult {
    /// 成功进回收站的条数
    pub deleted: usize,
    /// 失败条数
    pub failed: usize,
    /// 成功回收的字节数（估算）
    pub freed_bytes: u64,
    /// 逐条结果
    pub details: Vec<JunkDeleteDetail>,
    pub elapsed_ms: u64,
}

/// 单条删除结果。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct JunkDeleteDetail {
    pub path: String,
    pub ok: bool,
    pub error: Option<String>,
}

// ===========================================================================
// 白名单目录
// ===========================================================================

/// 判断一个路径是否落在「允许清理的目录白名单」内。
///
/// 这是删除前的最后一道闸：不在白名单里的路径一律拒绝，
/// 哪怕前端或 IPC 被构造成任意路径删除。
pub fn is_whitelisted(path: &str) -> bool {
    let norm = path.to_ascii_lowercase();
    targets().iter().any(|t| {
        let dn = t.dir.to_string_lossy().to_ascii_lowercase();
        !dn.is_empty() && norm.starts_with(&dn)
    })
}

/// 一个扫描目标：目录 + 类别 + 是否递归。
pub struct Target {
    dir: PathBuf,
    kind: JunkKind,
    recursive: bool,
    /// 只统计比这个时间新的文件（Unix 秒）—— 0 表示不限。
    /// 用来避开「正在使用中的临时文件」。
    min_age_secs: u64,
}

/// 取环境变量展开后的目录。
fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from).filter(|p| !p.as_os_str().is_empty())
}

/// 本机所有允许扫描的目录。
///
/// **这份清单就是安全边界的全部。** 里面没有、且不会有的东西：
/// 桌面、文档、图片、下载、OneDrive、任何用户自建目录。
pub fn targets() -> Vec<Target> {
    let now = super::process::now_unix();
    let day = 24 * 3600;
    let mut v = Vec::new();

    // 用户临时目录 —— 典型 %TEMP%，但很多机器上并不叫这个名字。
    if let Some(d) = env_dir("TEMP") {
        v.push(Target {
            dir: d,
            kind: JunkKind::UserTemp,
            recursive: true,
            min_age_secs: day,
        });
    }
    if let Some(d) = env_dir("TMP") {
        v.push(Target {
            dir: d,
            kind: JunkKind::UserTemp,
            recursive: true,
            min_age_secs: day,
        });
    }

    // 系统临时目录
    if let Some(d) = env_dir("SystemRoot") {
        v.push(Target {
            dir: d.join("Temp"),
            kind: JunkKind::WindowsTemp,
            recursive: true,
            min_age_secs: day,
        });
    }
    if let Some(d) = env_dir("windir") {
        v.push(Target {
            dir: d.join("Temp"),
            kind: JunkKind::WindowsTemp,
            recursive: true,
            min_age_secs: day,
        });
    }

    // 缩略图缓存
    if let Some(d) = env_dir("LOCALAPPDATA") {
        v.push(Target {
            dir: d.join("Microsoft").join("Windows").join("Explorer"),
            kind: JunkKind::ThumbnailCache,
            recursive: false,
            min_age_secs: 0,
        });
    }

    // Windows 更新残留
    if let Some(d) = env_dir("SystemRoot") {
        v.push(Target {
            dir: d.join("SoftwareDistribution").join("Download"),
            kind: JunkKind::UpdateResidue,
            recursive: true,
            min_age_secs: day,
        });
    }

    // 崩溃转储
    if let Some(d) = env_dir("LOCALAPPDATA") {
        v.push(Target {
            dir: d.join("CrashDumps"),
            kind: JunkKind::CrashDump,
            recursive: true,
            min_age_secs: 0,
        });
    }
    if let Some(d) = env_dir("ProgramData") {
        v.push(Target {
            dir: d.join("Microsoft").join("Windows").join("WER").join("ReportArchive"),
            kind: JunkKind::CrashDump,
            recursive: true,
            min_age_secs: day,
        });
    }

    let _ = now;
    v
}

/// 回收站目录。
fn recycle_bin_dir() -> Option<PathBuf> {
    env_dir("SystemDrive").map(|d| d.join("$Recycle.Bin"))
}

// ===========================================================================
// 扫描
// ===========================================================================

/// 扫描垃圾文件（**只读，不改任何东西**）。
pub fn scan() -> JunkScan {
    let t0 = std::time::Instant::now();
    let mut items: Vec<JunkItem> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for t in targets() {
        if !t.dir.is_dir() {
            continue;
        }
        let before = items.len();
        if t.recursive {
            match walk(&t.dir, &t, &mut items, &mut seen) {
                Ok(()) => {}
                Err(e) => skipped.push(format!("{}：{}", t.dir.display(), e)),
            }
        } else {
            let rd = match std::fs::read_dir(&t.dir) {
                Ok(r) => r,
                Err(e) => {
                    skipped.push(format!("{}：{}", t.dir.display(), e));
                    continue;
                }
            };
            for entry in rd.flatten() {
                let p = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                // 缩略图缓存只认这两类扩展名，别把别的东西算进来
                if !name.to_ascii_lowercase().ends_with(".db") && !name.to_ascii_lowercase().ends_with(".tmp")
                {
                    continue;
                }
                let Ok(meta) = entry.metadata() else { continue };
                if !meta.is_file() {
                    continue;
                }
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if t.min_age_secs > 0 && modified + t.min_age_secs > super::process::now_unix() {
                    continue;
                }
                push_item(&mut items, &mut seen, &p, t.kind, meta.len(), modified);
            }
        }
        let _ = before;
    }

    // 回收站：只统计体积，不提供删除
    if let Some(d) = recycle_bin_dir() {
        let mut total = 0u64;
        let mut count = 0usize;
        if let Ok(rd) = std::fs::read_dir(&d) {
            for entry in rd.flatten() {
                // 回收站里每个用户的子目录是 S-1-5-… 的 SID
                let sub = entry.path();
                if let Ok(rd2) = std::fs::read_dir(&sub) {
                    for f in rd2.flatten() {
                        if let Ok(m) = f.metadata() {
                            if m.is_file() {
                                total += m.len();
                                count += 1;
                            }
                        }
                    }
                }
            }
        }
        if count > 0 {
            items.push(JunkItem {
                path: d.to_string_lossy().to_string(),
                kind: JunkKind::RecycleBin,
                size: total,
                modified: 0,
            });
        }
    }

    // 按类别汇总
    let mut groups: Vec<(String, usize, u64)> = Vec::new();
    for k in [
        JunkKind::UserTemp,
        JunkKind::WindowsTemp,
        JunkKind::ThumbnailCache,
        JunkKind::UpdateResidue,
        JunkKind::CrashDump,
        JunkKind::RecycleBin,
    ] {
        let sel: Vec<&JunkItem> = items.iter().filter(|i| i.kind == k).collect();
        if sel.is_empty() {
            continue;
        }
        groups.push((
            k.as_str().to_string(),
            sel.len(),
            sel.iter().map(|i| i.size).sum(),
        ));
    }
    groups.sort_by(|a, b| b.2.cmp(&a.2));

    let total_bytes = items.iter().map(|i| i.size).sum();
    // 体积大的排前面，用户先看到最值钱的那几项
    items.sort_by(|a, b| b.size.cmp(&a.size));

    JunkScan {
        items,
        groups,
        total_bytes,
        elapsed_ms: t0.elapsed().as_millis() as u64,
        skipped,
    }
}

fn push_item(
    items: &mut Vec<JunkItem>,
    seen: &mut std::collections::HashSet<String>,
    p: &Path,
    kind: JunkKind,
    size: u64,
    modified: u64,
) {
    let key = p.to_string_lossy().to_ascii_lowercase();
    if seen.contains(&key) {
        return;
    }
    seen.insert(key);
    items.push(JunkItem {
        path: p.to_string_lossy().to_string(),
        kind,
        size,
        modified,
    });
}

/// 递归收集一个目录下的文件。
fn walk(
    dir: &Path,
    t: &Target,
    items: &mut Vec<JunkItem>,
    seen: &mut std::collections::HashSet<String>,
) -> Result<(), String> {
    let rd = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    let now = super::process::now_unix();
    for entry in rd.flatten() {
        let p = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        // 符号链接 / 联接点不跟进去，避免绕出白名单目录
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            let _ = walk(&p, t, items, seen);
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        let modified = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if t.min_age_secs > 0 && modified + t.min_age_secs > now {
            continue;
        }
        push_item(items, seen, &p, t.kind, meta.len(), modified);
    }
    Ok(())
}

// ===========================================================================
// 删除（走回收站）
// ===========================================================================

/// 把一批路径送进回收站。
///
/// 用 `SHFileOperationW` 的 `FO_DELETE` + `FOF_ALLOWUNDO`：
/// 文件被移到回收站，用户可以自己恢复。**不做物理删除。**
///
/// 传入的路径必须都在白名单目录内 —— 这里再查一遍，
/// 防止前端或 IPC 被构造成任意路径删除。
pub fn delete_to_recycle_bin(paths: &[String]) -> JunkDeleteResult {
    let t0 = std::time::Instant::now();
    let mut r = JunkDeleteResult::default();

    // 安全复核：只接受白名单目录下的路径
    let mut valid: Vec<String> = Vec::new();
    for p in paths {
        if is_whitelisted(p) {
            valid.push(p.clone());
        } else {
            r.details.push(JunkDeleteDetail {
                path: p.clone(),
                ok: false,
                error: Some("该路径不在允许清理的目录白名单内，已拒绝".into()),
            });
            r.failed += 1;
        }
    }
    if valid.is_empty() {
        r.elapsed_ms = t0.elapsed().as_millis() as u64;
        return r;
    }

    // 双 NUL 结尾的路径列表
    let mut buf: Vec<u16> = Vec::new();
    for p in &valid {
        buf.extend(p.encode_utf16());
        buf.push(0);
    }
    buf.push(0);

    use windows::Win32::UI::Shell::{SHFileOperationW, SHFILEOPSTRUCTW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE};

    let mut op = SHFILEOPSTRUCTW {
        hwnd: windows::Win32::Foundation::HWND::default(),
        wFunc: FO_DELETE as u32,
        pFrom: windows::core::PCWSTR(buf.as_ptr()),
        pTo: windows::core::PCWSTR::null(),
        fFlags: (FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_SILENT.0 | FOF_NOERRORUI.0) as u16,
        fAnyOperationsAborted: windows::Win32::Foundation::BOOL(0),
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: windows::core::PCWSTR::null(),
    };
    // SAFETY: op 结构合法，pFrom 指向双 NUL 结尾的有效串。
    let res = unsafe { SHFileOperationW(&mut op) };
    let ok = res == 0 && !op.fAnyOperationsAborted.as_bool();

    if ok {
        for p in &valid {
            r.deleted += 1;
            r.details.push(JunkDeleteDetail {
                path: p.clone(),
                ok: true,
                error: None,
            });
        }
    } else {
        for p in &valid {
            r.failed += 1;
            r.details.push(JunkDeleteDetail {
                path: p.clone(),
                ok: false,
                error: Some(format!("回收站操作未成功（代码 {}）", res)),
            });
        }
    }
    r.elapsed_ms = t0.elapsed().as_millis() as u64;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junk_kinds_have_labels() {
        for k in [
            JunkKind::UserTemp,
            JunkKind::WindowsTemp,
            JunkKind::ThumbnailCache,
            JunkKind::UpdateResidue,
            JunkKind::CrashDump,
            JunkKind::RecycleBin,
        ] {
            assert!(!k.label().is_empty());
            assert!(!k.as_str().is_empty());
        }
    }

    #[test]
    fn recycle_bin_is_not_deletable_by_us() {
        assert!(!JunkKind::RecycleBin.deletable());
        assert!(JunkKind::UserTemp.deletable());
    }

    #[test]
    fn targets_never_include_user_documents() {
        // 这是本模块最重要的断言：扫描范围里绝不能出现用户目录。
        let home = std::env::var_os("USERPROFILE")
            .map(|p| p.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        for t in targets() {
            let d = t.dir.to_string_lossy().to_ascii_lowercase();
            // 允许用户临时目录（它通常就在 profile\Temp 下），
            // 但不允许 profile 本身或文档/桌面/下载等目录。
            if home.is_empty() {
                continue;
            }
            for forbidden in ["\\desktop", "\\documents", "\\pictures", "\\downloads", "\\music", "\\videos"] {
                assert!(!d.contains(forbidden), "扫描目标不能包含 {}", forbidden);
            }
            assert!(d != home, "扫描目标不能是用户主目录本身");
        }
    }

    #[test]
    fn delete_rejects_paths_outside_whitelist() {
        let r = delete_to_recycle_bin(&[
            "C:\\Windows\\System32\\config\\SAM".to_string(),
            "C:\\Users\\Administrator\\Documents\\重要文档.docx".to_string(),
        ]);
        assert_eq!(r.deleted, 0);
        assert_eq!(r.failed, 2);
        for d in &r.details {
            assert!(d.error.is_some(), "白名单外的路径必须带拒绝理由");
        }
    }

    #[test]
    fn whitelist_check_accepts_temp_and_rejects_documents() {
        let temp = std::env::temp_dir();
        let f = temp.join("old666_whitelist_probe.tmp");
        assert!(
            is_whitelisted(&f.to_string_lossy()),
            "用户临时目录应在白名单内"
        );
        assert!(
            !is_whitelisted("C:\\Users\\Administrator\\Documents\\重要文档.docx"),
            "文档目录绝不能进白名单"
        );
        assert!(
            !is_whitelisted("C:\\Windows\\System32\\config\\SAM"),
            "系统目录绝不能进白名单"
        );
        assert!(
            !is_whitelisted("C:\\Users\\Administrator\\Desktop\\a.txt"),
            "桌面绝不能进白名单"
        );
    }

    #[test]
    fn scan_is_read_only_and_reports_groups() {
        let s = scan();
        // 扫描本身不该改变任何东西 —— 这里只断言它能跑完且结构自洽。
        let sum: u64 = s.groups.iter().map(|g| g.2).sum();
        assert!(sum <= s.total_bytes, "分组字节之和不能超过总量");
        for g in &s.groups {
            assert!(!g.0.is_empty());
        }
    }
}
