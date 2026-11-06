//! 启动项的白名单 / 黑名单。
//!
//! 与 P1 的**进程**白名单（`core/store.rs`）是两回事：那边按 PID / 进程名豁免净化，
//! 这边按启动项名称决定「要不要建议你关掉它」。
//!
//! * **白名单** = 「这个我要留着」。它不会被「一键选中推荐禁用项」选中，
//!   列表里带「已加白」标记。哪怕风险引擎说它可禁用，用户说了算。
//! * **黑名单** = 「这个我要关，别再问我」。加入黑名单时**直接禁用**，
//!   不进二次确认弹窗；之后每次扫描到它仍处于启用状态（例如软件自己又开了回来），
//!   「一键优化」会把它一并关掉，同样不弹窗。
//!
//! 匹配用**名称**（小写、去 `.exe`）而不是条目 ID：条目 ID 里含注册表路径，
//! 软件升级换路径后 ID 就变了，用户加的名单会静默失效。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::core::store;
use crate::core::types::StartupItem;

/// 名单种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    White,
    Black,
}

impl ListKind {
    pub fn parse(s: &str) -> Option<ListKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "white" | "whitelist" => Some(ListKind::White),
            "black" | "blacklist" => Some(ListKind::Black),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            ListKind::White => "white",
            ListKind::Black => "black",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            ListKind::White => "白名单",
            ListKind::Black => "黑名单",
        }
    }
}

/// 两份名单。存的是归一化后的匹配串（小写、去 `.exe`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Lists {
    #[serde(default)]
    pub whitelist: Vec<String>,
    #[serde(default)]
    pub blacklist: Vec<String>,
}

/// 名单文件路径（外置 JSON，用户可以直接编辑或备份）。
pub fn path() -> PathBuf {
    store::data_dir().join("startup_lists.json")
}

impl Lists {
    /// 读名单。文件不存在 / 坏了都退回空名单 ——
    /// 名单读不出来不该让整页扫描失败，最多是标记少了两行。
    pub fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(path()) else {
            return Self::default();
        };
        let mut l: Self = serde_json::from_str(&text).unwrap_or_default();
        // 顺手归一化：用户手写 `OneDrive.exe` 也要能匹配上
        l.whitelist = l.whitelist.iter().map(|k| norm(k)).filter(|k| !k.is_empty()).collect();
        l.blacklist = l.blacklist.iter().map(|k| norm(k)).filter(|k| !k.is_empty()).collect();
        l.whitelist.sort();
        l.whitelist.dedup();
        l.blacklist.sort();
        l.blacklist.dedup();
        l
    }

    pub fn save(&self) -> Result<(), String> {
        let p = path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败：{e}"))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| format!("序列化失败：{e}"))?;
        std::fs::write(&p, text).map_err(|e| format!("写入名单失败：{e}"))
    }

    pub fn get(&self, kind: ListKind) -> &[String] {
        match kind {
            ListKind::White => &self.whitelist,
            ListKind::Black => &self.blacklist,
        }
    }

    fn get_mut(&mut self, kind: ListKind) -> &mut Vec<String> {
        match kind {
            ListKind::White => &mut self.whitelist,
            ListKind::Black => &mut self.blacklist,
        }
    }

    /// 加入名单。已在名单里返回 `Err`，让 UI 能如实说「已经在里面了」。
    ///
    /// 两份名单互斥：加进白名单时会自动从黑名单移除，反之亦然 ——
    /// 同时出现在两边会得到自相矛盾的结论（既推荐保留又直接禁用）。
    pub fn add(&mut self, kind: ListKind, key: &str) -> Result<(), String> {
        let k = norm(key);
        if k.is_empty() {
            return Err("名称不能为空".into());
        }
        let other = match kind {
            ListKind::White => ListKind::Black,
            ListKind::Black => ListKind::White,
        };
        if self.get(kind).contains(&k) {
            return Err(format!("「{k}」已经在{}里了", kind.label()));
        }
        self.get_mut(other).retain(|x| x != &k);
        let v = self.get_mut(kind);
        v.push(k);
        v.sort();
        Ok(())
    }

    pub fn remove(&mut self, kind: ListKind, key: &str) -> Result<(), String> {
        let k = norm(key);
        let v = self.get_mut(kind);
        let before = v.len();
        v.retain(|x| x != &k);
        if v.len() == before {
            return Err(format!("「{k}」不在{}里", kind.label()));
        }
        Ok(())
    }

    pub fn contains(&self, kind: ListKind, name: &str) -> bool {
        let k = norm(name);
        !k.is_empty() && self.get(kind).contains(&k)
    }

    /// 这条启动项是否命中名单。
    ///
    /// 两个来源都认：**显示名**（`Microsoft OneDrive`）和**命令行里的
    /// 可执行文件名**（`onedrive`）。用户脑子里的「它」通常是 exe 名，
    /// 而列表里显示的是软件写进注册表的那个名字 —— 只认其中一个，
    /// 用户就会遇到「我明明加了黑名单，它却没被处理」。
    pub fn hits(&self, kind: ListKind, it: &StartupItem) -> bool {
        if self.contains(kind, &it.name) {
            return true;
        }
        match crate::core::knowledge::exe_base(&it.command) {
            Some(base) if !base.is_empty() => self.contains(kind, &base),
            _ => false,
        }
    }

    /// 给一批启动项打上名单标记。一次读盘，批量应用。
    pub fn apply_all(&self, items: &mut [StartupItem]) {
        for it in items.iter_mut() {
            it.in_whitelist = self.hits(ListKind::White, it);
            it.in_blacklist = self.hits(ListKind::Black, it);
        }
    }
}

