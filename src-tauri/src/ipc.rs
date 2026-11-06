//! Tauri 命令层 —— UI 与核心服务之间唯一的桥。
//!
//! 设计原则：
//! 1. **不做业务判断**。这一层只负责「加锁 → 调用 core → 序列化返回」。
//!    任何安全逻辑都不许写在这里，否则 GUI 和 CLI 会出现两套判定。
//! 2. 全局状态用 `Mutex<PurgeEngine>`，因为采样器与 PDH 句柄必须跨调用存活。

use std::sync::Mutex;

use serde::Serialize;
use tauri::State;

use crate::core::purge::{self, PurgeEngine};
use crate::core::store::{self, Snapshot, Whitelist};
use crate::core::types::{
    BootTimeline, CollectStats, ProcessInfo, PurgeResult, StartupItem, StartupResult,
    StartupScan, SystemMetrics,
};

/// 应用全局状态。
pub struct AppState {
    pub engine: Mutex<PurgeEngine>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            engine: Mutex::new(PurgeEngine::new()),
        }
    }
}

/// 统一返回包装：前端只需判断 `ok`。
#[derive(Serialize)]
pub struct Reply<T> {
    pub ok: bool,
    pub data: Option<T>,
    pub error: Option<String>,
}

impl<T> Reply<T> {
    fn ok(data: T) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }
    fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(msg.into()),
        }
    }
}

/// 扫描结果。
#[derive(Serialize)]
pub struct ScanPayload {
    pub processes: Vec<ProcessInfo>,
    pub metrics: SystemMetrics,
    pub stats: CollectStats,
}

/// 扫描进程列表。
///
/// * `with_paths` —— 首轮传 true（要拿签名与路径），后续高频刷新传 false 提速。
#[tauri::command]
pub fn scan_processes(
    state: State<'_, AppState>,
    with_paths: Option<bool>,
) -> Reply<ScanPayload> {
    let mut e = match state.engine.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(), // 中毒后继续用，系统采集不该因一次 panic 就瘫掉
    };
    let (processes, metrics, stats) = e.scan(with_paths.unwrap_or(true));
    Reply::ok(ScanPayload {
        processes,
        metrics,
        stats,
    })
}

/// 执行净化。
///
/// * `targets` —— `null` / 省略 = 自动模式；数组 = 只处理这些 PID。
#[tauri::command]
pub fn purge_processes(
    state: State<'_, AppState>,
    targets: Option<Vec<u32>>,
    grace_ms: Option<u64>,
) -> Reply<PurgeResult> {
    let mut e = match state.engine.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let r = e.purge(targets, grace_ms.unwrap_or(800));
    Reply::ok(r)
}

/// 净化结果的一句话总结（产品文案）。
#[tauri::command]
pub fn purge_summary(result: PurgeResult) -> Reply<String> {
    Reply::ok(purge::summarize(&result))
}

// ---------------------------------------------------------------------------
// 白名单
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn whitelist_list() -> Reply<Whitelist> {
    Reply::ok(Whitelist::load())
}

#[tauri::command]
pub fn whitelist_add(
    state: State<'_, AppState>,
    name: String,
    path: Option<String>,
) -> Reply<Whitelist> {
    let mut w = Whitelist::load();
    match w.add(&name, path.as_deref()) {
        Ok(()) => {
            // 同步到引擎，下一次扫描立即生效
            if let Ok(mut e) = state.engine.lock() {
                e.whitelist = w.clone();
            }
            Reply::ok(w)
        }
        Err(e) => Reply::err(e),
    }
}

#[tauri::command]
pub fn whitelist_remove(state: State<'_, AppState>, name: String) -> Reply<Whitelist> {
    let mut w = Whitelist::load();
    match w.remove(&name) {
        Ok(()) => {
            if let Ok(mut e) = state.engine.lock() {
                e.whitelist = w.clone();
            }
            Reply::ok(w)
        }
        Err(e) => Reply::err(e),
    }
}

// ---------------------------------------------------------------------------
// 快照 / 回滚
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn snapshot_list() -> Reply<Vec<Snapshot>> {
    Reply::ok(Snapshot::load_all())
}

/// 回滚一次净化。
#[tauri::command]
pub fn snapshot_rollback(id: String) -> Reply<RollbackOutcome> {
    let (ok, fail, errors) = Snapshot::rollback(&id);
    Reply::ok(RollbackOutcome { ok, fail, errors })
}

