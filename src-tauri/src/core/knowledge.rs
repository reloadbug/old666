//! 启动项知识库 —— 回答「这个开机自启项到底是干什么的」。
//!
//! 分两层：
//!
//! * **内置库**（`knowledge/builtin.json`，用 `include_str!` 编进二进制）——
//!   覆盖系统组件、常见国产/国际软件、硬件厂商驱动套件、更新器与推广组件。
//!   随程序升级而更新，用户改不动。
//! * **用户库**（`data/knowledge/startup.json`，外置可编辑）—— 用户自己写的说明，
//!   **优先级高于内置库**。这样「内置库里写错/没收录的东西」用户能就地纠正，
//!   而且升级程序不会把用户的补充覆盖掉。
//!
//! 匹配用的是**词边界包含**而不是裸 `contains`：内置库里有 `node`、`w32time`、
//! `qq` 这类短词，裸包含会让 `RuntimeBroker` 命中 `time`、`nodejs-helper` 命中 `node`。
//! 词边界要求命中片段的前后都不能是字母数字，`runtime` 里就不会再冒出 `time`。

use std::path::PathBuf;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::core::store;
use crate::core::types::{Knowledge, RiskLevel, SignatureStatus, StartupItem, StartupSource};

/// 内置知识库（编译进二进制，共 140+ 条）
const BUILTIN_JSON: &str = include_str!("knowledge/builtin.json");

static BUILTIN: OnceLock<Vec<RawEntry>> = OnceLock::new();

/// 知识库条目的原始形态。内置库与用户库用同一套结构，
/// 用户可以直接照抄内置库的写法增删条目。
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct RawEntry {
    /// 匹配串（小写）。命中「启动项显示名」或「可执行文件名」中任意一条即可。
    pub keys: Vec<String>,
    pub title: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub desc: String,
    #[serde(default)]
    pub advice: String,
    /// 是不是「只看名字猜出来」的兜底条目（`updater` / `helper` / `push` 这类）。
    ///
    /// 这类条目的 key 很宽，能命中一大堆真实存在的程序。它们只该在
    /// **没有任何具体条目命中**时才生效，而且建议必须保守 —— 一个叫
    /// `xxxlauncher` 的程序到底是游戏启动器还是厂商推广位，光看名字分不出来，
    /// 说「可以禁用」是不负责任的。
    #[serde(default)]
    pub generic: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct UserFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    entries: Vec<RawEntry>,
}

/// 内置库（首次访问时解析一次，之后复用）。
pub fn builtin() -> &'static [RawEntry] {
    BUILTIN.get_or_init(|| serde_json::from_str(BUILTIN_JSON).unwrap_or_default())
}

/// 用户库文件路径。绿色便携：跟着 `data/` 走。
pub fn user_path() -> PathBuf {
    store::data_dir().join("knowledge").join("startup.json")
}

/// 读用户库。文件不存在或格式坏了都返回空 ——
/// 知识库是「锦上添花」，绝不该因为它读不了就让整页扫描失败。
pub fn user_entries() -> Vec<RawEntry> {
    let Ok(text) = std::fs::read_to_string(user_path()) else {
        return Vec::new();
    };
    serde_json::from_str::<UserFile>(&text)
        .map(|f| f.entries)
        .unwrap_or_default()
}

/// 写用户库（整文件覆盖）。
fn save_user(entries: &[RawEntry]) -> Result<(), String> {
    let path = user_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建知识库目录失败：{e}"))?;
    }
    let file = UserFile {
        version: 1,
        entries: entries.to_vec(),
    };
    let text = serde_json::to_string_pretty(&file).map_err(|e| format!("序列化失败：{e}"))?;
    std::fs::write(&path, text).map_err(|e| format!("写入知识库失败：{e}"))
}

