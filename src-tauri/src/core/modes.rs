//! P3 · 场景模式引擎 —— 规则 DSL 的解析、加载、校验与编排。
//!
//! 设计目标（对应实施计划 P3 验收标准）：
//!
//! * **新增一个模式只需新增规则文件，不改代码** —— 内置模式是 `modes/*.json`，
//!   通过 `include_str!` 编译进 exe；用户自定义模式是 `data/modes/*.json`，
//!   运行时读取。两者走同一套解析与校验。
//! * **可解释** —— 每条规则都带 `explain`（会做什么、不会做什么、为什么安全），
//!   UI 直接展示，不在前端硬编码文案。
//! * **幂等** —— 同一模式重复应用不产生额外副作用；规则执行前先探测当前状态。
//! * **冲突可预期** —— 模式之间通过 `mutually_exclusive_with` 声明互斥；
//!   同一模式内的规则通过 `priority` 排序，低优先级规则让位。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::types::StartupItem;

// ---------------------------------------------------------------------------
// 规则 DSL
// ---------------------------------------------------------------------------

/// 规则的触发条件。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum When {
    /// 无条件执行。
    Always,
    /// 内存占用超过 `pct` 百分比时执行。
    MemUsageAbove { pct: u32 },
}

impl When {
    /// 条件是否命中。
    pub fn matches(&self, mem_usage_pct: u32) -> bool {
        match self {
            When::Always => true,
            When::MemUsageAbove { pct } => mem_usage_pct >= *pct,
        }
    }

    /// 条件的自然语言描述（用于 UI 的"为什么现在没执行"）。
    pub fn describe(&self) -> String {
        match self {
            When::Always => "始终执行".into(),
            When::MemUsageAbove { pct } => format!("内存占用 ≥ {pct}% 时执行"),
        }
    }
}

/// 清理范围。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// 只处理「完全闲置」的目标（P1 的安全口径）。
    #[default]
    SafeOnly,
    /// 闲置目标 + 当前非前台但有窗口的无关程序。
    SafeAndBackground,
}

/// 规则要执行的动作。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// 结束进程。
    PurgeProcesses {
        #[serde(default)]
        scope: Scope,
        #[serde(default)]
        trim: bool,
    },
    /// 裁剪工作集（不终止任何进程）。
    TrimMemory {
        #[serde(default)]
        min_working_set_mb: u64,
    },
    /// 设置电源计划（`balanced` / `high_performance`）。
    SetPowerPlan { plan: String },
    /// 提升当前前台进程优先级。
    BoostForegroundPriority,
    /// 开关系统通知免打扰。
    SetFocusAssist { on: bool },
    /// 把指定名字的启动项改为延迟启动。
    DelayStartup { names: Vec<String> },
    /// 关键服务保护（收紧可改范围）。
    ProtectCriticalServices,
    /// 变更双确认（由 UI 层落实）。
    RequireDoubleConfirm,
}

impl Action {
    /// 动作名（用于留痕与 UI 标签）。
    pub fn name(&self) -> &'static str {
        match self {
            Action::PurgeProcesses { .. } => "purge_processes",
            Action::TrimMemory { .. } => "trim_memory",
            Action::SetPowerPlan { .. } => "set_power_plan",
            Action::BoostForegroundPriority => "boost_foreground_priority",
            Action::SetFocusAssist { .. } => "set_focus_assist",
            Action::DelayStartup { .. } => "delay_startup",
            Action::ProtectCriticalServices => "protect_critical_services",
            Action::RequireDoubleConfirm => "require_double_confirm",
        }
    }

    /// 这个动作是否会终止进程 —— 需要用户逐次确认的判定就靠它。
    pub fn terminates_process(&self) -> bool {
        matches!(self, Action::PurgeProcesses { .. })
    }
}