#[derive(Serialize)]
pub struct RollbackOutcome {
    pub ok: usize,
    pub fail: usize,
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------------------
// 环境信息
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct EnvInfo {
    pub version: String,
    pub cpu_count: u32,
    pub pdh_healthy: bool,
    pub portable: bool,
    pub data_dir: String,
    pub admin: bool,
    pub blacklist_coverage: (usize, usize, usize),
}

#[tauri::command]
pub fn env_info(state: State<'_, AppState>) -> Reply<EnvInfo> {
    let e = match state.engine.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    Reply::ok(EnvInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        cpu_count: e.cpu_count(),
        pdh_healthy: e.perf_healthy(),
        portable: store::is_portable(),
        data_dir: store::data_dir().display().to_string(),
        admin: crate::core::is_elevated(),
        blacklist_coverage: crate::core::blacklist::coverage(),
    })
}

// ---------------------------------------------------------------------------
// P2 · 开机加速
// ---------------------------------------------------------------------------

/// 把「谁拖慢了开机」回填到启动项上。
///
/// 这活儿放在 IPC 层而不是 `core::startup::scan()` 里：`scan()` 的职责是
/// **纯粹的启动项枚举**（CLI、测试都用它，不该被动读开机日志）；而
/// 「这一列只在 UI 显示」正好是 UI 自己的关注点。
///
/// 实测代价：`boot::latest()` 1.2 ms + 回填 1.4 ms ≈ 2.6 ms，
/// 相对快速扫描的 150 ms 只有 1.7%，换来的是一条真实可读的
/// 「拖慢开机 54.1 s」—— 这一列才是用户愿意点禁用的理由。
fn attach_boot_impact(scan: &mut StartupScan) {
    if let Some(rec) = crate::core::boot::latest() {
        crate::core::startup::attach_impact(&mut scan.items, &rec.culprits);
    }
}

/// 解析前端传来的 action 字符串。解析不出来就是 None —— 上层要给用户
/// 明确错误，绝不能默认当成「禁用」。
fn parse_mode(action: Option<&str>) -> Option<crate::core::startup_ctl::Mode> {
    crate::core::startup_ctl::Mode::parse(action.unwrap_or("disable"))
}

/// 开机时间线（分段耗时 + 降级归因 + 历史趋势）。
///
/// `limit` —— 取最近几次开机，默认 8，上限 50。
#[tauri::command]
pub fn boot_timeline(limit: Option<usize>) -> Reply<BootTimeline> {
    Reply::ok(crate::core::boot::timeline(limit.unwrap_or(8)))
}

/// 启动项**快速**扫描（不校验签名）。
///
/// 首屏专用：实测 148 项约 150 ms。结果里 `signature_pending = true` 的条目
/// 还要再调一次 [`startup_verify`] 才有确定的发布者与风险档。
#[tauri::command]
pub fn startup_scan() -> Reply<StartupScan> {
    let mut scan = crate::core::startup::scan();
    attach_boot_impact(&mut scan);
    Reply::ok(scan)
}

/// 补齐启动项的签名与风险分级。
///
/// * `ids` —— 只处理这些条目（`null` / 省略 = 全部）。
///   前端通常只传当前可见、或用户展开过的行，免得为一个折叠起来的
///   200 MB 可执行文件白读一遍。
///
/// 返回更新后的完整列表，省去前端自己做合并。
#[tauri::command]
pub fn startup_verify(ids: Option<Vec<String>>) -> Reply<StartupScan> {
    let mut scan = crate::core::startup::scan();
    let only = ids.map(|v| v.into_iter().collect::<std::collections::HashSet<_>>());
    let updated = crate::core::startup::verify_pending(&mut scan.items, only.as_ref());
    attach_boot_impact(&mut scan);
    // 重新统计一次「可安全禁用候选」——补签可能把条目从谨慎升为可安全禁用
    scan.disable_candidates = crate::core::startup::count_disable_candidates(&scan.items);
    scan.pending_signatures = scan.items.iter().filter(|i| i.signature_pending).count();
    scan.stage_ms.push(("补签".into(), updated as u64));
    Reply::ok(scan)
}

/// 启动项**深度**扫描（含签名校验）。
///
/// 比快速扫描慢 0.7–2 s（要读几百 MB 文件），仅用于 CLI 与后台任务。
/// UI 请走 `startup_scan` + `startup_verify` 两步。
#[tauri::command]
pub fn startup_scan_deep() -> Reply<StartupScan> {
    Reply::ok(crate::core::startup::scan_deep())
}

