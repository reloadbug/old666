//! P4 · 一键体检：体检项定义与评分模型。
//!
//! 设计取向：**宁可少报，不可乱报**。
//!
//! - 每一项都必须能给出「人话理由」，说清「为什么这条扣分」；
//! - 任何一项拿不到确定结论时**不算扣分**，只标记为「无法判定」；
//! - 评分模型与实际系统状态正相关（用户能人工校验出来）。
//!
//! 体检是**纯只读**的：不结束进程、不删文件、不改任何系统设置。

use serde::{Deserialize, Serialize};

use super::memory;
use super::process;
use super::startup;

/// 体检项的严重度。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// 良好
    Good,
    /// 值得留意
    Notice,
    /// 建议处理
    Warn,
    /// 严重
    Bad,
    /// 无法判定
    Unknown,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Good => "good",
            Severity::Notice => "notice",
            Severity::Warn => "warn",
            Severity::Bad => "bad",
            Severity::Unknown => "unknown",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Severity::Good => "良好",
            Severity::Notice => "留意",
            Severity::Warn => "建议处理",
            Severity::Bad => "严重",
            Severity::Unknown => "无法判定",
        }
    }
}

/// 一个体检项的结论。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CheckItem {
    /// 稳定 ID，前端用来做「一键修复」的路由
    pub id: String,
    /// 标题，如「内存占用偏高」
    pub title: String,
    /// 人话理由 —— 说清「为什么」
    pub reason: String,
    /// 建议怎么做（可为空）
    pub advice: String,
    pub severity: Severity,
    /// 这一项扣了多少分（0 表示不扣）
    pub cost: u32,
    /// 可量化的证据（如「内存占用 87%」）
    pub metric: String,
    /// 是否支持一键修复
    pub fixable: bool,
    /// 修复动作标识（前端据此路由到具体能力）
    pub fix: Option<String>,
}

impl CheckItem {
    fn good(id: &str, title: &str, metric: &str) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            reason: "正常".into(),
            advice: String::new(),
            severity: Severity::Good,
            cost: 0,
            metric: metric.into(),
            fixable: false,
            fix: None,
        }
    }
}

/// 一次体检的完整结果。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CheckReport {
    /// 健康分 0–100
    pub score: u32,
    /// 各项结论，按扣分从多到少排序（扣 0 分的排最后）
    pub items: Vec<CheckItem>,
    /// 体检耗时（毫秒）
    pub elapsed_ms: u64,
    /// 体检摘要，一句话
    pub summary: String,
    /// 是否需要管理员权限才能完整体检
    pub needs_admin: bool,
}

/// 满分。
pub const MAX_SCORE: u32 = 100;

// ===========================================================================
// 各项体检
// ===========================================================================

/// 内存占用。
fn check_memory() -> CheckItem {
    let pct = memory::memory_percent();
    let (total, avail) = memory::system_memory();
    let total_gb = total as f64 / (1024.0 * 1024.0 * 1024.0);
    let metric = format!("占用 {:.0}%（共 {:.1} GB，剩余 {:.1} GB）", pct, total_gb, avail as f64 / (1024.0 * 1024.0 * 1024.0));
    if pct >= 90.0 {
        CheckItem {
            id: "memory".into(),
            title: "内存占用很高".into(),
            reason: format!("物理内存已用 {:.0}%，系统已经明显吃紧，程序开始变卡。", pct),
            advice: "到「净化」页做一次清理，或切换到「服务器模式」只裁剪不结束。".into(),
            severity: Severity::Bad,
            cost: 18,
            metric,
            fixable: true,
            fix: Some("trim_memory".into()),
        }
    } else if pct >= 80.0 {
        CheckItem {
            id: "memory".into(),
            title: "内存占用偏高".into(),
            reason: format!("物理内存已用 {:.0}%，还没到吃紧，但已经进入该清理的区间。", pct),
            advice: "可以跑一次「温和清理」。".into(),
            severity: Severity::Warn,
            cost: 8,
            metric,
            fixable: true,
            fix: Some("trim_memory".into()),
        }
    } else if pct >= 65.0 {
        CheckItem {
            id: "memory".into(),
            title: "内存占用正常偏高".into(),
            reason: format!("物理内存已用 {:.0}%，在正常范围内偏高。", pct),
            advice: String::new(),
            severity: Severity::Notice,
            cost: 3,
            metric,
            fixable: false,
            fix: None,
        }
    } else {
        let mut it = CheckItem::good("memory", "内存占用正常", &metric);
        it.reason = format!("物理内存已用 {:.0}%，处于健康区间。", pct);
        it
    }
}

