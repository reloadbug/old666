//! 场景模式的执行器 —— 把规则变成真实的系统动作。
//!
//! 这一层是「规则引擎」与「系统之间」的最后一层，职责很窄：
//!
//! 1. 按规则顺序执行动作；
//! 2. **遇到会终止进程的动作，必须先拿到用户的逐次确认**；
//! 3. 每条动作都产出可读的执行结果（成功 / 失败 / 为什么）。
//!
//! 动作本身的安全性并不在这里重新发明 —— 结束进程仍然走
//! [`crate::core::purge::PurgeEngine`] 的四重兜底，裁剪仍然走
//! [`crate::core::safety::check_trim`]，启动项变更仍然走
//! [`crate::core::startup_ctl::apply_scoped_with`]。
//!
//! 这里的职责是**编排**与**确认**，不是**再判断一次安全**。

use serde::Serialize;

use super::mode_actions;
use super::modes::{Action, Mode, ModePlan};
use super::purge::{PurgeEngine, PurgeScope};
use super::safety;
use super::startup;
use super::startup_ctl;

/// 单条动作的执行结果。
#[derive(Serialize, Clone, Debug)]
pub struct ActionResult {
    /// 规则 id。
    pub rule_id: String,
    /// 动作名。
    pub action: String,
    /// 是否成功。
    pub ok: bool,
    /// 人话结果。
    pub detail: String,
    /// 释放的内存字节（仅裁剪 / 结束时有意义）。
    pub freed_bytes: u64,
    /// 该动作是否被跳过（因为没确认 / 条件不满足 / 出错）。
    pub skipped: bool,
}

/// 一次模式应用的整体结果。
#[derive(Serialize, Clone, Debug)]
pub struct ApplyResult {
    pub mode_id: String,
    pub mode_name: String,
    /// 逐条动作的结果。
    pub actions: Vec<ActionResult>,
    /// 汇总释放的内存字节。
    pub freed_bytes: u64,
    /// 一句话总结。
    pub summary: String,
}

/// 执行参数。
pub struct ApplyOptions {
    /// 只执行这些规则 id（空 = 全部命中的规则）。
    pub rule_ids: Vec<String>,
    /// 用户是否已确认「会终止进程」的动作。
    ///
    /// **这是红线闸**：为 false 时，任何会终止进程的动作一律跳过并说明原因。
    pub kill_confirmed: bool,
    /// 温和关闭等待时长。
    pub grace_ms: u64,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        Self {
            rule_ids: Vec::new(),
            kill_confirmed: false,
            grace_ms: 800,
        }
    }
}

/// 把模式计划落到系统上。
///
/// * `plan` —— 由 [`super::modes::plan`] 生成的编排结果。
/// * `engine` —— 净化引擎（结束 / 裁剪走它）。
///
/// 返回逐条动作的结果。任何一条动作失败都不会中断整体执行 ——
/// 用户要看到的是「哪些做了、哪些没做、为什么」。
pub fn apply(
    mode: &Mode,
    plan: &ModePlan,
    engine: &mut PurgeEngine,
    opts: &ApplyOptions,
) -> ApplyResult {
    let mut results: Vec<ActionResult> = Vec::new();

    for rp in &plan.rules {
        // 条件没命中 → 跳过（不是错误）
        if !rp.triggered {
            results.push(ActionResult {
                rule_id: rp.id.clone(),
                action: rp.action.clone(),
                ok: true,
                detail: format!("条件未满足（{}），未执行", rp.when),
                freed_bytes: 0,
                skipped: true,
            });
            continue;
        }

        // 用户只勾选了部分规则
        if !opts.rule_ids.is_empty() && !opts.rule_ids.iter().any(|x| x == &rp.id) {
            results.push(ActionResult {
                rule_id: rp.id.clone(),
                action: rp.action.clone(),
                ok: true,
                detail: "未被勾选，跳过".into(),
                freed_bytes: 0,
                skipped: true,
            });
            continue;
        }

        // 找到规则本体
        let Some(rule) = mode.rules.iter().find(|r| r.id == rp.id) else {
            continue;
        };

        let mut res = execute_action(&rule.then, engine, opts);
        res.rule_id = rp.id.clone();
        results.push(res);
    }

    let freed: u64 = results.iter().map(|r| r.freed_bytes).sum();
    let done = results.iter().filter(|r| !r.skipped && r.ok).count();
    let failed = results.iter().filter(|r| !r.ok).count();
    let summary = if failed == 0 {
        format!("模式「{}」已应用：{done} 项动作完成，释放 {}", plan.mode_name, human(freed))
    } else {
        format!(
            "模式「{}」已应用：{done} 项完成，{failed} 项失败，释放 {}",
            plan.mode_name,
            human(freed)
        )
    };

    ApplyResult {
        mode_id: mode.id.clone(),
        mode_name: mode.name.clone(),
        actions: results,
        freed_bytes: freed,
        summary,
    }
}