/// 启动项变更的**演练**结果。
///
/// 只出计划、不改系统，用于让用户在确认前看清「会动什么、怎么还原」。
/// 这是唯一允许出现在确认对话框里的形态 —— 真实变更必须由
/// [`startup_apply`] 发起，且那时用户已经点过确认。
///
/// `action` —— `"disable"`（默认）/ `"enable"` / `"delete"` /
/// `"delay"` / `"undelay"`。这五种是**五种不同的用户意图**，
/// UI 必须按意图分开给按钮，不能揉成一个「优化」。
#[tauri::command]
pub fn startup_plan(ids: Vec<String>, action: Option<String>) -> Reply<StartupResult> {
    let scan = crate::core::startup::scan();
    let want: std::collections::HashSet<String> = ids.into_iter().collect();
    let items: Vec<StartupItem> = scan
        .items
        .into_iter()
        .filter(|i| want.contains(&i.id))
        .collect();
    if items.is_empty() {
        return Reply::err("没有找到对应的启动项（可能已被卸载或已禁用）");
    }
    let Some(mode) = parse_mode(action.as_deref()) else {
        return Reply::err("未知的 action（可用 disable / enable / delete / delay / undelay）");
    };
    // 演练也要装白名单闸：保证「计划」阶段与「执行」阶段的范围完全一致
    let allowed: std::collections::HashSet<String> = items.iter().map(|i| i.id.clone()).collect();
    crate::core::safety::set_startup_allowlist(Some(allowed));
    let plan = crate::core::startup_ctl::apply_with(&items, true, mode);
    crate::core::safety::set_startup_allowlist(None);
    Reply::ok(plan)
}

/// 真正执行启动项变更（禁用 / 启用 / 删除 / 延迟）。
///
/// **安全边界**：本命令不会做任何自动决策 —— `ids` 必须来自用户刚刚看过
/// 的、由 [`startup_plan`] 给出的清单。危险档条目在这一层再被拦一次。
/// 生产构建下受 `OLD666_DRY_RUN=1` 与测试构建的双重熔断保护。
#[tauri::command]
pub fn startup_apply(ids: Vec<String>, action: Option<String>) -> Reply<StartupResult> {
    if crate::core::safety::is_dry_run() {
        return Reply::err("演练模式已开启（OLD666_DRY_RUN=1），真实变更被禁止");
    }
    let scan = crate::core::startup::scan();
    let want: std::collections::HashSet<String> = ids.into_iter().collect();
    let items: Vec<StartupItem> = scan
        .items
        .into_iter()
        .filter(|i| want.contains(&i.id))
        .collect();
    if items.is_empty() {
        return Reply::err("没有找到对应的启动项（可能已被卸载或已禁用）");
    }
    let Some(mode) = parse_mode(action.as_deref()) else {
        return Reply::err("未知的 action（可用 disable / enable / delete / delay / undelay）");
    };
    // 白名单闸只在这一段操作期间存在，改完立刻撤销
    Reply::ok(crate::core::startup_ctl::apply_scoped_with(&items, false, mode))
}

/// 还原一批启动项变更。
#[tauri::command]
pub fn startup_restore(batch_id: String) -> Reply<RollbackOutcome> {
    let (ok, fail, errors) = crate::core::startup_ctl::restore(&batch_id);
    Reply::ok(RollbackOutcome { ok, fail, errors })
}

/// 列出全部变更批次（新的在前），供「变更记录」页面使用。
#[tauri::command]
pub fn startup_batches() -> Reply<Vec<String>> {
    Reply::ok(crate::core::startup_ctl::list_batches())
}

// ---------------------------------------------------------------------------
// P2 · 知识库（「这个自启项到底是干什么的」）
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct KnowledgeItem {
    pub key: String,
    pub title: String,
    pub advice: String,
}

#[derive(Serialize)]
pub struct KnowledgePayload {
    /// 内置库条目数
    pub builtin: usize,
    /// 用户自己补充 / 改写的条目数
    pub user: usize,
    /// 用户库文件路径（外置 JSON，用户可以直接编辑）
    pub user_path: String,
    /// 用户库里都是些什么（设置页列出、可逐条删掉以恢复内置说明）
    pub user_entries: Vec<KnowledgeItem>,
}

