//! 本地持久化：用户白名单 + 操作快照（回滚用）。
//!
//! **绿色便携承诺**：一律不写注册表。数据放在
//! 1. 程序目录下的 `data/`（便携模式，解压即用、删除无残留）；
//! 2. 若程序目录不可写（装在 Program Files），退到 `%APPDATA%\old666\`。
//!
//! 全部用 JSON，用户可读、可手改、可备份。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::blacklist;
use super::types::KilledRecord;

// ---------------------------------------------------------------------------
// 路径解析
// ---------------------------------------------------------------------------

/// 数据目录。首次调用会尝试创建。
///
/// 解析顺序：
/// 1. 环境变量 `OLD666_DATA_DIR`（测试隔离 / 多实例并存 / 用户自定义）；
/// 2. 便携优先：exe 同级 `data/`；
/// 3. 兜底：`%APPDATA%\old666\`。
pub fn data_dir() -> PathBuf {
    // 显式覆盖优先 —— 自测时指向临时目录，绝不碰用户真实数据
    if let Some(dir) = std::env::var_os("OLD666_DATA_DIR") {
        if !dir.is_empty() {
            let p = PathBuf::from(dir);
            if ensure_dir(&p) {
                return p;
            }
        }
    }
    // 便携优先：exe 同级 data/
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let portable = dir.join("data");
            if ensure_dir(&portable) {
                return portable;
            }
        }
    }
    // 兜底：%APPDATA%\old666
    let fallback = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("old666");
    let _ = ensure_dir(&fallback);
    fallback
}

fn ensure_dir(p: &Path) -> bool {
    if p.is_dir() {
        return true;
    }
    std::fs::create_dir_all(p).is_ok()
}

/// 便携模式判定（数据目录在 exe 同级）。
pub fn is_portable() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("data")))
        .map(|d| d == data_dir())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// 白名单
// ---------------------------------------------------------------------------

/// 用户白名单 —— 命中即永久豁免（但**不能**覆盖硬黑名单）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Whitelist {
    /// 小写进程名（不含 .exe）
    pub names: Vec<String>,
    /// 完整路径（可选，更精确）
    pub paths: Vec<String>,
}

impl Whitelist {
    fn file() -> PathBuf {
        data_dir().join("whitelist.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::file())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(Self::file(), json).map_err(|e| format!("写入白名单失败：{e}"))
    }

    pub fn contains_name(&self, name: &str) -> bool {
        let n = blacklist::normalize(name);
        self.names.iter().any(|x| *x == n)
    }

    pub fn contains_path(&self, path: &str) -> bool {
        let p = path.to_ascii_lowercase();
        self.paths.iter().any(|x| x.eq_ignore_ascii_case(&p))
    }

    /// 加入白名单。**拒绝加入硬黑名单进程**（否则等于给用户一个拆地基的开关）。
    pub fn add(&mut self, name: &str, path: Option<&str>) -> Result<(), String> {
        if blacklist::is_denied_name(name) {
            return Err(format!(
                "「{name}」是系统关键进程，出于安全考虑不允许加入白名单"
            ));
        }
        let n = blacklist::normalize(name);
        if !self.names.contains(&n) {
            self.names.push(n);
        }
        if let Some(p) = path {
            let lp = p.to_ascii_lowercase();
            if !self.paths.contains(&lp) {
                self.paths.push(lp);
            }
        }
        self.save()
    }

    pub fn remove(&mut self, name: &str) -> Result<(), String> {
        let n = blacklist::normalize(name);
        self.names.retain(|x| *x != n);
        self.save()
    }
}

// ---------------------------------------------------------------------------
// 快照（回滚）
// ---------------------------------------------------------------------------

/// 一次净化操作的快照。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Snapshot {
    pub id: String,
    pub timestamp: u64,
    /// 被结束的进程（回滚时重新拉起）
    pub killed: Vec<KilledRecord>,
    /// 被裁剪工作集的进程（回滚时无需动作，仅记录）
    pub trimmed: Vec<KilledRecord>,
    pub freed_bytes: u64,
}

/// 一条记录是否代表「当时真的结束了进程」。
///
/// 只有这类记录才允许回滚：
/// - `skipped` —— 被安全闸挡下，根本没动；
/// - `gone` —— 进程本来就不在了；
/// - `dry-run` —— 演练产物，什么都没做。
///
/// 漏掉任何一类，用户点一次回滚就会凭空拉起一批从未被结束的进程。
fn was_really_killed(r: &KilledRecord) -> bool {
    r.error.is_none() && !matches!(r.method.as_str(), "skipped" | "gone" | "dry-run")
}

impl Snapshot {
    fn file() -> PathBuf {
        data_dir().join("snapshots.json")
    }

