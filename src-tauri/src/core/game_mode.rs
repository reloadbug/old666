//! 游戏模式：游戏在跑的时候，先把「抢资源的启动项」让开；游戏退出，再把它们请回来。
//!
//! ## 为什么要有这个
//!
//! 有些程序开机自启之后就一直常驻，吃 CPU、吃内存、还在后台做磁盘 IO。
//! 平时无所谓，打游戏的时候这些就是帧数的敌人。用户要的是：**打游戏时别碍事，
//! 打完游戏该有的照旧**。
//!
//! 所以这里做两件事，缺一不可：
//!
//! 1. **让开** —— 游戏进程出现时，把匹配到的启动项禁用（走 `startup_ctl` 的安全闸）；
//! 2. **请回来** —— 游戏进程消失后，把**同一批**启动项恢复原样。
//!
//! ## 靠批次还原，而不是靠「记住我关了什么」
//!
//! 还原用的是 `startup_ctl::restore(batch)` —— 也就是 P2 那个「一键还原」
//! 用的同一套回滚数据（`StartupApproved` 原值、服务原启动类型、注册表原值…）。
//! 这样还原的语义是「回到我动手之前的样子」，而不是「重新启用」——
//! 后者会把一个原本就被软件自己关掉的条目错误地打开。
//!
//! ## 安全边界
//!
//! * **白名单优先**：加白了的一律不动（`startup_ctl` 的白名单硬闸先于一切）。
//! * **不碰危险档**：`risk == Dangerous` 或 `can_disable == false` 的条目一律跳过 ——
//!   「影响游戏性能」是我们自己的判断，不能盖过「禁用它可能让系统出问题」。
//! * **不结束任何进程**：这个模块只改启动项（注册表 / 服务 / 计划任务），
//!   一个进程都不杀。
//! * **演练优先**：演练模式（`safety::is_dry_run()`）下只出计划，不真改。
//!
//! ## 判定「游戏在跑」靠 exe 名
//!
//! 用**可执行文件名**（小写、去 `.exe`）匹配，跟 `startup_lists` 的口径一致。
//! 游戏主程序换目录不换名字，而显示名经常带版本号后缀，靠名字匹配反而不稳。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::core::store;
use crate::core::types::StartupItem;

/// 轮询间隔（毫秒）。游戏进出的判定不需要更细，5 秒足够，
/// 而更细只会白白多扫一遍进程表。
pub const POLL_MS: u64 = 5000;

/// 游戏模式配置。
///
/// 用户可改：`data/game_mode.json`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameModeConfig {
    /// 总开关。关掉时线程不启动、也不做任何事。
    #[serde(default)]
    pub enabled: bool,
    /// 游戏进程的 exe 名（小写、去 `.exe`），如 `steam`、`valorant`、`cs2`。
    #[serde(default)]
    pub games: Vec<String>,
    /// 游戏期间要「让开」的启动项匹配串（同样小写去 `.exe`）。
    /// 既可以写显示名，也可以写 exe 名。
    #[serde(default)]
    pub targets: Vec<String>,
    /// 游戏退出后是否自动恢复这些启动项。
    #[serde(default = "default_true")]
    pub auto_restore: bool,
}

fn default_true() -> bool {
    true
}

impl Default for GameModeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            games: Vec::new(),
            targets: Vec::new(),
            auto_restore: true,
        }
    }
}

impl GameModeConfig {
    /// 校验并归一化：匹配串统一小写去 `.exe`，空的剔除。
    pub fn sanitized(mut self) -> Self {
        self.games = norm_all(self.games);
        self.targets = norm_all(self.targets);
        self
    }

    /// 配置能不能用 —— 至少要有一个游戏、一个目标。
    pub fn is_usable(&self) -> bool {
        !self.games.is_empty() && !self.targets.is_empty()
    }
}

fn norm_all(mut v: Vec<String>) -> Vec<String> {
    v = v.into_iter().map(|s| norm(&s)).filter(|s| !s.is_empty()).collect();
    v.sort();
    v.dedup();
    v
}