fn knowledge_payload() -> KnowledgePayload {
    let (builtin, user) = crate::core::knowledge::counts();
    let user_entries = crate::core::knowledge::user_entries()
        .into_iter()
        .map(|e| KnowledgeItem {
            key: e.keys.first().cloned().unwrap_or_default(),
            title: e.title,
            advice: e.advice,
        })
        .collect();
    KnowledgePayload {
        builtin,
        user,
        user_path: crate::core::knowledge::user_path().display().to_string(),
        user_entries,
    }
}

/// 知识库覆盖情况（设置页展示）。
#[tauri::command]
pub fn knowledge_stats() -> Reply<KnowledgePayload> {
    Reply::ok(knowledge_payload())
}

/// 新增 / 改写一条说明。
///
/// `key` 是匹配串 —— 命中「启动项显示名」或「可执行文件名」都算。
/// 用户库**优先于内置库**，所以「内置库写错了」也能就地纠正，
/// 而且升级程序不会把用户的补充覆盖掉。
#[tauri::command]
pub fn knowledge_save(
    key: String,
    title: String,
    vendor: Option<String>,
    desc: String,
    advice: Option<String>,
) -> Reply<KnowledgePayload> {
    match crate::core::knowledge::upsert_user(
        &key,
        &title,
        vendor.as_deref().unwrap_or(""),
        &desc,
        advice.as_deref().unwrap_or(""),
    ) {
        Ok(()) => Reply::ok(knowledge_payload()),
        Err(e) => Reply::err(e),
    }
}

/// 删掉一条**用户**条目（内置条目删不掉，只能被用户条目覆盖）。
#[tauri::command]
pub fn knowledge_remove(key: String) -> Reply<KnowledgePayload> {
    match crate::core::knowledge::remove_user(&key) {
        Ok(()) => Reply::ok(knowledge_payload()),
        Err(e) => Reply::err(e),
    }
}

// ---------------------------------------------------------------------------
// P2 · 启动项白名单 / 黑名单
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ListsPayload {
    pub whitelist: Vec<String>,
    pub blacklist: Vec<String>,
    pub path: String,
}

fn lists_payload(l: &crate::core::startup_lists::Lists) -> ListsPayload {
    ListsPayload {
        whitelist: l.whitelist.clone(),
        blacklist: l.blacklist.clone(),
        path: crate::core::startup_lists::path().display().to_string(),
    }
}

#[tauri::command]
pub fn startup_lists() -> Reply<ListsPayload> {
    Reply::ok(lists_payload(&crate::core::startup_lists::Lists::load()))
}

/// 加入名单的结果：名单本身 + 「因为加黑而立刻被禁用的条目」。
#[derive(Serialize)]
pub struct ListAddOutcome {
    pub lists: ListsPayload,
    /// 这次因加入黑名单而被**立即禁用**的条目数
    pub disabled: usize,
    /// 没能禁用的条目及原因（权限不足、安全闸拦下…）
    pub failed: Vec<String>,
    /// 加黑时是否处在演练模式（此时只出计划、没真禁用）
    pub dry_run: bool,
}

/// 加入名单。`kind` = `"white"` / `"black"`。
///
/// **黑名单会立即生效**：用户点「加入黑名单」的那一刻就已经表态了
/// （「加入黑名单的不需要过问直接禁用」），所以这里不再弹二次确认，
/// 直接对匹配到的启用条目执行禁用，并把结果如实回报。
///
/// 白名单只是「标记为要留着」——它会影响「一键选中推荐项」的挑选，
/// 但不会去改动系统（用户可能只是想先把它排除在外）。
#[tauri::command]
pub fn startup_list_add(kind: String, name: String) -> Reply<ListAddOutcome> {
    let Some(kind) = crate::core::startup_lists::ListKind::parse(&kind) else {
        return Reply::err("kind 只能是 white / black");
    };
    let mut lists = crate::core::startup_lists::Lists::load();
    if let Err(e) = lists.add(kind, &name) {
        return Reply::err(e);
    }
    if let Err(e) = lists.save() {
        return Reply::err(e);
    }

    let mut out = ListAddOutcome {
        lists: lists_payload(&lists),
        disabled: 0,
        failed: Vec::new(),
        dry_run: crate::core::safety::is_dry_run(),
    };

    if kind == crate::core::startup_lists::ListKind::Black {
        // 重新扫一次：名单标记（in_blacklist）是在扫描里贴上的，
        // 而 apply_with 正是靠这个标记来放行「不建议禁用」的条目
        let scan = crate::core::startup::scan();
        let items: Vec<StartupItem> = scan
            .items
            .into_iter()
            .filter(|i| i.in_blacklist && i.enabled)
            .collect();
        if !items.is_empty() {
            let r = crate::core::startup_ctl::apply_scoped_with(
                &items,
                out.dry_run,
                crate::core::startup_ctl::Mode::Disable,
            );
            out.disabled = r.changes.iter().filter(|c| c.ok).count();
            out.failed = r
                .changes
                .iter()
                .filter(|c| !c.ok)
                .map(|c| format!("{}：{}", c.name, c.error.clone().unwrap_or_default()))
                .collect();
        }
    }

    Reply::ok(out)
}