/// 新增或覆盖一条用户条目。
///
/// `key` 是匹配串；若用户库里已有同 key 的条目则就地更新，
/// 否则新增。空 key 直接拒绝 —— 否则会命中一切。
pub fn upsert_user(
    key: &str,
    title: &str,
    vendor: &str,
    desc: &str,
    advice: &str,
) -> Result<(), String> {
    let key = normalize_key(key);
    if key.is_empty() {
        return Err("匹配串不能为空".into());
    }
    if title.trim().is_empty() {
        return Err("标题不能为空".into());
    }
    let mut entries = user_entries();
    match entries.iter_mut().find(|e| {
        e.keys
            .iter()
            .any(|k| normalize_key(k) == key)
    }) {
        Some(e) => {
            e.keys = vec![key.clone()];
            e.title = title.trim().to_string();
            e.vendor = vendor.trim().to_string();
            e.desc = desc.trim().to_string();
            e.advice = advice.trim().to_string();
            // 用户亲手写的说明永远不是「兜底推断」，建议照用户填的用
            e.generic = false;
        }
        None => entries.push(RawEntry {
            keys: vec![key.clone()],
            title: title.trim().to_string(),
            vendor: vendor.trim().to_string(),
            desc: desc.trim().to_string(),
            advice: advice.trim().to_string(),
            generic: false,
        }),
    }
    save_user(&entries)
}

/// 删除一条用户条目（内置条目删不掉，只能被用户条目覆盖）。
pub fn remove_user(key: &str) -> Result<(), String> {
    let key = normalize_key(key);
    let mut entries = user_entries();
    let before = entries.len();
    entries.retain(|e| !e.keys.iter().any(|k| normalize_key(k) == key));
    if entries.len() == before {
        return Err(format!("用户库里没有「{key}」这条"));
    }
    save_user(&entries)
}

/// 按名称 + 命令行查一条说明。
///
/// 用户库优先；同一优先级下**匹配串更长者胜**（更具体的描述赢过通用兜底，
/// 例如 `nvbackend` 赢过 `updater`）。
pub fn lookup(name: &str, command: &str, source: StartupSource) -> Option<Knowledge> {
    lookup_with(name, command, source, &user_entries())
}

/// 给一批启动项填上说明。**只读一次用户库文件** —— 148 项逐条查会白读 148 次磁盘。
pub fn apply_all(items: &mut [StartupItem]) {
    let user = user_entries();
    for it in items.iter_mut() {
        it.knowledge = lookup_with(it.name.as_str(), it.command.as_str(), it.source, &user);
        reconcile_risk(it);
    }
}

/// 用知识库的建议去**校正**风险档，消除界面上「黄/红标」和「说明面板」互相打架。
///
/// 为什么需要这一步：风险档来自关键词规则链（`startup_risk`），说明来自知识库，
/// 两者本来是各算各的。实测本机 142 条启动项里有 6 条结论相反 ——
/// 最难看的一条是「诊断策略服务」：面板里写着「可以禁用」，
/// 列表上却是红色的「不建议禁用」，用户一眼就看出程序在胡说。
///
/// 定下来的优先级是：
///
/// 1. **硬底线**（安全软件、内核态驱动）—— 知识库也不许改写，写错了就是裸奔；
/// 2. **知识库的具体条目** —— 人工维护，且用户看得见、改得动，
///    所以它压过关键词规则；用户自己补一条「可以禁用」，标就真的变绿；
/// 3. 其余情况保留规则链的结论。
///
/// 注意「可以禁用」和「建议保留」都不带兜底条目 —— 兜底条目（`generic`）的建议
/// 在 `search` 里已经被强制收敛成「谨慎处理」，不会在这里给出硬结论。
fn reconcile_risk(it: &mut StartupItem) {
    // ① 硬底线优先，且**必须**把理由也换掉：
    //    否则会出现「标是红的、理由是『可以安全禁用』」这种更荒唐的组合。
    if let Some(floor) = crate::core::startup_risk::hard_floor(&it.name, &it.command, it.source) {
        it.can_disable = floor.can_disable();
        it.risk = floor.risk;
        it.risk_reason = format!("{}{}", it.reason_prefix, floor.reason);
        return;
    }
    // ② 知识库的具体建议
    let Some(kb) = it.knowledge.as_ref() else {
        return;
    };
    // 兜底条目（keys 是 `helper` / `assistant` / `updater` 这种）**只负责说明，
    // 绝不改写风险档**。它们连自己都只是「名字像」，建议在 `search` 里已经被
    // 强制收敛成「谨慎处理」；拿它去改写别人算出来的结论只会带来误判 ——
    // 实测 Windows 自带的「IP Helper」就是被「厂商助手 / 预装组件」认领的，
    // 顺带把「位于系统目录的自动启动服务」这条保守判定降级成了黄标。
    if kb.generic {
        return;
    }
    let Some((mut risk, mut tail)) = risk_from_advice(&kb.advice) else {
        return; // 「暂无建议」之类没结论的，留给规则链
    };
    // 名字是可以随便起的：一个把自己命名为 `GoogleUpdate.exe` 的未签名程序，
    // 绝不能因为知识库按名字命中就被判成「可以安全禁用」。
    if risk == RiskLevel::Safe && matches!(it.signature, SignatureStatus::Invalid) {
        risk = RiskLevel::Caution;
        tail = "可以禁用，但文件签名无效，请先确认来源";
    }
    let who = if kb.origin == "user" {
        "你补充的说明"
    } else {
        "内置说明"
    };
    it.risk = risk;
    // 前缀（「延迟启动，对开机影响较小；」「登录触发；」）必须留着 ——
    // 它跟风险档无关，是用户判断「关掉会不会更快」的另一条线索
    it.risk_reason = format!("{}{who}：{}，{tail}", it.reason_prefix, kb.title);
    it.can_disable = risk != RiskLevel::Dangerous;
}