/// 归一化：小写、去 `.exe`、去首尾空白。
pub fn norm(name: &str) -> String {
    let n = name.trim().to_ascii_lowercase();
    let n = n.strip_suffix(".exe").unwrap_or(&n);
    n.trim().to_string()
}

/// 配置文件路径。
pub fn config_path() -> PathBuf {
    store::data_dir().join("game_mode.json")
}

/// 读配置。文件不存在或坏了都退回默认（关闭）。
pub fn load_config() -> GameModeConfig {
    let Ok(text) = std::fs::read_to_string(config_path()) else {
        return GameModeConfig::default();
    };
    serde_json::from_str::<GameModeConfig>(&text)
        .unwrap_or_default()
        .sanitized()
}

/// 写配置。
pub fn save_config(cfg: &GameModeConfig) -> Result<(), String> {
    let p = config_path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    let text = serde_json::to_string_pretty(cfg).map_err(|e| format!("序列化失败：{e}"))?;
    std::fs::write(&p, text).map_err(|e| format!("写入配置失败：{e}"))
}

/// 运行状态（给 UI 看）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GameModeState {
    /// 当前是否处于「游戏进行中」
    pub in_game: bool,
    /// 当前匹配到的游戏 exe 名
    pub game: Option<String>,
    /// 本次让开的批次 ID（用来还原）
    pub batch_id: Option<String>,
    /// 本次让开了哪些条目
    pub disabled: Vec<String>,
    /// 最近一次检查时间（本地时间戳，秒）
    pub checked_at: u64,
}

/// 一次「让开」的结果。
#[derive(Debug, Clone, Default, Serialize)]
pub struct DisableOutcome {
    pub batch_id: Option<String>,
    /// 真正被禁用的条目名
    pub disabled: Vec<String>,
    /// 被跳过的条目及原因
    pub skipped: Vec<String>,
    pub dry_run: bool,
}

/// 挑出「游戏期间要让开」的条目 —— **纯决策，不碰系统**。
///
/// 跳过条件（每一条都要对用户说清楚）：
/// * 已经禁用了 —— 没得让
/// * 在白名单里 —— 用户说了要留着
/// * `risk == Dangerous` 或 `can_disable == false` —— 禁用它可能让系统出问题，
///   这比「影响游戏性能」严重
/// * 不在 targets 里
pub fn pick_targets(items: &[StartupItem], cfg: &GameModeConfig) -> Vec<StartupItem> {
    items
        .iter()
        .filter(|it| {
            if !it.enabled {
                return false;
            }
            if it.in_whitelist {
                return false;
            }
            if it.risk == crate::core::types::RiskLevel::Dangerous || !it.can_disable {
                return false;
            }
            matches_target(it, &cfg.targets)
        })
        .cloned()
        .collect()
}

/// 这条启动项是否命中 targets（显示名或 exe 名都算）。
pub fn matches_target(it: &StartupItem, targets: &[String]) -> bool {
    let name = norm(&it.name);
    let exe = crate::core::knowledge::exe_base(&it.command)
        .map(|b| norm(&b))
        .unwrap_or_default();
    targets.iter().any(|t| {
        !t.is_empty() && (name == *t || (!exe.is_empty() && exe == *t))
    })
}

/// 判断某个进程是不是「游戏」。用 exe 名匹配。
pub fn is_game_process(exe_name: &str, games: &[String]) -> bool {
    let n = norm(exe_name);
    if n.is_empty() {
        return false;
    }
    games.iter().any(|g| !g.is_empty() && *g == n)
}

// ===========================================================================
// 常驻执行器
// ===========================================================================

/// 游戏模式执行器。后台线程按固定间隔问两个问题：
/// 「游戏在跑吗？」「游戏刚退了吗？」
///
/// 状态机很简单，靠 `in_game` 翻转：
///
/// ```text
///  游戏不在  ──检测到游戏──▶  让开（禁用 + 记批次）
///     ▲                          │
///     └──────检测到游戏退出──────┘  还原（restore 批次）
/// ```
pub struct GameModeHub {
    cfg: Mutex<GameModeConfig>,
    state: Mutex<GameModeState>,
    running: AtomicBool,
    stop: AtomicBool,
}

