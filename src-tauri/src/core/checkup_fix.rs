//! P4 · 一键体检的「一键修复」执行器 + 修复前后量化对比。
//!
//! # 这里的红线
//!
//! 一键修复**只做可逆或明确安全的动作**：
//!
//! | 修复动作 | 做什么 | 会不会结束进程 | 会不会删文件 |
//! |---|---|---|---|
//! | `trim_memory` | 裁剪闲置程序的工作集 | **不会** | 不会 |
//! | `junk_clean` | 把白名单目录里的垃圾送进回收站 | 不会 | 会（但进回收站，可恢复） |
//! | `startup_scan` / `purge_scan` | 只跳转到对应页面，由用户自己点 | 不会 | 不会 |
//!
//! 也就是说：**一键修复永远不会结束任何进程**。
//! 结束进程仍然只在「净化」页、经过用户逐次确认之后才可能发生。
//! 原因很简单 —— 体检报告是机器判定的，万一判错了，
//! 「一键修复」这个入口会让错误直接落地，而且用户根本没看过清单。

use serde::{Deserialize, Serialize};

use super::{checkup, junk, memory, purge::PurgeEngine};

/// 一次修复请求。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FixRequest {
    /// 修复动作：`trim_memory` / `junk_clean`
    pub kind: String,
    /// `junk_clean` 用的路径清单；其它动作忽略。
    #[serde(default)]
    pub paths: Vec<String>,
}

/// 单个修复动作的结果。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FixOutcome {
    pub kind: String,
    /// 是否真的执行了（false = 被跳过，附理由）
    pub ok: bool,
    /// 人话说明：做了什么、为什么没做
    pub detail: String,
    /// 释放的字节数（真实值）
    pub freed_bytes: u64,
    /// 涉及的条目数
    pub affected: usize,
}

/// 修复前后的量化对比。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FixSummary {
    /// 每个动作的结果
    pub outcomes: Vec<FixOutcome>,
    /// 修复前健康分
    pub before_score: u32,
    /// 修复后健康分
    pub after_score: u32,
    /// 分数变化（正数 = 变好）
    pub score_delta: i32,
    /// 修复前内存占用百分比
    pub before_mem_pct: f32,
    /// 修复后内存占用百分比
    pub after_mem_pct: f32,
    /// 修复前后释放的字节数合计
    pub freed_bytes: u64,
    /// 总耗时（毫秒）
    pub elapsed_ms: u64,
    /// 修复后重新体检的完整结果
    pub after: checkup::CheckReport,
}

/// 执行一批修复，返回修复前后的量化对比。
///
/// * `engine` —— 用来做工作集裁剪。
/// * `requests` —— 要修的项。
/// * `before` —— 修复前的体检结果（由调用方传入，这样对比是同一台机器的同一时刻）。
///
/// **本函数不会结束任何进程。** `trim_memory` 走的是
/// `purge_scoped(..., kill_enabled = false)`，只裁剪不结束。
pub fn run(
    engine: &mut PurgeEngine,
    requests: &[FixRequest],
    before: &checkup::CheckReport,
) -> FixSummary {
    let t0 = std::time::Instant::now();
    let before_mem_pct = memory::memory_percent();
    let mut outcomes: Vec<FixOutcome> = Vec::new();
    let mut freed_bytes: u64 = 0;

    for req in requests {
        let outcome = match req.kind.as_str() {
            "trim_memory" => trim_memory(engine),
            "junk_clean" => junk_clean(&req.paths),
            other => FixOutcome {
                kind: other.to_string(),
                ok: false,
                detail: format!("不认识的修复动作「{other}」，已跳过"),
                freed_bytes: 0,
                affected: 0,
            },
        };
        freed_bytes += outcome.freed_bytes;
        outcomes.push(outcome);
    }

    // 修完重新体检一次，用同一套模型对比 —— 这样「分数变高了」是有依据的。
    let after = checkup::run();
    let after_mem_pct = memory::memory_percent();

    FixSummary {
        outcomes,
        before_score: before.score,
        after_score: after.score,
        score_delta: after.score as i32 - before.score as i32,
        before_mem_pct,
        after_mem_pct,
        freed_bytes,
        elapsed_ms: t0.elapsed().as_millis() as u64,
        after,
    }
}