/// 知识库建议 → 风险档。返回 `None` 表示这条建议不足以定档。
fn risk_from_advice(advice: &str) -> Option<(RiskLevel, &'static str)> {
    match advice.trim() {
        "可以禁用" => Some((RiskLevel::Safe, "可以安全禁用")),
        "谨慎处理" => Some((RiskLevel::Caution, "可以禁用，但会影响对应功能，请自行判断")),
        "建议保留" => Some((RiskLevel::Dangerous, "建议保留，禁用可能影响系统或软件的正常使用")),
        _ => None,
    }
}

/// 真正的查表，分三级：
///
/// 1. **显示名 + 可执行名**，只看**具体条目**；
/// 2. 一级落空时，拿**路径里的厂商/产品目录名**（`path_hint`）再查一次，仍只看具体条目；
/// 3. 前两级都没有具体条目命中，才轮到**兜底条目**（`generic`，建议会被收敛为「谨慎处理」）。
///
/// 分三级而不是把线索拼成一段，是为了让下一级的线索**不可能抢占**上一级的正确结果 ——
/// 路径里出现 `tencent` 是常有的事，拼进去就可能把一条本该命中「微信」的启动项
/// 引到泛泛的「腾讯」条目上。同理，兜底条目排在最后一级：它的 key 太宽
/// （`updater` / `helper`），一旦参与前面几级，就会把「Google 更新程序」
/// 这种本该被路径线索认出来的条目挡在外面。
///
/// 级内排序键是三元组 **(库, 匹配串长度)**：
///
/// 1. 用户库永远压过内置库 —— 用户写的就是用户要的；
/// 2. 同级下匹配串更长者胜（`nvbackend` 赢过 `update`）。
///
/// 把「命中哪一条」也一并带出来（`Knowledge::key`）——「恢复内置说明」要靠它
/// 精确删掉用户那一条，而不是让用户自己去设置页里对号入座。
fn lookup_with(
    name: &str,
    command: &str,
    _source: StartupSource,
    user: &[RawEntry],
) -> Option<Knowledge> {
    let hay = haystack(name, command);
    if let Some(k) = search(&hay, user, false) {
        return Some(k);
    }
    let hint = path_hint(command);
    if !hint.is_empty() {
        if let Some(k) = search(&hint, user, false) {
            return Some(k);
        }
    }
    search(&hay, user, true).or_else(|| {
        if hint.is_empty() {
            None
        } else {
            search(&hint, user, true)
        }
    })
}