/// 启动项数量与风险。
fn check_startup() -> CheckItem {
    let scan = startup::scan();
    let total = scan.items.len();
    let disabled_candidates = scan.disable_candidates;
    let metric = format!("共 {} 项，其中 {} 项建议禁用", total, disabled_candidates);
    if total >= 80 {
        CheckItem {
            id: "startup".into(),
            title: "开机启动项偏多".into(),
            reason: format!(
                "自启动条目 {} 项（注册表 / 文件夹 / 服务 / 计划任务），开机时都要排队。",
                total
            ),
            advice: "到「开机加速」页批量禁用不需要的项，可延迟的改为延迟启动。".into(),
            severity: Severity::Warn,
            cost: 12,
            metric,
            fixable: true,
            fix: Some("startup_scan".into()),
        }
    } else if total >= 50 {
        CheckItem {
            id: "startup".into(),
            title: "启动项数量一般".into(),
            reason: format!("自启动条目 {} 项，属于常见范围。", total),
            advice: String::new(),
            severity: Severity::Notice,
            cost: 4,
            metric,
            fixable: false,
            fix: None,
        }
    } else {
        let mut it = CheckItem::good("startup", "启动项数量正常", &metric);
        it.reason = format!("自启动条目只有 {} 项，开机负担轻。", total);
        it
    }
}

/// 进程数量。
fn check_process_count() -> CheckItem {
    let (raw, _) = process::enumerate();
    let n = raw.len();
    let metric = format!("{} 个进程", n);
    if n >= 350 {
        CheckItem {
            id: "process_count".into(),
            title: "进程数量偏多".into(),
            reason: format!("当前有 {} 个进程在跑，内存与调度压力都上来了。", n),
            advice: "到「净化」页查看可以安全结束的部分。".into(),
            severity: Severity::Warn,
            cost: 10,
            metric,
            fixable: true,
            fix: Some("purge_scan".into()),
        }
    } else if n >= 250 {
        CheckItem {
            id: "process_count".into(),
            title: "进程数量略多".into(),
            reason: format!("当前有 {} 个进程在跑，属于常见范围。", n),
            advice: String::new(),
            severity: Severity::Notice,
            cost: 3,
            metric,
            fixable: false,
            fix: None,
        }
    } else {
        let mut it = CheckItem::good("process_count", "进程数量正常", &metric);
        it.reason = format!("当前只有 {} 个进程，系统负担轻。", n);
        it
    }
}

/// 无签名程序 —— 潜在的安全隐患。
fn check_unsigned() -> CheckItem {
    // 这里刻意**不**做全盘签名校验：那要哈希整个文件，代价太高。
    // 只统计「可执行文件所在目录属于用户目录、且没有签名」的进程。
    let (raw, _) = process::enumerate();
    let mut unsigned = 0usize;
    for p in &raw {
        let Some(path) = process::query_exe_path(p.pid) else {
            continue;
        };
        if !path.to_ascii_lowercase().contains("\\users\\") {
            continue;
        }
        match super::signature::verify_optional(Some(&path)) {
            crate::core::types::SignatureStatus::Unsigned => unsigned += 1,
            crate::core::types::SignatureStatus::Invalid => unsigned += 1,
            _ => {}
        }
    }
    let metric = format!("{} 个用户目录程序未通过签名校验", unsigned);
    if unsigned >= 8 {
        CheckItem {
            id: "unsigned".into(),
            title: "多个程序没有有效数字签名".into(),
            reason: format!(
                "有 {} 个跑在用户目录下的程序既没有数字签名、签名也校验不过。\
                 这不代表它们一定有问题，但值得你亲自确认一下来源。",
                unsigned
            ),
            advice: "在「净化」页查看这些程序的完整路径与发布者。".into(),
            severity: Severity::Warn,
            cost: 8,
            metric,
            fixable: false,
            fix: None,
        }
    } else if unsigned >= 3 {
        CheckItem {
            id: "unsigned".into(),
            title: "个别程序没有有效数字签名".into(),
            reason: format!("有 {} 个用户目录下的程序未通过签名校验。", unsigned),
            advice: String::new(),
            severity: Severity::Notice,
            cost: 3,
            metric,
            fixable: false,
            fix: None,
        }
    } else {
        let mut it = CheckItem::good("unsigned", "程序来源总体可信", &metric);
        it.reason = "用户目录下的程序基本都有有效数字签名。".into();
        it
    }
}