/// 一条规则：条件 → 动作 + 说明。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Rule {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub desc: String,
    /// 数值越小越先执行。
    #[serde(default = "default_priority")]
    pub priority: i32,
    pub when: When,
    pub then: Action,
    /// 可解释说明：会做什么 / 不会做什么 / 为什么安全。
    #[serde(default)]
    pub explain: String,
}

fn default_priority() -> i32 {
    100
}

/// 模式级设置。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ModeSettings {
    /// 内存阈值（百分比）。`0` 表示不启用内存相关条件。
    #[serde(default)]
    pub mem_threshold_pct: u32,
    /// 目标电源计划。
    #[serde(default)]
    pub power_plan: Option<String>,
    /// 是否所有变更都要二次确认。
    #[serde(default)]
    pub require_double_confirm: bool,
    /// 是否保护关键服务。
    #[serde(default)]
    pub protect_critical_services: bool,
}

/// 一个场景模式。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Mode {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub tagline: String,
    #[serde(default)]
    pub desc: String,
    #[serde(default)]
    pub icon: String,
    /// 内置模式为 true（不可删除），用户自定义为 false。
    #[serde(default)]
    pub builtin: bool,
    /// 互斥模式 id 列表。
    #[serde(default)]
    pub mutually_exclusive_with: Vec<String>,
    #[serde(default)]
    pub settings: ModeSettings,
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// 自定义模式来源文件（内置为 None）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
}

impl Mode {
    /// 校验模式定义是否自洽。返回全部问题，不在第一个错误处停下 —— 用户要一次看到。
    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();
        if self.id.trim().is_empty() {
            errs.push("模式 id 不能为空".into());
        }
        if self.name.trim().is_empty() {
            errs.push("模式名称不能为空".into());
        }
        if self.rules.is_empty() {
            errs.push("模式至少要有一条规则".into());
        }
        let mut seen = BTreeMap::new();
        for r in &self.rules {
            if r.id.trim().is_empty() {
                errs.push("规则 id 不能为空".into());
            }
            if let Some(prev) = seen.insert(r.id.clone(), ()) {
                let _ = prev;
                errs.push(format!("规则 id 重复：{}", r.id));
            }
            if r.title.trim().is_empty() {
                errs.push(format!("规则「{}」缺少标题", r.id));
            }
            if let Action::SetPowerPlan { plan } = &r.then {
                if plan != "balanced" && plan != "high_performance" {
                    errs.push(format!(
                        "规则「{}」的电源计划只能是 balanced / high_performance，实际为 {plan}",
                        r.id
                    ));
                }
            }
            if let Action::DelayStartup { names } = &r.then {
                if names.is_empty() {
                    errs.push(format!("规则「{}」的 delay_startup 没有指定任何名字", r.id));
                }
            }
            if let When::MemUsageAbove { pct } = r.when {
                if pct == 0 || pct > 100 {
                    errs.push(format!("规则「{}」的内存阈值必须在 1..=100，实际为 {pct}", r.id));
                }
            }
        }
        errs
    }

    /// 规则按优先级排序（数值小者先）。
    pub fn sorted_rules(&self) -> Vec<&Rule> {
        let mut v: Vec<&Rule> = self.rules.iter().collect();
        v.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.id.cmp(&b.id)));
        v
    }

    /// 该模式是否会终止进程 —— UI 据此决定是否要弹强确认。
    pub fn terminates_process(&self) -> bool {
        self.rules.iter().any(|r| r.then.terminates_process())
    }
}

// ---------------------------------------------------------------------------
// 加载
// ---------------------------------------------------------------------------

/// 内置模式规则文件（编译进 exe，新增模式只需加文件）。
const BUILTIN: &[(&str, &str)] = &[
    ("daily", include_str!("modes/daily.json")),
    ("game", include_str!("modes/game.json")),
    ("server", include_str!("modes/server.json")),
];

/// 用户自定义模式目录：`data/modes/`。
pub fn user_modes_dir() -> PathBuf {
    super::store::data_dir().join("modes")
}