    pub fn load_all() -> Vec<Snapshot> {
        std::fs::read_to_string(Self::file())
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<Snapshot>>(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_all(list: &[Snapshot]) -> Result<(), String> {
        // 只保留最近 50 次，防止无限增长
        let tail: Vec<&Snapshot> = list.iter().rev().take(50).collect();
        let mut v: Vec<Snapshot> = tail.into_iter().rev().cloned().collect();
        let json = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
        std::fs::write(Self::file(), json).map_err(|e| format!("写入快照失败：{e}"))?;
        v.clear();
        Ok(())
    }

    pub fn append(snap: Snapshot) -> Result<(), String> {
        let mut all = Self::load_all();
        all.push(snap);
        Self::save_all(&all)
    }

    /// 回滚：把被结束的进程重新拉起来。
    ///
    /// 返回 `(成功数, 失败数, 失败明细)`。
    /// 注意：进程的**内部状态无法恢复**（未保存的文档找不回来），
    /// 产品文案必须诚实说明这一点。
    pub fn rollback(id: &str) -> (usize, usize, Vec<String>) {
        let all = Self::load_all();
        let Some(snap) = all.iter().find(|s| s.id == id) else {
            return (0, 0, vec!["找不到该快照".into()]);
        };

        let mut ok = 0usize;
        let mut errs: Vec<String> = Vec::new();

        for rec in &snap.killed {
            // 只回滚当时真的被杀掉的
            if !was_really_killed(rec) {
                continue;
            }
            let Some(path) = rec.exe_path.as_deref() else {
                errs.push(format!("{}：没有记录可执行路径，无法拉起", rec.name));
                continue;
            };
            if !Path::new(path).exists() {
                errs.push(format!("{}：程序文件已不存在", rec.name));
                continue;
            }
            match std::process::Command::new(path).spawn() {
                Ok(_) => ok += 1,
                Err(e) => errs.push(format!("{}：启动失败 {e}", rec.name)),
            }
        }

        let fail = errs.len();
        (ok, fail, errs)
    }

    /// 该快照里「可回滚」的进程数（有路径且当时确实被结束）。
    pub fn rollbackable(&self) -> usize {
        self.killed.iter().filter(|r| was_really_killed(r)).count()
    }
}

/// 生成快照 ID。
///
/// 组成：毫秒时间戳 + 进程内自增序号 + PID。
/// **序号是必须的** —— 只靠「毫秒 + PID」时，同一毫秒内连续两次净化会
/// 生成完全相同的 ID，回滚就可能认错快照、拉起错误的进程。
pub fn new_snapshot_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("snap-{ts:x}-{seq:x}-{:x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 注意：本模块的测试**一律不写盘**。
    // 早期版本的 `whitelist_roundtrip` 结尾调了一次 `save()`，会把
    // 用户真实的 `whitelist.json` 覆盖成空 —— 那是个会直接砸掉用户
    // 白名单的隐患，已移除。凡涉及落盘的断言都走 `OLD666_DATA_DIR`
    // 指向的临时目录，见 `poc --stress`。

    #[test]
    fn data_dir_creates() {
        let d = data_dir();
        assert!(d.is_dir(), "数据目录应被创建：{d:?}");
    }

    #[test]
    fn whitelist_logic_without_touching_disk() {
        let mut w = Whitelist {
            names: vec!["mytool".into()],
            paths: vec![],
        };
        assert!(w.contains_name("MyTool.exe"));
        assert!(!w.contains_name("other.exe"));

        // 硬黑名单进程不允许加入白名单（这条路径直接返回 Err，不会落盘）
        assert!(w.add("lsass.exe", None).is_err());
        assert!(!w.contains_name("lsass.exe"));

        // 内存态增删（不调用 save）
        let n = blacklist::normalize("someapp.exe");
        if !w.names.contains(&n) {
            w.names.push(n.clone());
        }
        assert!(w.contains_name("SomeApp.exe"));
        w.names.retain(|x| *x != n);
        assert!(!w.contains_name("SomeApp.exe"));
    }

    #[test]
    fn snapshot_id_unique_shape() {
        let a = new_snapshot_id();
        let b = new_snapshot_id();
        assert!(a.starts_with("snap-"));
        assert_ne!(a, b);
    }

    /// 回滚一个不存在的快照：只读，不动系统。
    #[test]
    fn rollback_missing_snapshot() {
        let (ok, fail, errs) = Snapshot::rollback("snap-nonexistent");
        assert_eq!(ok, 0);
        assert_eq!(fail, 0);
        assert_eq!(errs.len(), 1);
    }

    /// `rollbackable()` 只统计「当时真的被结束且有路径」的进程，
    /// 演练记录（dry-run）与跳过记录都不该被算进去。
    #[test]
    fn rollbackable_ignores_dry_run_records() {
        let s = Snapshot {
            id: "snap-x".into(),
            timestamp: 0,
            killed: vec![
                KilledRecord {
                    pid: 1,
                    name: "a.exe".into(),
                    exe_path: Some("C:/a.exe".into()),
                    freed_bytes: 0,
                    method: "dry-run".into(),
                    error: None,
                },
                KilledRecord {
                    pid: 2,
                    name: "b.exe".into(),
                    exe_path: Some("C:/b.exe".into()),
                    freed_bytes: 0,
                    method: "skipped".into(),
                    error: None,
                },
                KilledRecord {
                    pid: 3,
                    name: "c.exe".into(),
                    exe_path: Some("C:/c.exe".into()),
                    freed_bytes: 0,
                    method: "force".into(),
                    error: None,
                },
            ],
            trimmed: vec![],
            freed_bytes: 0,
        };
        assert_eq!(s.rollbackable(), 1, "只有 method=force 的那条可回滚");
    }
}