/// 在一段搜索文本里找最合适的条目。
///
/// `allow_generic` 为 false 时跳过兜底条目 —— 让调用方能把「先用具体条目认人、
/// 实在认不出才动用兜底」这件事分层表达出来。排序规则见 [`lookup_with`]。
fn search(hay: &str, user: &[RawEntry], allow_generic: bool) -> Option<Knowledge> {
    if hay.trim().is_empty() {
        return None;
    }
    // (库优先级, 是否兜底, 匹配串长度, 条目, 出处, 命中的匹配串)
    let mut best: Option<(u8, bool, usize, &RawEntry, &'static str, String)> = None;
    for (origin_prio, origin, list) in [(0u8, "user", user), (1u8, "builtin", builtin())] {
        for e in list {
            if e.generic && !allow_generic {
                continue;
            }
            for raw_key in &e.keys {
                let key = normalize_key(raw_key);
                if key.is_empty() || !contains_word(hay, &key) {
                    continue;
                }
                // 库优先级更小者胜；同级下匹配串更长者胜。
                let better = match &best {
                    None => true,
                    Some((bp, _, bs, _, _, _)) => {
                        origin_prio < *bp || (origin_prio == *bp && key.len() > *bs)
                    }
                };
                if better {
                    best = Some((origin_prio, e.generic, key.len(), e, origin, key));
                }
            }
        }
    }
    best.map(|(_, generic, _, e, origin, key)| Knowledge {
        title: e.title.clone(),
        vendor: e.vendor.clone(),
        desc: e.desc.clone(),
        advice: if generic {
            // 兜底条目：不管库里写了什么，都不给「可以禁用」这种硬结论
            "谨慎处理".into()
        } else if e.advice.trim().is_empty() {
            "暂无建议".into()
        } else {
            e.advice.clone()
        },
        origin: origin.to_string(),
        key,
        generic,
    })
}

/// 内置 / 用户 条目数（设置页展示覆盖情况）。
pub fn counts() -> (usize, usize) {
    (builtin().len(), user_entries().len())
}

// ---------------------------------------------------------------------------
// 匹配
// ---------------------------------------------------------------------------

/// 搜索文本 = 「启动项显示名」+「命令行里那个可执行文件名」，全小写。
///
/// 刻意**不把整条命令行**塞进来：路径里的 `C:\Windows\System32` 会让
/// `windows`、`system` 这类词命中一大堆无关条目。
///
/// 去重这里必须用 `contains_word` 而不是 `contains`：任务名 `\OllamaServer`
/// 里确实含有 `ollama` 这几个字母，但后面紧跟 `S` —— 词边界判定它**不命中**。
/// 若用 `contains` 判定「已经有了、不必再补」，可执行名 `ollama` 就永远补不进来，
/// 这条启动项便白白漏掉说明（真实踩过的坑）。
fn haystack(name: &str, command: &str) -> String {
    let mut s = name.to_ascii_lowercase();
    if let Some(base) = exe_base(command) {
        if !base.is_empty() && !contains_word(&s, &base) {
            s.push(' ');
            s.push_str(&base);
        }
    }
    s
}

/// 命令行路径里的「厂商/产品目录名」，作为**最后一级**线索。
///
/// 有些启动项的 exe 名本身就毫无信息量：Google 的更新服务叫
/// `updater.exe`，只有看它所在的 `...\Google\GoogleUpdater\157.0.8089.0\`
/// 才知道是谁的更新器。这里把路径里有意义的段挑出来。
///
/// 只挑：纯字母数字（可含 `-` `_`）、长度 ≥ 4、含字母、且不在噪音表里的段。
/// 盘符、`Program Files`、`AppData`、版本号目录（`157.0.8089.0`）这类
/// 一律丢掉 —— 它们对「这是谁」没有任何帮助。
///
/// **这层只在具体条目全部没命中时才用**，所以哪怕挑错了段，
/// 也不会把一条本来能正确命中的启动项引导到别的条目上去。
fn path_hint(command: &str) -> String {
    const NOISE: &[&str] = &[
        "program", "files", "file", "windows", "system", "system32", "syswow64",
        "appdata", "local", "roaming", "programdata", "common", "users", "user",
        "administrator", "public", "default", "bin", "apps", "app", "application",
        "current", "temp", "tmp", "cache", "data", "resources", "resource",
        "driver", "drivers", "driverstore", "filerepository", "amd64", "x86", "x64",
        "packages", "package", "windowsapps", "startup", "start", "menu", "programs",
        "installed", "update", "updater", "updates", "systemprofile", "servicing",
    ];
    // 只取「可执行文件路径」这一段。`.exe` 后面全是命令行参数，
    // 按分隔符切会把 `/clean`、`-k netsvcs` 切成 `clean`、`k`、`netsvcs`
    // 这种毫无意义的段 —— 它们既不是厂商也不是产品，还可能撞上别的条目。
    let lower = command.to_ascii_lowercase();
    let cut = lower
        .find(".exe")
        .map(|i| i + 4)
        .unwrap_or(command.len())
        .min(command.len());
    let path = &command[..cut];

    let mut out: Vec<String> = Vec::new();
    for seg in path.split(['\\', '/', '"']) {
        let seg = seg.trim().trim_matches('"').to_ascii_lowercase();
        if seg.len() < 4 || seg.ends_with(".exe") {
            continue;
        }
        if !seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            continue;
        }
        if !seg.chars().any(|c| c.is_ascii_alphabetic()) {
            continue; // 版本号目录（157.0.8089.0）、纯数字
        }
        if NOISE.contains(&seg.as_str()) || out.contains(&seg) {
            continue;
        }
        out.push(seg);
    }
    out.join(" ")
}