/// 解析单个模式文件文本。
pub fn parse_mode(text: &str) -> Result<Mode, String> {
    let mut m: Mode = serde_json::from_str(text).map_err(|e| format!("模式文件解析失败：{e}"))?;
    m.builtin = false;
    let errs = m.validate();
    if !errs.is_empty() {
        return Err(errs.join("；"));
    }
    Ok(m)
}

/// 加载全部模式：内置 + 用户自定义。
///
/// 自定义模式 id 与内置冲突时**内置优先**（自定义被忽略），避免用户
/// 意外用同名模式覆盖掉安全边界更严的内置模式。
pub fn load_all() -> Vec<Mode> {
    let mut out: Vec<Mode> = BUILTIN
        .iter()
        .filter_map(|(_, text)| serde_json::from_str::<Mode>(text).ok())
        .collect();

    let dir = user_modes_dir();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let mut files: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|e| e == "json").unwrap_or(false))
            .collect();
        files.sort();
        for p in files {
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let Ok(mut m) = parse_mode(&text) else {
                continue;
            };
            if out.iter().any(|x| x.id == m.id) {
                continue; // 内置优先
            }
            m.source_file = Some(
                p.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
            );
            out.push(m);
        }
    }

    out.sort_by(|a, b| {
        b.builtin
            .cmp(&a.builtin)
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// 按 id 找模式。
pub fn find(id: &str) -> Option<Mode> {
    load_all().into_iter().find(|m| m.id == id)
}

/// 保存自定义模式（写 `data/modes/<id>.json`）。
pub fn save_custom(mode: &Mode) -> Result<(), String> {
    let errs = mode.validate();
    if !errs.is_empty() {
        return Err(errs.join("；"));
    }
    if BUILTIN.iter().any(|(id, _)| *id == mode.id) {
        return Err(format!("「{}」是内置模式，不能覆盖", mode.id));
    }
    let dir = user_modes_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建自定义模式目录失败：{e}"))?;
    let safe: String = mode
        .id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let path = dir.join(format!("{safe}.json"));
    let mut m = mode.clone();
    m.builtin = false;
    m.source_file = None;
    let json = serde_json::to_string_pretty(&m).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("保存自定义模式失败：{e}"))
}

/// 删除自定义模式。
pub fn delete_custom(id: &str) -> Result<(), String> {
    if BUILTIN.iter().any(|(bid, _)| *bid == id) {
        return Err("内置模式不能删除".into());
    }
    let dir = user_modes_dir();
    let path = dir.join(format!("{id}.json"));
    if !path.exists() {
        return Err(format!("没有找到自定义模式「{id}」"));
    }
    std::fs::remove_file(&path).map_err(|e| format!("删除自定义模式失败：{e}"))
}

// ---------------------------------------------------------------------------
// 编排（plan）
// ---------------------------------------------------------------------------

/// 单条规则在本次编排中的状态。
#[derive(Serialize, Clone, Debug)]
pub struct RulePlan {
    pub id: String,
    pub title: String,
    pub desc: String,
    pub explain: String,
    pub action: String,
    pub priority: i32,
    /// 条件是否命中。
    pub triggered: bool,
    /// 条件描述。
    pub when: String,
    /// 动作参数（供 UI 展示与执行器使用）。
    pub params: serde_json::Value,
    /// 动作是否需要二次确认（终止进程 / 系统级变更）。
    pub needs_confirm: bool,
    /// 目标说明（哪些启动项 / 进程），执行前先给用户看清楚。
    pub targets: Vec<String>,
}

/// 一次模式编排的完整结果。
#[derive(Serialize, Clone, Debug)]
pub struct ModePlan {
    pub mode_id: String,
    pub mode_name: String,
    /// 当前内存占用百分比（用于说明条件判定）。
    pub mem_usage_pct: u32,
    /// 是否要求所有变更二次确认。
    pub require_double_confirm: bool,
    /// 是否存在会终止进程的动作。
    pub terminates_process: bool,
    pub rules: Vec<RulePlan>,
    /// 模式之间的互斥提示（切换到本模式会让哪些模式失效）。
    pub excludes: Vec<String>,
}

/// 编排一个模式，得到「会做什么、不做什么」的完整计划。
///
/// `startup_items` 传入当前启动项快照，用于把 `delay_startup` 的目标展开成
/// 具体条目名 —— 用户必须先看到清单，再决定是否执行。
pub fn plan(
    mode: &Mode,
    mem_usage_pct: u32,
    startup_items: &[StartupItem],
) -> ModePlan {
    let mut rules = Vec::new();
    for r in mode.sorted_rules() {
        let triggered = r.when.matches(mem_usage_pct);
        let targets = match &r.then {
            Action::DelayStartup { names } => startup_items
                .iter()
                .filter(|it| names.iter().any(|n| super::startup_lists::norm(n) == super::startup_lists::norm(&it.name)))
                .map(|it| format!("{}（{}）", it.name, it.source.label()))
                .collect(),
            Action::PurgeProcesses { .. } => Vec::new(),
            _ => Vec::new(),
        };
        let params = serde_json::to_value(&r.then).unwrap_or(serde_json::Value::Null);
        rules.push(RulePlan {
            id: r.id.clone(),
            title: r.title.clone(),
            desc: r.desc.clone(),
            explain: r.explain.clone(),
            action: r.then.name().to_string(),
            priority: r.priority,
            triggered,
            when: r.when.describe(),
            params,
            needs_confirm: r.then.terminates_process() || matches!(r.then, Action::DelayStartup { .. }),
            targets,
        });
    }
    ModePlan {
        mode_id: mode.id.clone(),
        mode_name: mode.name.clone(),
        mem_usage_pct,
        require_double_confirm: mode.settings.require_double_confirm,
        terminates_process: mode.terminates_process(),
        rules,
        excludes: mode.mutually_exclusive_with.clone(),
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_all_parse_and_validate() {
        for (id, text) in BUILTIN {
            let m: Mode = serde_json::from_str(text).unwrap_or_else(|e| panic!("{id} 解析失败：{e}"));
            assert_eq!(m.id, *id, "模式 id 与文件名不一致");
            assert!(m.builtin, "{id} 应标记为内置");
            let errs = m.validate();
            assert!(errs.is_empty(), "{id} 校验失败：{errs:?}");
            assert!(!m.rules.is_empty(), "{id} 没有规则");
        }
    }

    #[test]
    fn builtin_ids_are_unique() {
        let mut ids = std::collections::HashSet::new();
        for (id, _) in BUILTIN {
            assert!(ids.insert(*id), "内置模式 id 重复：{id}");
        }
    }

    #[test]
    fn mutual_exclusions_are_symmetric() {
        let modes = load_all();
        for a in &modes {
            for b in &a.mutually_exclusive_with {
                let other = modes.iter().find(|m| &m.id == b);
                assert!(other.is_some(), "{} 的互斥目标 {} 不存在", a.id, b);
                let other = other.unwrap();
                assert!(
                    other.mutually_exclusive_with.contains(&a.id),
                    "互斥声明不对称：{} ↔ {}",
                    a.id,
                    other.id
                );
            }
        }
    }

    #[test]
    fn when_matches_threshold() {
        assert!(When::Always.matches(0));
        assert!(When::Always.matches(99));
        assert!(!When::MemUsageAbove { pct: 80 }.matches(79));
        assert!(When::MemUsageAbove { pct: 80 }.matches(80));
        assert!(When::MemUsageAbove { pct: 80 }.matches(95));
    }

    #[test]
    fn daily_threshold_is_80_game_is_60_server_is_90() {
        let d = find("daily").unwrap();
        let g = find("game").unwrap();
        let s = find("server").unwrap();
        assert_eq!(d.settings.mem_threshold_pct, 80);
        assert_eq!(g.settings.mem_threshold_pct, 60);
        assert_eq!(s.settings.mem_threshold_pct, 90);
    }

    #[test]
    fn server_mode_requires_double_confirm_and_protects_services() {
        let s = find("server").unwrap();
        assert!(s.settings.require_double_confirm);
        assert!(s.settings.protect_critical_services);
        assert!(s.rules.iter().any(|r| matches!(r.then, Action::RequireDoubleConfirm)));
    }

    #[test]
    fn purge_action_is_the_only_one_terminating_processes() {
        let modes = load_all();
        for m in &modes {
            for r in &m.rules {
                let terminating = matches!(r.then, Action::PurgeProcesses { .. });
                assert_eq!(terminating, r.then.terminates_process(), "规则 {}", r.id);
            }
        }
    }

    #[test]
    fn plan_marks_rules_by_threshold() {
        let d = find("daily").unwrap();
        let low = plan(&d, 10, &[]);
        let purge_rule = low.rules.iter().find(|r| r.id == "daily.purge.gentle").unwrap();
        assert!(!purge_rule.triggered, "内存 10% 时不应触发清理");
        // always 条件的规则始终触发
        let power = low.rules.iter().find(|r| r.id == "daily.power.balanced").unwrap();
        assert!(power.triggered);

        let high = plan(&d, 90, &[]);
        let purge_rule = high.rules.iter().find(|r| r.id == "daily.purge.gentle").unwrap();
        assert!(purge_rule.triggered, "内存 90% 时应触发清理");
    }

    #[test]
    fn rules_are_sorted_by_priority() {
        for m in load_all() {
            let ps: Vec<i32> = m.sorted_rules().iter().map(|r| r.priority).collect();
            let mut sorted = ps.clone();
            sorted.sort();
            assert_eq!(ps, sorted, "模式 {} 的规则未按优先级排序", m.id);
        }
    }

    #[test]
    fn validate_rejects_bad_definitions() {
        let bad = Mode {
            id: "".into(),
            name: "".into(),
            tagline: String::new(),
            desc: String::new(),
            icon: String::new(),
            builtin: false,
            mutually_exclusive_with: vec![],
            settings: ModeSettings::default(),
            rules: vec![],
            source_file: None,
        };
        let errs = bad.validate();
        assert!(errs.iter().any(|e| e.contains("id 不能为空")));
        assert!(errs.iter().any(|e| e.contains("名称不能为空")));
        assert!(errs.iter().any(|e| e.contains("至少要有一条规则")));
    }

    #[test]
    fn validate_rejects_duplicate_rule_ids_and_bad_plan() {
        let mut m = find("daily").unwrap();
        m.rules[1].id = m.rules[0].id.clone();
        let errs = m.validate();
        assert!(errs.iter().any(|e| e.contains("规则 id 重复")));

        let mut m2 = find("daily").unwrap();
        for r in m2.rules.iter_mut() {
            if let Action::SetPowerPlan { plan } = &mut r.then {
                *plan = "turbo".into();
            }
        }
        let errs = m2.validate();
        assert!(errs.iter().any(|e| e.contains("电源计划")));
    }

    #[test]
    fn delay_startup_targets_are_expanded_in_plan() {
        let m = find("daily").unwrap();
        let mut it = super::super::types::StartupItem::blank_for_test("OneDrive", "OneDrive.exe");
        it.enabled = true;
        let p = plan(&m, 10, &[it]);
        let r = p.rules.iter().find(|r| r.id == "daily.startup.delay").unwrap();
        assert_eq!(r.targets.len(), 1);
        assert!(r.targets[0].contains("OneDrive"));
    }

    #[test]
    fn parse_mode_rejects_invalid_json() {
        assert!(parse_mode("{not json").is_err());
        assert!(parse_mode("{\"id\":\"x\"}").is_err()); // 缺字段
    }
}