/// 归一化：小写、去 `.exe`、去首尾空白。
pub fn norm(name: &str) -> String {
    let n = name.trim().to_ascii_lowercase();
    let n = n.strip_suffix(".exe").unwrap_or(&n);
    n.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_strips_exe_and_case() {
        assert_eq!(norm("OneDrive.exe"), "onedrive");
        assert_eq!(norm("  ONEDRIVE  "), "onedrive");
        assert_eq!(norm(""), "");
    }

    /// 两份名单必须互斥 —— 同时命中会得出「既保留又禁用」的矛盾结论。
    #[test]
    fn lists_are_mutually_exclusive() {
        let mut l = Lists::default();
        l.add(ListKind::Black, "OneDrive").unwrap();
        assert!(l.contains(ListKind::Black, "onedrive.exe"));
        l.add(ListKind::White, "onedrive").unwrap();
        assert!(l.contains(ListKind::White, "OneDrive"));
        assert!(
            !l.contains(ListKind::Black, "onedrive"),
            "加进白名单后必须自动从黑名单移除"
        );
    }

    /// 用户按 exe 名加名单，必须能命中「显示名」完全不同的条目 ——
    /// 这是用户最容易踩的坑（软件写的显示名带厂商前缀）。
    #[test]
    fn list_matches_the_exe_name_not_just_the_display_name() {
        let mut l = Lists::default();
        l.add(ListKind::Black, "onedrive").unwrap();

        let it = StartupItem::blank_for_test(
            "Microsoft OneDrive",
            r#""C:\Program Files\Microsoft OneDrive\OneDrive.exe" /background"#,
        );
        assert!(
            l.hits(ListKind::Black, &it),
            "按 exe 名加黑名单应命中显示名带厂商前缀的条目"
        );
        assert!(!l.hits(ListKind::White, &it));

        // 反向：按显示名加白，也应命中
        let mut l2 = Lists::default();
        l2.add(ListKind::White, "Microsoft OneDrive").unwrap();
        assert!(l2.hits(ListKind::White, &it));
    }

    #[test]
    fn duplicate_add_is_rejected_with_reason() {
        let mut l = Lists::default();
        l.add(ListKind::White, "a").unwrap();
        let e = l.add(ListKind::White, "a.exe").unwrap_err();
        assert!(e.contains("已经在"), "{e}");
    }
}