/// 从命令行里挖出可执行文件的基名（不含 `.exe`，小写）。
///
/// 做法是找第一个 `.exe` 再往前回溯到最后一个**路径分隔符**。
/// 关键在于分隔符集合里**不能有空格**：`C:\Program Files\App\my util.exe -x`
/// 这种「文件名里带空格」的命令，按空格切会在 `my` 处截断得到 `util`，
/// 而按 `\` 回溯得到 `my util` —— 后者才是真正的文件名。
/// 反过来说，路径里的空格（`Program Files`）也天然被 `\` 兜住了。
///
/// `pub(crate)` 是因为启动项名单（`startup_lists`）也要用同一套启发式 ——
/// 用户按 `onedrive` 加黑，得能命中显示名叫「Microsoft OneDrive」的条目。
pub(crate) fn exe_base(command: &str) -> Option<String> {
    let lower = command.to_ascii_lowercase();
    let idx = lower.find(".exe")?;
    let head = &lower[..idx];
    let start = head
        .rfind(['\\', '/', '"', '|', ','])
        .map(|i| i + 1)
        .unwrap_or(0);
    let base = head[start..].trim();
    if base.is_empty() {
        None
    } else {
        Some(base.to_string())
    }
}

/// 归一化匹配串：小写、去 `.exe`、去首尾空白。
/// 归一化匹配串。
///
/// **不剥 `.exe`**。早先这里会把 `.exe` 后缀去掉，于是「Windows 核心系统进程」
/// 那条的 key 从 `services.exe` 变成了 `services` —— 而 `contains_word` 是
/// 按词边界匹配的，`Gaming Services`（Xbox 游戏服务）里的 `Services` 正好是个
/// 独立的词，直接被判成「登录验证 / 会话管理，属于绝对不可动的部分」。
/// 带上扩展名的 key 本来就是**文件名**，haystack 里也带着扩展名
/// （`...\GamingServicesNet.exe`），按文件名原样匹配才精确。
fn normalize_key(k: &str) -> String {
    k.trim().to_ascii_lowercase()
}