/// 磁盘可用空间。
fn check_disk() -> CheckItem {
    let Ok(total) = disk_space() else {
        let mut it = CheckItem::good("disk", "磁盘空间正常", "无法读取");
        it.severity = Severity::Unknown;
        it.reason = "读不到磁盘信息，无法判定。".into();
        return it;
    };
    let pct = if total.total > 0 {
        (total.free as f64 / total.total as f64) * 100.0
    } else {
        100.0
    };
    let metric = format!("剩余 {:.1} GB（{:.0}%）", total.free as f64 / 1e9, pct);
    if pct < 8.0 {
        CheckItem {
            id: "disk".into(),
            title: "系统盘空间紧张".into(),
            reason: format!("系统盘只剩 {:.0}%（{:.1} GB）可用，Windows 会因为空间不足而变慢。", pct, total.free as f64 / 1e9),
            advice: "用「清理」页清掉临时文件与更新残留。".into(),
            severity: Severity::Bad,
            cost: 16,
            metric,
            fixable: true,
            fix: Some("junk_scan".into()),
        }
    } else if pct < 15.0 {
        CheckItem {
            id: "disk".into(),
            title: "系统盘空间偏紧".into(),
            reason: format!("系统盘只剩 {:.0}% 可用。", pct),
            advice: "建议定期清理临时文件。".into(),
            severity: Severity::Warn,
            cost: 7,
            metric,
            fixable: true,
            fix: Some("junk_scan".into()),
        }
    } else {
        let mut it = CheckItem::good("disk", "磁盘空间充足", &metric);
        it.reason = format!("系统盘还有 {:.0}% 可用。", pct);
        it
    }
}

/// 磁盘空间信息。
struct DiskSpace {
    total: u64,
    free: u64,
}

fn disk_space() -> Result<DiskSpace, String> {
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows::Win32::Foundation::GetLastError;
    let path: Vec<u16> = "C:\\".encode_utf16().chain(std::iter::once(0)).collect();
    let mut free_to_caller = 0u64;
    let mut total = 0u64;
    let mut total_free = 0u64;
    // SAFETY: path 是有效 NUL 结尾的 UTF-16 串，输出参数都是有效引用。
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            windows::core::PCWSTR(path.as_ptr()),
            Some(&mut free_to_caller),
            Some(&mut total),
            Some(&mut total_free),
        )
    };
    if ok.is_ok() {
        Ok(DiskSpace {
            total,
            free: free_to_caller,
        })
    } else {
        Err(format!("GetDiskFreeSpaceExW failed: {:?}", unsafe { GetLastError() }))
    }
}

// ===========================================================================
// 汇总
// ===========================================================================

/// 跑一次完整体检（纯只读）。
pub fn run() -> CheckReport {
    let t0 = std::time::Instant::now();
    let mut items = vec![
        check_memory(),
        check_disk(),
        check_startup(),
        check_process_count(),
        check_unsigned(),
    ];

    // 扣分从多到少；扣 0 分的排最后。
    items.sort_by(|a, b| {
        b.cost
            .cmp(&a.cost)
            .then_with(|| a.id.cmp(&b.id))
    });

    let total_cost: u32 = items.iter().map(|i| i.cost).sum();
    let score = MAX_SCORE.saturating_sub(total_cost);

    let bad = items.iter().filter(|i| i.severity == Severity::Bad).count();
    let warn = items.iter().filter(|i| i.severity == Severity::Warn).count();
    let summary = if bad > 0 {
        format!("发现 {} 项严重问题、{} 项建议处理的问题。", bad, warn)
    } else if warn > 0 {
        format!("整体健康，有 {} 项建议处理的问题。", warn)
    } else {
        "整体健康，没有发现明显问题。".to_string()
    };

    CheckReport {
        score,
        items,
        elapsed_ms: t0.elapsed().as_millis() as u64,
        summary,
        needs_admin: super::is_elevated(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_is_bounded() {
        let r = run();
        assert!(r.score <= MAX_SCORE, "健康分不能超过满分");
    }

    #[test]
    fn items_are_sorted_by_cost_desc() {
        let r = run();
        for w in r.items.windows(2) {
            assert!(w[0].cost >= w[1].cost, "体检项必须按扣分从多到少排序");
        }
    }

    #[test]
    fn every_item_has_a_reason() {
        let r = run();
        for i in &r.items {
            assert!(!i.reason.trim().is_empty(), "体检项「{}」必须有理由", i.id);
            assert!(!i.title.trim().is_empty(), "体检项「{}」必须有标题", i.id);
        }
    }

    #[test]
    fn severity_labels_cover_all() {
        for s in [Severity::Good, Severity::Notice, Severity::Warn, Severity::Bad, Severity::Unknown] {
            assert!(!s.label().is_empty());
            assert!(!s.as_str().is_empty());
        }
    }
}