/// 移出名单。
#[tauri::command]
pub fn startup_list_remove(kind: String, name: String) -> Reply<ListsPayload> {
    let Some(kind) = crate::core::startup_lists::ListKind::parse(&kind) else {
        return Reply::err("kind 只能是 white / black");
    };
    let mut lists = crate::core::startup_lists::Lists::load();
    match lists.remove(kind, &name) {
        Ok(()) => match lists.save() {
            Ok(()) => Reply::ok(lists_payload(&lists)),
            Err(e) => Reply::err(e),
        },
        Err(e) => Reply::err(e),
    }
}

// ===========================================================================
// 测试
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{boot, startup};

    /// P2 的启动项扫描必须把「谁拖慢了开机」回填进来 —— 这一列是用户愿意点
    /// 禁用的唯一理由，曾经只在 CLI 里做，GUI 上永远不显示（前端无头测试抓出来的）。
    #[test]
    fn startup_scan_attaches_boot_impact() {
        let got = startup_scan();
        assert!(got.ok, "startup_scan 必须 ok");
        let items = got.data.expect("ok 时必须带 data").items;

        let mut expect = startup::scan();
        if let Some(rec) = boot::latest() {
            startup::attach_impact(&mut expect.items, &rec.culprits);
        }
        assert_eq!(items.len(), expect.items.len());
        for (a, b) in items.iter().zip(expect.items.iter()) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.impact_ms, b.impact_ms, "{} 的拖慢标注应一致", a.name);
        }
    }

    /// `Reply` 的形状就是前端 `bCall` 解包所依赖的契约：
    /// `ok` 是布尔、`data`/`error` 两个键总在。回滚结果里那个数字 `ok`
    /// 不能被误判成 Reply 的 `ok` —— 前端正是靠 `typeof ok === 'boolean'` 区分的。
    #[test]
    fn reply_envelope_shape_is_stable() {
        let good: Reply<u32> = Reply::ok(7);
        let bad: Reply<u32> = Reply::err("炸了");
        assert!(good.ok && good.error.is_none() && good.data == Some(7));
        assert!(!bad.ok && bad.data.is_none() && bad.error.as_deref() == Some("炸了"));

        let rollback = startup_batches();
        assert!(rollback.ok);
        assert!(rollback.data.is_some());
    }

    /// 空 ids 必须在命令层就被挡掉，而不是空手跑到执行器里。
    #[test]
    fn empty_ids_are_rejected_before_execution() {
        let plan = startup_plan(vec![], None);
        assert!(!plan.ok, "空清单不该给出可执行计划");
        assert!(plan.error.is_some());
        let apply = startup_apply(vec![], None);
        assert!(!apply.ok, "空清单不该触发任何写入");
    }

    /// 开机时间线：有记录时，各阶段必须精确加总到 BootTime
    /// （这条不变式是「归因误差 0%」的根基）。
    #[test]
    fn boot_timeline_phases_sum_exactly() {
        let r = boot_timeline(Some(8));
        let tl = r.data.expect("boot_timeline 必须 ok");
        if !tl.available {
            return; // 没有开机诊断日志的机器上如实跳过
        }
        for rec in &tl.records {
            let sum: u64 = rec.phases.iter().map(|p| p.ms).sum();
            assert_eq!(sum, rec.boot_ms, "阶段之和必须等于总开机耗时");
            let main: u64 = rec
                .phases
                .iter()
                .filter(|p| p.kind == "main")
                .map(|p| p.ms)
                .sum();
            assert_eq!(
                main, rec.main_path_ms,
                "主路径各段之和必须等于 MainPathBootTime"
            );
        }
    }
}