/// 词边界包含：命中片段的前后都不能是字母数字。
///
/// `contains_word("runtimebroker", "time")` → false（前面是 `n`）
/// `contains_word("windows time", "time")`   → true
/// `contains_word("qqprotect", "qq")`        → false（后面是 `p`）
fn contains_word(hay: &str, key: &str) -> bool {
    let hb = hay.as_bytes();
    let mut from = 0usize;
    while from + key.len() <= hb.len() {
        let Some(rel) = hay[from..].find(key) else {
            return false;
        };
        let i = from + rel;
        let j = i + key.len();
        let before_ok = i == 0 || !hb[i - 1].is_ascii_alphanumeric();
        let after_ok = j == hb.len() || !hb[j].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
        from = i + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_library_parses_and_has_substance() {
        let b = builtin();
        assert!(
            b.len() >= 100,
            "内置知识库条目太少（{} 条），覆盖不了常见启动项",
            b.len()
        );
        for e in b {
            assert!(!e.keys.is_empty(), "「{}」没有匹配串", e.title);
            assert!(!e.title.trim().is_empty(), "有条目缺标题");
            assert!(
                !e.desc.trim().is_empty(),
                "「{}」没有描述 —— 知识库的价值就在描述上",
                e.title
            );
            assert!(
                matches!(e.advice.as_str(), "可以禁用" | "谨慎处理" | "建议保留" | "建议禁用"),
                "「{}」的建议取值不规范：{}",
                e.title,
                e.advice
            );
        }
    }

    /// 词边界必须真的挡住子串误命中 —— 这是内置库能用短词的前提。
    #[test]
    fn word_boundary_blocks_substring_hits() {
        assert!(!contains_word("runtimebroker", "time"), "runtime 里的 time 不该命中");
        assert!(!contains_word("qqprotect", "qq"), "qqprotect 里的 qq 不该命中");
        assert!(!contains_word("codecs", "code"), "codecs 里的 code 不该命中");
        assert!(contains_word("windows time", "time"));
        assert!(contains_word("qq", "qq"));
        assert!(contains_word("visual studio code", "code"));
        assert!(contains_word("c:\\x\\onedrive", "onedrive"));
    }

    #[test]
    fn exe_base_survives_quotes_and_spaces() {
        assert_eq!(exe_base(r#""C:\Program Files\App\a.exe" -k"#).as_deref(), Some("a"));
        assert_eq!(exe_base(r"C:\Program Files\App\a.exe -k").as_deref(), Some("a"));
        assert_eq!(exe_base(r"C:\Windows\system32\svchost.exe -k netsvcs").as_deref(), Some("svchost"));
        // 文件名里带空格：必须整段取回来，不能按空格截断
        assert_eq!(exe_base(r"C:\Program Files\App\my util.exe -x").as_deref(), Some("my util"));
        assert_eq!(exe_base(r#""C:\Program Files\My App\a b.exe" /min"#).as_deref(), Some("a b"));
        // 带引号的参数、前置启动器都要能穿过
        assert_eq!(exe_base(r#"cmd /c "C:\x\a.exe""#).as_deref(), Some("a"));
        assert_eq!(exe_base(r#"start "" /b C:\x\a.exe"#).as_deref(), Some("a"));
        assert_eq!(exe_base("no-exe-here"), None);
    }

    #[test]
    fn lookup_hits_known_and_misses_unknown() {
        let k = lookup("OneDrive", r#""C:\Program Files\Microsoft OneDrive\OneDrive.exe" /background"#, StartupSource::RunKey)
            .expect("OneDrive 应该有内置说明");
        assert!(k.title.contains("OneDrive"), "{}", k.title);
        assert_eq!(k.origin, "builtin");

        assert!(lookup("某个不存在的自启项xyzzy", "C:\\tmp\\xyzzy.exe", StartupSource::RunKey).is_none());
    }

    /// 具体条目要赢过通用兜底：`nvbackend` 比 `updater` 更该被选中。
    #[test]
    fn specific_entry_beats_generic_fallback() {
        let k = lookup("NvBackend", r"C:\Program Files\NVIDIA Corporation\Update Core\NvBackend.exe", StartupSource::RunKey)
            .expect("NvBackend 应该有说明");
        assert!(
            k.vendor.to_ascii_lowercase().contains("nvidia"),
            "命中的应该是 NVIDIA 那条，实际是：{} / {}",
            k.title,
            k.vendor
        );
    }

    /// 兜底条目只该给保守建议，而且必须输给用户手写的条目。
    ///
    /// 背景：`updater` / `helper` / `launcher` 这类宽 key 会命中一大堆真实程序。
    /// 早期版本里 `delta_force_launcher` 被 `launcher` 命中，被判成「厂商助手、
    /// 可以禁用」——但那是游戏启动器，禁用后游戏直接打不开。光凭名字下
    /// 「可以禁用」的硬结论是不可接受的。
    #[test]
    fn generic_fallback_is_cautious_and_loses_to_user_entries() {
        // 1) 只被兜底 key 命中的程序：结论必须是「谨慎处理」。
        //    注意这里用「Acme Updater」而不是「AcmeUpdater」：词边界要求
        //    匹配串前后不能是字母数字，所以 `acmeupdater` 里的 `updater`
        //    本来就不会命中 —— 这层保守是刻意的，泛化面越小越好。
        let k = lookup_with(
            "Acme Updater",
            r"C:\Program Files\Acme\AcmeUpdater.exe",
            StartupSource::RunKey,
            &[],
        )
        .expect("updater 兜底条目应该命中");
        assert_eq!(k.advice, "谨慎处理", "兜底条目不该给硬结论：{}", k.title);
        assert_eq!(k.vendor, "（按名称推断）", "兜底条目要说清它是推断出来的");

        // 2) 用户库手写的条目压过内置兜底
        let user = vec![RawEntry {
            keys: vec!["acmeupdater".into()],
            title: "公司补丁更新器".into(),
            vendor: "Acme IT".into(),
            desc: "IT 统一下发的补丁更新器，公司规定必须保留。".into(),
            advice: "建议保留".into(),
            generic: false,
        }];
        let k = lookup_with(
            "Acme Updater",
            r"C:\Program Files\Acme\AcmeUpdater.exe",
            StartupSource::RunKey,
            &user,
        )
        .expect("用户条目应该命中");
        assert_eq!(k.origin, "user");
        assert_eq!(k.advice, "建议保留", "用户写的就是用户要的");
    }

    /// 路径线索只挑「厂商 / 产品」目录名，程序目录与版本号目录必须被丢掉。
    #[test]
    fn path_hint_keeps_vendor_segments_only() {
        let h = path_hint(
            r#""C:\Program Files (x86)\Google\GoogleUpdater\157.0.8089.0\updater.exe" --system"#,
        );
        assert!(h.contains("google"), "漏掉厂商段：{h}");
        assert!(h.contains("googleupdater"), "漏掉产品段：{h}");
        assert!(!h.contains("program"), "噪音目录不该进来：{h}");
        assert!(!h.contains("157"), "版本号目录不该进来：{h}");
        assert!(!h.contains("updater.exe"), "exe 名由 exe_base 负责，不该重复：{h}");

        // 命令行参数不是线索：`/clean` 曾被切成 `clean` 混进搜索结果
        let h2 = path_hint(r"%windir%\system32\MdmDiagnosticsTool.exe /clean");
        assert!(h2.is_empty(), "命令行开关不该变成线索：{h2}");
    }

    /// 显示名是中文、exe 名又毫无信息量（`updater.exe`）时，
    /// 必须靠路径认出它是谁的更新器，而不是丢给「软件自动更新程序」这条兜底。
    #[test]
    fn vendor_hint_rescues_generic_exe_names() {
        let k = lookup(
            "Google 更新程序内部服务 (GoogleUpdaterInternalService157.0.8089.0)",
            r#""C:\Program Files (x86)\Google\GoogleUpdater\157.0.8089.0\updater.exe" --system"#,
            StartupSource::Service,
        )
        .expect("应该能认出这是 Google 的更新器");
        assert!(k.title.contains("Google"), "实际命中：{}", k.title);
        assert_ne!(k.vendor, "（按名称推断）", "不该落到兜底条目上：{}", k.title);
    }

    /// 路径线索不许抢占「显示名已经认出来了」的结果。
    #[test]
    fn vendor_hint_never_overrides_a_name_match() {
        let k = lookup(
            "WeChat",
            r"C:\Program Files\Tencent\WeChat\WeChat.exe",
            StartupSource::RunKey,
        )
        .expect("微信应该命中");
        assert!(
            k.title.contains("微信") || k.title.contains("WeChat"),
            "路径里的 tencent 不该把它改判：{}",
            k.title
        );
    }

    /// 回归护栏：裸 `launcher` 不许出现在内置库里。
    /// 它会把所有「游戏/软件启动器」一律判成厂商推广组件 —— 用户照着做
    /// 就等于把游戏启动器关掉。宁可什么都不说，也不能说错。
    #[test]
    fn bare_launcher_key_is_banned() {
        for e in builtin() {
            for k in &e.keys {
                assert_ne!(
                    normalize_key(k),
                    "launcher",
                    "「{}」用裸 launcher 做匹配串，会连游戏启动器一起命中",
                    e.title
                );
            }
        }
    }

    /// 同一个匹配串不许出现在两个条目里 —— 否则命中哪条取决于条目顺序，
    /// 是「改一行 JSON 结果就变」的隐性 bug（曾出现 `fax`、`nissrv` 各两份）。
    #[test]
    fn builtin_keys_do_not_collide_across_entries() {
        let mut seen: std::collections::HashMap<String, &str> = Default::default();
        for e in builtin() {
            for k in &e.keys {
                let k = normalize_key(k);
                if let Some(prev) = seen.get(&k) {
                    assert_eq!(
                        *prev, e.title,
                        "匹配串「{k}」同时属于「{prev}」和「{}」，命中结果会随条目顺序漂移",
                        e.title
                    );
                }
                seen.insert(k, e.title.as_str());
            }
        }
    }
}