/// 执行单个动作。
fn execute_action(action: &Action, engine: &mut PurgeEngine, opts: &ApplyOptions) -> ActionResult {
    let mk = |ok: bool, detail: String, freed: u64, skipped: bool| ActionResult {
        rule_id: String::new(),
        action: action.name().to_string(),
        ok,
        detail,
        freed_bytes: freed,
        skipped,
    };

    match action {
        Action::PurgeProcesses { scope, trim } => {
            // ---- 红线闸：没有逐次确认，绝不结束任何进程 ----
            if !opts.kill_confirmed {
                return mk(
                    false,
                    "会结束进程的动作需要你逐次确认，本次未执行任何结束操作".into(),
                    0,
                    true,
                );
            }
            if safety::is_dry_run() {
                return mk(false, "演练模式已开启（OLD666_DRY_RUN=1），真实执行被禁止".into(), 0, true);
            }
            let sc = match scope {
                super::modes::Scope::SafeOnly => PurgeScope::SafeOnly,
                super::modes::Scope::SafeAndBackground => PurgeScope::SafeAndBackground,
            };
            let r = engine.purge_scoped(None, opts.grace_ms, sc, *trim, true);
            let killed = r.killed.len();
            let trimmed = r.trimmed.len();
            let freed = r.freed_bytes;
            let errs: Vec<String> = r
                .killed
                .iter()
                .chain(r.trimmed.iter())
                .filter_map(|k| k.error.clone())
                .collect();
            let detail = if errs.is_empty() {
                format!("结束 {killed} 个闲置进程，裁剪 {trimmed} 个，释放 {}", human(freed))
            } else {
                format!(
                    "结束 {killed} 个、裁剪 {trimmed} 个，释放 {}（{} 项被安全闸挡下：{}）",
                    human(freed),
                    errs.len(),
                    errs.iter().take(3).cloned().collect::<Vec<_>>().join("；")
                )
            };
            mk(true, detail, freed, false)
        }

        Action::TrimMemory { .. } => {
            if safety::is_dry_run() {
                return mk(false, "演练模式已开启（OLD666_DRY_RUN=1），真实执行被禁止".into(), 0, true);
            }
            let r = engine.purge_scoped(None, opts.grace_ms, PurgeScope::SafeOnly, true, false);
            let trimmed = r.trimmed.len();
            let freed = r.freed_bytes;
            mk(
                true,
                format!("裁剪 {trimmed} 个进程的工作集，释放 {}", human(freed)),
                freed,
                false,
            )
        }

        Action::SetPowerPlan { plan } => {
            match mode_actions::set_power_plan(plan) {
                Ok(name) => mk(true, format!("电源计划已切换为「{name}」"), 0, false),
                Err(e) => mk(false, e, 0, false),
            }
        }

        Action::BoostForegroundPriority => {
            let Some(pid) = mode_actions::foreground_pid() else {
                return mk(false, "找不到当前前台进程，已跳过".into(), 0, true);
            };
            if let Some(cur) = mode_actions::priority_class(pid) {
                if cur >= mode_actions::PRIORITY_ABOVE_NORMAL {
                    return mk(true, "前台进程已是高优先级，无需调整".into(), 0, false);
                }
            }
            match mode_actions::set_priority_class(pid, mode_actions::PRIORITY_ABOVE_NORMAL) {
                Ok(()) => mk(true, "已把前台进程优先级提升到「高于标准」".into(), 0, false),
                Err(e) => mk(false, e, 0, false),
            }
        }

        Action::SetFocusAssist { on } => match mode_actions::set_focus_assist(*on) {
            Ok(()) => mk(
                true,
                if *on {
                    "已开启系统通知免打扰".into()
                } else {
                    "已关闭系统通知免打扰".into()
                },
                0,
                false,
            ),
            Err(e) => mk(false, e, 0, false),
        },

        Action::DelayStartup { names } => {
            // 只改「延迟自动启动」，且只改自动启动的服务 —— 其他来源会在
            // startup_ctl 里被明确拒绝并说明原因。
            let scan = startup::scan();
            let want: Vec<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
            let items: Vec<super::types::StartupItem> = scan
                .items
                .into_iter()
                .filter(|i| {
                    want.iter().any(|w| {
                        super::startup_lists::norm(w) == super::startup_lists::norm(&i.name)
                    })
                })
                .collect();
            if items.is_empty() {
                return mk(false, "没有匹配到可延迟的启动项".into(), 0, true);
            }
            if safety::is_dry_run() {
                return mk(false, "演练模式已开启（OLD666_DRY_RUN=1），真实变更被禁止".into(), 0, true);
            }
            let r = startup_ctl::apply_scoped_with(
                &items,
                false,
                startup_ctl::Mode::Delayed { on: true },
            );
            let ok = r.changes.iter().filter(|c| c.ok).count();
            let bad = r.changes.iter().filter(|c| !c.ok).count();
            let detail = if bad == 0 {
                format!("{ok} 个启动项已改为延迟启动")
            } else {
                let first = r
                    .changes
                    .iter()
                    .find(|c| !c.ok)
                    .and_then(|c| c.error.clone())
                    .unwrap_or_default();
                format!("{ok} 个已延迟，{bad} 个未处理（{first}）")
            };
            mk(bad == 0, detail, 0, false)
        }

        Action::ProtectCriticalServices => {
            // 这是「收紧可改范围」的策略声明，本身不改系统。
            // 真正生效的地方在 startup_ctl 的风险闸：服务端与关键服务本来
            // 就带保护标记，这里把模式意图显式记进结果，让用户看得见。
            mk(
                true,
                "已启用关键服务保护（服务端与关键启动项保持只读）".into(),
                0,
                false,
            )
        }

        Action::RequireDoubleConfirm => mk(
            true,
            "本模式要求所有变更二次确认".into(),
            0,
            false,
        ),
    }
}