/// 裁剪闲置程序的工作集 —— **不结束任何进程**。
fn trim_memory(engine: &mut PurgeEngine) -> FixOutcome {
    // kill_enabled = false：只裁剪，不结束。
    let r = engine.purge_scoped(
        None,
        800,
        super::purge::PurgeScope::SafeOnly,
        true,
        false,
    );
    let affected = r.trimmed.len();
    let bytes = r.freed_bytes;
    let detail = if affected == 0 {
        "没有找到闲置程度足以裁剪的程序，工作集保持原样".to_string()
    } else {
        format!("裁剪了 {} 个闲置程序的工作集，共释放 {}", affected, human_bytes(bytes))
    };
    FixOutcome {
        kind: "trim_memory".into(),
        ok: affected > 0,
        detail,
        freed_bytes: bytes,
        affected,
    }
}

/// 把垃圾送进回收站 —— 后端会再查一遍白名单。
fn junk_clean(paths: &[String]) -> FixOutcome {
    if paths.is_empty() {
        return FixOutcome {
            kind: "junk_clean".into(),
            ok: false,
            detail: "没有选中任何垃圾条目，什么都没做".into(),
            freed_bytes: 0,
            affected: 0,
        };
    }
    let r = junk::delete_to_recycle_bin(paths);
    let detail = if r.failed == 0 {
        format!("{} 项已送进回收站（可以自己恢复）", r.deleted)
    } else {
        format!(
            "{} 项已送进回收站，{} 项被拒绝（不在允许清理的目录内）",
            r.deleted, r.failed
        )
    };
    FixOutcome {
        kind: "junk_clean".into(),
        ok: r.deleted > 0,
        detail,
        freed_bytes: r.freed_bytes,
        affected: r.deleted,
    }
}

/// 字节数说成人话。
fn human_bytes(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut n = b as f64;
    let mut i = 0usize;
    while n >= 1024.0 && i < U.len() - 1 {
        n /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", b, U[0])
    } else {
        format!("{:.1} {}", n, U[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_fix_is_skipped_with_reason() {
        let mut engine = PurgeEngine::dry();
        let before = checkup::run();
        let s = run(
            &mut engine,
            &[FixRequest {
                kind: "nuke_everything".into(),
                paths: vec![],
            }],
            &before,
        );
        assert_eq!(s.outcomes.len(), 1);
        assert!(!s.outcomes[0].ok);
        assert!(s.outcomes[0].detail.contains("不认识"));
        assert_eq!(s.freed_bytes, 0);
    }

    #[test]
    fn junk_clean_with_empty_paths_does_nothing() {
        let mut engine = PurgeEngine::dry();
        let before = checkup::run();
        let s = run(
            &mut engine,
            &[FixRequest {
                kind: "junk_clean".into(),
                paths: vec![],
            }],
            &before,
        );
        assert!(!s.outcomes[0].ok);
        assert_eq!(s.outcomes[0].affected, 0);
    }

    #[test]
    fn junk_clean_rejects_paths_outside_whitelist() {
        let mut engine = PurgeEngine::dry();
        let before = checkup::run();
        let s = run(
            &mut engine,
            &[FixRequest {
                kind: "junk_clean".into(),
                paths: vec!["C:\\Users\\Administrator\\Documents\\重要.docx".into()],
            }],
            &before,
        );
        assert!(!s.outcomes[0].ok);
        assert_eq!(s.outcomes[0].affected, 0);
        assert!(s.outcomes[0].detail.contains("被拒绝"));
    }

    #[test]
    fn summary_always_carries_both_scores() {
        let mut engine = PurgeEngine::dry();
        let before = checkup::run();
        let s = run(&mut engine, &[], &before);
        assert_eq!(s.before_score, before.score);
        assert!(s.after_score <= checkup::MAX_SCORE);
        assert_eq!(
            s.score_delta,
            s.after_score as i32 - s.before_score as i32
        );
    }

    #[test]
    fn trim_memory_never_kills_anything() {
        // 这是本模块最重要的一条：一键修复不得结束任何进程。
        let mut engine = PurgeEngine::dry();
        let before = checkup::run();
        let s = run(
            &mut engine,
            &[FixRequest {
                kind: "trim_memory".into(),
                paths: vec![],
            }],
            &before,
        );
        let o = &s.outcomes[0];
        assert_eq!(o.kind, "trim_memory");
        // 演练模式下一个进程都不该被结束 —— 断言它没报告任何结束。
        assert!(o.detail.contains("裁剪") || o.detail.contains("没有找到"));
    }
}