impl GameModeHub {
    pub fn new() -> Self {
        Self {
            cfg: Mutex::new(load_config()),
            state: Mutex::new(GameModeState::default()),
            running: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        }
    }

    pub fn config(&self) -> GameModeConfig {
        self.cfg.lock().map(|c| c.clone()).unwrap_or_default()
    }

    pub fn set_config(&self, cfg: GameModeConfig) -> GameModeConfig {
        let cfg = cfg.sanitized();
        if let Ok(mut c) = self.cfg.lock() {
            *c = cfg.clone();
        }
        cfg
    }

    pub fn state(&self) -> GameModeState {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// 启动后台线程。幂等。
    pub fn start(self: &Arc<Self>) {
        if self.running.swap(true, Ordering::Relaxed) {
            return;
        }
        self.stop.store(false, Ordering::Relaxed);
        let hub = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name("old666-gamemode".into())
            .stack_size(256 * 1024)
            .spawn(move || hub.run_loop());
    }

    /// 停止后台线程。
    pub fn stop_monitor(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.running.store(false, Ordering::Relaxed);
    }

    fn run_loop(self: Arc<Self>) {
        while !self.stop.load(Ordering::Relaxed) {
            let cfg = self.config();
            if cfg.enabled && cfg.is_usable() {
                let _ = self.tick(&cfg);
            }
            std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        }
        self.running.store(false, Ordering::Relaxed);
    }

    /// 走一步状态机。返回 `true` 表示这一步做了动作（让开或还原）。
    pub fn tick(&self, cfg: &GameModeConfig) -> bool {
        let game = self.detect_game(cfg);
        let in_game = game.is_some();
        let mut acted = false;

        {
            let st = match self.state.lock() {
                Ok(s) => s,
                Err(_) => return false,
            };
            if in_game && !st.in_game {
                // 游戏刚开 → 让开
                drop(st);
                let outcome = self.disable_for_game(cfg);
                if let Ok(mut st) = self.state.lock() {
                    st.in_game = true;
                    st.game = game.clone();
                    st.batch_id = outcome.batch_id.clone();
                    st.disabled = outcome.disabled.clone();
                    st.checked_at = now_secs();
                }
                acted = true;
            } else if !in_game && st.in_game {
                // 游戏刚退 → 请回来
                let batch = st.batch_id.clone();
                drop(st);
                if cfg.auto_restore {
                    if let Some(b) = batch.as_deref() {
                        crate::core::startup_ctl::restore(b);
                    }
                }
                if let Ok(mut st) = self.state.lock() {
                    st.in_game = false;
                    st.game = None;
                    st.batch_id = None;
                    st.disabled.clear();
                    st.checked_at = now_secs();
                }
                acted = true;
            } else if let Ok(mut st) = self.state.lock() {
                st.checked_at = now_secs();
            }
        }
        let _ = game;
        acted
    }

    /// 找出当前在跑的游戏 exe 名。
    pub fn detect_game(&self, cfg: &GameModeConfig) -> Option<String> {
        if cfg.games.is_empty() {
            return None;
        }
        let (procs, _) = crate::core::process::enumerate();
        for p in procs.iter() {
            if is_game_process(&p.name, &cfg.games) {
                return Some(norm(&p.name));
            }
        }
        None
    }

    /// 让开：禁用匹配到的启动项，并记下批次以便还原。
    pub fn disable_for_game(&self, cfg: &GameModeConfig) -> DisableOutcome {
        let mut out = DisableOutcome {
            dry_run: crate::core::safety::is_dry_run(),
            ..Default::default()
        };
        let scan = crate::core::startup::scan();
        let targets = pick_targets(&scan.items, cfg);
        if targets.is_empty() {
            return out;
        }
        let r = crate::core::startup_ctl::apply_scoped_with(
            &targets,
            out.dry_run,
            crate::core::startup_ctl::Mode::Disable,
        );
        for c in r.changes.iter() {
            if c.ok {
                out.disabled.push(c.name.clone());
            } else {
                out.skipped.push(format!(
                    "{}：{}",
                    c.name,
                    c.error.clone().unwrap_or_else(|| "未知原因".into())
                ));
            }
        }
        out.batch_id = Some(r.batch_id);
        out
    }
}

impl Default for GameModeHub {
    fn default() -> Self {
        Self::new()
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::{RiskLevel, StartupSource};

    fn item(name: &str, command: &str) -> StartupItem {
        let mut it = StartupItem::blank_for_test(name, command);
        it.source = StartupSource::Service;
        it.risk = RiskLevel::Safe;
        it.can_disable = true;
        it
    }

    #[test]
    fn norm_strips_exe_and_case() {
        assert_eq!(norm("Steam.EXE"), "steam");
        assert_eq!(norm("  Valorant  "), "valorant");
        assert_eq!(norm(""), "");
    }

    #[test]
    fn config_sanitizes_and_dedups() {
        let cfg = GameModeConfig {
            enabled: true,
            games: vec!["Steam.exe".into(), "steam".into(), " ".into()],
            targets: vec!["OneDrive.EXE".into()],
            auto_restore: true,
        }
        .sanitized();
        assert_eq!(cfg.games, vec!["steam".to_string()]);
        assert_eq!(cfg.targets, vec!["onedrive".to_string()]);
        assert!(cfg.is_usable());
    }

    #[test]
    fn config_without_games_or_targets_is_not_usable() {
        let cfg = GameModeConfig {
            enabled: true,
            games: vec![],
            targets: vec!["onedrive".into()],
            auto_restore: true,
        };
        assert!(!cfg.is_usable());
    }

    /// 白名单必须赢 —— 用户说「这个我要留着」，游戏模式也不能动它。
    #[test]
    fn whitelisted_items_are_never_picked() {
        let mut it = item("OneDrive", r#""C:\Program Files\OneDrive\OneDrive.exe""#);
        it.in_whitelist = true;
        let cfg = GameModeConfig {
            enabled: true,
            games: vec!["steam".into()],
            targets: vec!["onedrive".into()],
            auto_restore: true,
        };
        assert!(pick_targets(&[it], &cfg).is_empty());
    }

    /// 危险项不许被游戏模式动 —— 「影响游戏性能」盖不过「禁用可能出事」。
    #[test]
    fn dangerous_items_are_skipped() {
        let mut it = item("CoreDriver", r#""C:\Windows\System32\CoreDriver.sys""#);
        it.risk = RiskLevel::Dangerous;
        it.can_disable = false;
        let cfg = GameModeConfig {
            enabled: true,
            games: vec!["steam".into()],
            targets: vec!["coredriver".into()],
            auto_restore: true,
        };
        assert!(pick_targets(&[it], &cfg).is_empty());
    }

    #[test]
    fn targets_match_display_name_or_exe_name() {
        let it = item(
            "Microsoft OneDrive",
            r#""C:\Program Files\Microsoft OneDrive\OneDrive.exe" /background"#,
        );
        let cfg = GameModeConfig {
            enabled: true,
            games: vec!["steam".into()],
            targets: vec!["onedrive".into()],
            auto_restore: true,
        };
        assert_eq!(pick_targets(&[it], &cfg).len(), 1);
    }

    #[test]
    fn disabled_items_are_not_picked() {
        let mut it = item("OneDrive", r#""C:\Program Files\OneDrive\OneDrive.exe""#);
        it.enabled = false;
        let cfg = GameModeConfig {
            enabled: true,
            games: vec!["steam".into()],
            targets: vec!["onedrive".into()],
            auto_restore: true,
        };
        assert!(pick_targets(&[it], &cfg).is_empty());
    }

    #[test]
    fn game_process_matching_is_case_and_suffix_insensitive() {
        assert!(is_game_process("Steam.exe", &["steam".to_string()]));
        assert!(is_game_process("STEAM", &["steam".to_string()]));
        assert!(!is_game_process("steamwebhelper", &["steam".to_string()]));
        assert!(!is_game_process("", &["steam".to_string()]));
    }
}