/// 字节数转人话。
fn human(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::modes;

    #[test]
    fn human_formats_bytes() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(12 * 1024 * 1024), "12.0 MB");
        assert_eq!(human(2 * 1024 * 1024 * 1024), "2.00 GB");
    }

    #[test]
    fn purge_action_is_skipped_without_confirmation() {
        // 这是本文件最重要的一条断言：**没有逐次确认就不许结束进程**。
        let mut engine = PurgeEngine::dry();
        let action = Action::PurgeProcesses {
            scope: modes::Scope::SafeOnly,
            trim: false,
        };
        let opts = ApplyOptions {
            kill_confirmed: false,
            ..Default::default()
        };
        let r = execute_action(&action, &mut engine, &opts);
        assert!(r.skipped, "未确认时必须跳过");
        assert!(r.detail.contains("确认"), "实际：{}", r.detail);
        assert_eq!(r.freed_bytes, 0);
    }

    #[test]
    fn non_terminating_actions_are_never_skipped_for_confirmation() {
        // 电源计划这类动作不需要「逐次确认」，否则模式根本没法用。
        let mut engine = PurgeEngine::dry();
        let action = Action::SetPowerPlan {
            plan: "balanced".into(),
        };
        let opts = ApplyOptions {
            kill_confirmed: false,
            ..Default::default()
        };
        let r = execute_action(&action, &mut engine, &opts);
        assert!(!r.skipped, "非终止类动作不该被确认闸拦下");
    }
}
