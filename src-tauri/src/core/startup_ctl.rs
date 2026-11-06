//! 启动项禁用 / 还原执行器。
//!
//! ## 一条铁律：先备份，再动手
//!
//! 每一次变更都把「怎么还原」写进 `undo` 字段并落盘成批次文件。
//! 还原不需要重新扫描系统，只按备份回放 —— 因为用户点「还原」时，
//! 系统状态可能已经和当时完全不同了。
//!
//! ## 各来源的手法
//!
//! | 来源 | 禁用 | 启用 | 删除 |
//! |---|---|---|---|
//! | 注册表 Run 值 | 写 `StartupApproved\Run` 的「已禁用」标志 | 写回「已启用」标志 | 删值（原值备份进变更记录） |
//! | 启动文件夹 | 写 `StartupApproved\StartupFolder` 标志 | 写回标志 | 移到隔离目录（备份） |
//! | 自动启动服务 | `ChangeServiceConfig(SERVICE_DISABLED)` | `ChangeServiceConfig(SERVICE_AUTO_START)` | **不支持**（见下） |
//! | 自动启动服务（延迟） | `ChangeServiceConfig2(SERVICE_CONFIG_DELAYED_AUTO_START_INFO)` | — | — |
//! | 计划任务 | `schtasks /Change /DISABLE` | `schtasks /Change /ENABLE` | **不支持**（见下） |
//!
//! ## 为什么「禁用」不去删注册表 Run 值
//!
//! 任务管理器、Autoruns 都是写 `StartupApproved` 下的二进制标志位
//! （首字节 `0x02` = 启用、`0x03` = 禁用），而不是删值。差别很实在：
//!
//! * 删了值，条目就从列表里**消失**，用户再也找不到还原入口；
//! * 很多软件下次启动会把值自己写回来，禁用形同虚设；
//! * 写标志位则是「值还在、只是不拉起了」，随时一键启用。
//!
//! 服务和计划任务没有等价的独立标志位（服务的 `Start`、任务的 `<Enabled>`
//! 本身就是状态），它们被禁用后会从枚举结果里消失 —— 这一层交给
//! `startup::merge_disabled_by_us()` 用变更留痕把条目**重建回列表**，
//! 用户看到的仍然是「已禁用 + 一键启用」。
//!
//! ## 为什么不支持删除服务 / 计划任务
//!
//! 删一个服务等于把软件卸掉一半（`sc delete` 之后 SCM 里再无此人，
//! 软件自己都装不回来）；删计划任务会让依赖它的功能静默失效。
//! 用户要的「删除」针对的是**注册表自启项**，这里就只对注册表值与启动文件夹
//! 快捷方式开放删除，服务 / 任务一律明确拒绝并引导去用「禁用」——
//! 宁可少一个按钮，也不要给一个按下去会让系统半残的按钮。
//!
//! ## 延迟启动的边界
//!
//! `DelayedAutostart` 只对 **自动启动（Start=2）的服务** 有意义，
//! 且只在**下次开机**生效。注册表项/启动文件夹/计划任务没有等价能力，
//! 因此 delay 模式下这些来源会逐条给出「该来源不支持延迟启动」——
//! 宁可明确拒绝，也不假装做了点什么。

//!
//! ## 三道闸
//!
//! 1. `Dangerous` 档的条目**在枚举阶段就被标成不可禁用**，这里再兜一次；
//! 2. `safety::check_startup_change` —— 白名单闸 + 熔断闸；
//! 3. 默认 `dry_run`：`cfg!(test)` 或 `OLD666_DRY_RUN=1` 时只出计划不动手。

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::core::safety;
use crate::core::store;
use crate::core::types::{RiskLevel, StartupChange, StartupItem, StartupResult, StartupSource};

// ===========================================================================
// 定位
// ===========================================================================

/// 从 `location` 解析出的可执行动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// 注册表 Run 值：`HKCU\Software\...\Run!值名`
    RunValue {
        hive: String,
        subkey: String,
        value: String,
    },
    /// 启动文件夹里的文件
    StartupFile { path: String },
    /// 服务
    Service { key_name: String },
    /// 计划任务（`name` 字段即任务 URI）
    Task { uri: String },
}

/// 解析启动项定位串。解析不出来就是错误 —— 宁可不动，也不能猜着动。
pub fn parse_target(item: &StartupItem) -> Result<Target, String> {
    match item.source {
        StartupSource::RunKey => {
            let (path, value) = item
                .location
                .split_once('!')
                .ok_or_else(|| format!("注册表定位串格式不对：{}", item.location))?;
            let (hive, subkey) = path
                .split_once('\\')
                .ok_or_else(|| format!("注册表定位串缺少根键：{path}"))?;
            if hive != "HKCU" && hive != "HKLM" {
                return Err(format!("不支持的注册表根键：{hive}"));
            }
            if subkey.trim().is_empty() || value.trim().is_empty() {
                return Err("注册表定位串缺少子键或值名".into());
            }
            Ok(Target::RunValue {
                hive: hive.to_string(),
                subkey: subkey.to_string(),
                value: value.to_string(),
            })
        }
        StartupSource::StartupFolder => Ok(Target::StartupFile {
            path: item.location.clone(),
        }),
        StartupSource::Service => {
            // 服务定位串必须形如 HKLM\SYSTEM\CurrentControlSet\Services\<KeyName>，
            // 至少得有反斜杠 —— 否则说明 location 被写坏了，宁可不动
            if !item.location.contains('\\') {
                return Err(format!("服务定位串格式不对：{}", item.location));
            }
            let key_name = item
                .location
                .rsplit('\\')
                .next()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| format!("服务定位串缺少服务名：{}", item.location))?;
            Ok(Target::Service {
                key_name: key_name.to_string(),
            })
        }
        StartupSource::ScheduledTask => {
            if item.name.trim().is_empty() {
                return Err("计划任务缺少名称".into());
            }
            Ok(Target::Task {
                uri: item.name.clone(),
            })
        }
    }
}

// ===========================================================================
// 批次持久化
// ===========================================================================

fn startup_dir() -> PathBuf {
    store::data_dir().join("startup")
}

fn quarantine_dir(batch: &str) -> PathBuf {
    startup_dir().join("quarantine").join(batch)
}

/// 生成批次 ID（毫秒时间戳 + 序号 + PID，保证同毫秒不撞）。
pub fn new_batch_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("sb-{ts:x}-{seq:x}-{:x}", std::process::id())
}

fn save_batch(batch: &str, changes: &[StartupChange]) -> Result<(), String> {
    let dir = startup_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建目录失败：{e}"))?;
    let path = batch_path(batch)?;
    let text = serde_json::to_string_pretty(changes).map_err(|e| format!("序列化失败：{e}"))?;
    std::fs::write(&path, text).map_err(|e| format!("写入批次文件失败：{e}"))
}

/// 批次文件的路径。
///
/// 批次 ID 来自前端，必须挡住路径穿越（`../`、`C:\`、`/`）——
/// 否则一个被污染的 ID 就能让程序去读/写任意文件。
fn batch_path(batch: &str) -> Result<PathBuf, String> {
    if batch.trim().is_empty() || batch.contains(['/', '\\', ':', '.']) {
        return Err(format!("非法的批次 ID：{batch}"));
    }
    Ok(startup_dir().join(format!("{batch}.json")))
}

/// 读取一个批次的变更记录。
pub fn load_batch(batch: &str) -> Result<Vec<StartupChange>, String> {
    let path = batch_path(batch)?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("读取批次失败：{e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("解析批次失败：{e}"))
}

/// 列出全部批次 ID（**新的在前**）。
///
/// 按文件修改时间排序而不是按 ID 字典序：ID 里虽然有毫秒时间戳，
/// 但那是十六进制，跨越长度差异的时间点比大小就不准了。
/// 顺序很要紧 —— 「找回这条最新的禁用记录」依赖它。
pub fn list_batches() -> Vec<String> {
    let mut out: Vec<(String, SystemTime)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(startup_dir()) {
        for e in rd.filter_map(Result::ok) {
            let p = e.path();
            if p.extension().map(|x| x == "json").unwrap_or(false) {
                if let Some(stem) = p.file_stem() {
                    let t = e
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(UNIX_EPOCH);
                    out.push((stem.to_string_lossy().to_string(), t));
                }
            }
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out.into_iter().map(|(id, _)| id).collect()
}

/// 留痕里的目标**现在还在不在**。
///
/// 用来挡「幽灵条目」：软件卸载之后，留痕里那条「已禁用」记录还在，
/// 但注册表值 / 服务 / 快捷方式早就没了。不查一下的话，列表里会永远
/// 挂着一条点「启用」必然失败的死条目。
pub fn target_alive(source: StartupSource, location: &str) -> bool {
    match source {
        StartupSource::Service => {
            let key = location.rsplit('\\').next().unwrap_or("").trim();
            !key.is_empty()
                && winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
                    .open_subkey(format!(r"SYSTEM\CurrentControlSet\Services\{key}"))
                    .is_ok()
        }
        StartupSource::RunKey => {
            let Some((path, value)) = location.split_once('!') else {
                return false;
            };
            let Some((hive, subkey)) = path.split_once('\\') else {
                return false;
            };
            let Ok(root) = hive_key(hive) else {
                return false;
            };
            winreg::RegKey::predef(root)
                .open_subkey(subkey)
                .map(|k| k.get_raw_value(value).is_ok())
                .unwrap_or(false)
        }
        StartupSource::StartupFolder => PathBuf::from(location).exists(),
        // 计划任务的状态在 XML 文件里，但「这个任务还算不算数」只有 SCM 说了算，
        // 这里不做判断，交给还原时兜底报错
        StartupSource::ScheduledTask => true,
    }
}

// ===========================================================================
// 变更执行
// ===========================================================================

/// 是否需要管理员才能改动该条目。
fn needs_admin(t: &Target) -> bool {
    match t {
        Target::RunValue { hive, .. } => hive == "HKLM",
        Target::StartupFile { .. } => false,
        Target::Service { .. } => true,
        Target::Task { .. } => true,
    }
}

/// 动手之前先问一句「到底有没有权限改这个」。
///
/// 演练模式原本只算撤销载荷、不碰系统，于是确认清单会承诺「全部可一键还原」，
/// 真正执行时才撞上 `ERROR_ACCESS_DENIED`（0x80070005）—— 用户看到的是
/// 「点了半天，弹个报错，什么也没发生」。这里做**只读**探测：
/// 服务的 ACL、注册表键的写权限，能打开就立刻关掉，不改任何值。
///
/// 被安全软件保护的服务（360 / 火绒等的自保护）即使以管理员身份也改不动，
/// 这是系统事实，不是 bug —— 但必须在**确认之前**说清楚。
pub fn probe_writable(t: &Target) -> Result<(), String> {
    match t {
        Target::Service { key_name } => probe_service(key_name),
        Target::RunValue { hive, subkey, .. } => probe_reg_key(hive, subkey),
        // 计划任务与快捷方式的权限要走 ACL 解析，收益不成正比：
        // 真出错时 schtasks / 文件移动给出的原因已经够清楚。
        _ => Ok(()),
    }
}

/// 只读探测：能不能拿到服务的「改配置」权限。
fn probe_service(key_name: &str) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, SC_MANAGER_CONNECT,
        SERVICE_CHANGE_CONFIG,
    };

    let name_w: Vec<u16> = key_name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let scm = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT)
            .map_err(|e| format!("打开服务管理器失败（需要管理员权限）：{e}"))?;
        let r = OpenServiceW(scm, PCWSTR(name_w.as_ptr()), SERVICE_CHANGE_CONFIG);
        let _ = CloseServiceHandle(scm);
        match r {
            Ok(h) => {
                let _ = CloseServiceHandle(h);
                Ok(())
            }
            Err(e) if is_access_denied(&e) => Err(
                "被系统拒绝修改：该服务受保护（常见于安全软件自保护，或只允许 SYSTEM 修改），本程序不会绕过它"
                    .to_string(),
            ),
            Err(e) => Err(format!("打不开该服务：{e}")),
        }
    }
}

/// 只读探测：Run 键能不能写（禁用要删值、启用要写 StartupApproved，都需要写权限）。
fn probe_reg_key(hive: &str, subkey: &str) -> Result<(), String> {
    let root = hive_key(hive)?;
    match winreg::RegKey::predef(root).open_subkey_with_flags(subkey, winreg::enums::KEY_SET_VALUE) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Err(
            "被系统拒绝修改：该注册表启动项需要管理员权限，或被安全软件保护".to_string(),
        ),
        Err(e) => Err(format!("打开注册表启动项失败：{e}")),
    }
}

/// `ERROR_ACCESS_DENIED` 在 windows-rs 里是 HRESULT(0x80070005)。
fn is_access_denied(e: &windows::core::Error) -> bool {
    e.code().0 as u32 == 0x8007_0005
}

/// 一次变更的意图。
///
/// 「禁用」「延迟启动」「启用」「删除」是**四种不同的用户意图**，
/// 必须分开走，不能揉成一个「优化」按钮：
///
/// * `Disable` —— 开机不再拉起，但条目留在列表里、随时可「启用」
/// * `Enable`  —— 把已禁用的项恢复启动（既包括我们自己禁的，也包括
///   用户在任务管理器 / 服务管理器里自己关掉的）
/// * `Delayed` —— 想留但想更快（仅自动启动服务）
/// * `Delete`  —— 彻底清掉这条自启（注册表值 / 启动项文件），
///   备份留在变更记录里。与「禁用」的区别是**不再出现在列表里**，
///   也就没有「启用」入口，只在变更记录里可恢复。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 禁用：开机不再拉起，但可以再启用
    Disable,
    /// 启用：把已禁用的项恢复启动
    Enable,
    /// 删除：彻底移除自启（可验证关掉），备份留档
    Delete,
    /// 延迟自动启动（仅自动启动服务；`on=false` 表示取消延迟）
    Delayed { on: bool },
}

impl Mode {
    /// 动作名（写进批次留痕）。
    pub fn action(&self) -> &'static str {
        match self {
            Mode::Disable => "disable",
            Mode::Enable => "enable",
            Mode::Delete => "delete",
            Mode::Delayed { on: true } => "delay",
            Mode::Delayed { on: false } => "undelay",
        }
    }

    /// 从字符串解析（前端传来的 action 字段）。
    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "disable" | "" => Some(Mode::Disable),
            "enable" => Some(Mode::Enable),
            "delete" | "remove" => Some(Mode::Delete),
            "delay" => Some(Mode::Delayed { on: true }),
            "undelay" => Some(Mode::Delayed { on: false }),
            _ => None,
        }
    }

    /// 这个动作是否**会关掉**某个开机自启（也就是需要走风险闸）。
    ///
    /// 「启用」不需要：它只是把系统恢复到原样，不可能让机器变得更脆弱。
    /// 如果这里也拦，「用户想把自己关掉的驱动服务开回来」就永远做不到。
    pub fn needs_risk_gate(&self) -> bool {
        !matches!(self, Mode::Enable)
    }

    /// 这个动作是否要求条目**当前处于启用状态**（否则无意义）。
    pub fn requires_enabled(&self) -> bool {
        matches!(self, Mode::Disable | Mode::Delayed { .. })
    }

    /// 这个动作是否要求条目**当前处于禁用状态**（否则无意义）。
    ///
    /// 「删除」两项都不要求：它关心的是「这条自启还在不在」，
    /// 而不是「它当前拉不拉起」—— 顺手删掉一条早就被禁用的残留也是常见诉求。
    pub fn requires_disabled(&self) -> bool {
        matches!(self, Mode::Enable)
    }
}

/// 执行一批启动项变更。
///
/// * `items` —— 要处理的条目（必须来自同一次 `startup::scan()`）
/// * `dry`   —— 只出计划不动手。调用方通常传 `safety::is_dry_run() || !confirmed`
/// * `mode`  —— 禁用 / 启用 / 删除 / 延迟启动
pub fn apply(items: &[StartupItem], dry: bool) -> StartupResult {
    apply_with(items, dry, Mode::Disable)
}

pub fn apply_with(items: &[StartupItem], dry: bool, mode: Mode) -> StartupResult {
    let t0 = Instant::now();
    let batch = new_batch_id();
    let mut changes = Vec::new();

    for item in items {
        // location / command 必须一起留痕：条目被禁用后可能从系统里消失，
        // 重建回列表时需要它们才能算出「该动哪里」
        let mut rec = StartupChange {
            item_id: item.id.clone(),
            name: item.name.clone(),
            source: item.source,
            action: mode.action().into(),
            undo: String::new(),
            ok: false,
            error: None,
            location: item.location.clone(),
            command: item.command.clone(),
        };

        // 闸 1a：方向一致性。「启用」只能作用于已禁用的条目，「禁用 / 延迟」
        // 只能作用于已启用的条目。弄反了必须明说，绝不能默默按另一个方向执行 ——
        // 那等于替用户做了他没说过的决定。
        if mode.requires_disabled() && item.enabled {
            rec.error = Some("该启动项当前就是启用状态".into());
            changes.push(rec);
            continue;
        }
        if mode.requires_enabled() && !item.enabled {
            rec.error = Some("该启动项已经处于禁用状态".into());
            changes.push(rec);
            continue;
        }

        // 闸 1b：危险项不允许被关掉。
        //
        // 例外只有一条：用户把这条**明确加进了黑名单**，且动作是「禁用」——
        // 那是他自己表过的态（「加入黑名单的不需要过问直接禁用」）。
        // 删除不可逆，黑名单不足以授权，仍然拒绝。
        //
        // 「启用」永不设闸：把系统恢复原样不可能让机器更脆弱。
        if mode.needs_risk_gate() && (item.risk == RiskLevel::Dangerous || !item.can_disable) {
            let blacklist_authorised = item.in_blacklist && matches!(mode, Mode::Disable);
            if !blacklist_authorised {
                rec.error = Some(format!(
                    "「{}」被标记为不建议禁用，已跳过（确认要动请先加入黑名单）",
                    item.name
                ));
                changes.push(rec);
                continue;
            }
        }

        // 闸 2：安全闸（白名单 + 熔断）
        let target = match parse_target(item) {
            Ok(t) => t,
            Err(e) => {
                rec.error = Some(e);
                changes.push(rec);
                continue;
            }
        };
        if !dry {
            if let Err(e) = safety::check_startup_change(&item.id) {
                rec.error = Some(e);
                changes.push(rec);
                continue;
            }
        }
        if needs_admin(&target) && !crate::core::is_elevated() {
            rec.error = Some("需要管理员权限才能修改该启动项".into());
            changes.push(rec);
            continue;
        }

        // 闸 3：权限预检（**演练阶段也做**）。
        // 演练的职责不只是「算出撤销载荷」，还要保证确认清单里承诺的事真能做到。
        // 受保护的服务在这里就被标出来，用户在点确认之前就知道它动不了。
        if let Err(e) = probe_writable(&target) {
            rec.error = Some(format!("「{}」{e}", item.name));
            changes.push(rec);
            continue;
        }

        match change_one(&target, &batch, dry, mode) {
            Ok(undo) => {
                rec.undo = undo;
                rec.ok = true;
            }
            Err(e) => rec.error = Some(e),
        }
        changes.push(rec);
    }

    // 只有真的动了系统才落盘备份
    if !dry {
        if let Err(e) = save_batch(&batch, &changes) {
            // 备份失败必须让用户知道，但已经改了的东西不撤回
            // （撤回反而可能造成更大混乱），只在第一条记录上标注
            if let Some(first) = changes.first_mut() {
                first.error = Some(match &first.error {
                    Some(prev) => format!("{prev}；另外：备份落盘失败 {e}"),
                    None => format!("备份落盘失败 {e}，请勿依赖「还原」功能"),
                });
            }
        }
    }

    StartupResult {
        batch_id: if dry { String::new() } else { batch },
        timestamp: crate::core::process::now_unix(),
        changes,
        dry_run: dry,
        elapsed_ms: t0.elapsed().as_millis() as u64,
    }
}

/// 对单个条目执行一种变更，返回「怎么还原」的载荷。
fn change_one(t: &Target, batch: &str, dry: bool, mode: Mode) -> Result<String, String> {
    match mode {
        Mode::Disable => disable_one(t, dry),
        Mode::Enable => enable_one(t, dry),
        Mode::Delete => delete_one(t, batch, dry),
        Mode::Delayed { on } => delay_one(t, dry, on),
    }
}

/// 禁用单个条目，返回「怎么还原」的载荷。
///
/// **不碰命令行本身**：注册表项与启动文件夹写 `StartupApproved` 标志位
/// （与任务管理器同一套机制），条目因此**仍然留在列表里**，随时可一键启用。
/// 服务改 `Start`、计划任务改 `<Enabled>` —— 这两个来源没有独立标志位，
/// 被禁用后会从枚举结果里消失，由 `startup::merge_disabled_by_us()` 重建回列表。
fn disable_one(t: &Target, dry: bool) -> Result<String, String> {
    let undo = undo_payload(t)?;
    if dry {
        return Ok(undo);
    }
    match t {
        Target::RunValue { .. } | Target::StartupFile { .. } => set_approved(t, false)?,
        Target::Service { key_name } => {
            set_service_start(key_name, crate::core::startup::START_DISABLED)?
        }
        Target::Task { uri } => run_schtasks("/Change", uri, "/DISABLE")?,
    }
    Ok(undo)
}

/// 启用单个条目 —— 把已禁用的项恢复启动。
///
/// 既覆盖「我们自己禁的」，也覆盖「用户在任务管理器 / 服务管理器里自己关掉的」：
/// 两者在系统看来是同一件事，用户想开回来就该能开回来。
fn enable_one(t: &Target, dry: bool) -> Result<String, String> {
    let undo = undo_payload(t)?;
    if dry {
        return Ok(undo);
    }
    match t {
        Target::RunValue { .. } | Target::StartupFile { .. } => set_approved(t, true)?,
        Target::Service { key_name } => set_service_start(key_name, crate::core::startup::START_AUTO)?,
        Target::Task { uri } => run_schtasks("/Change", uri, "/ENABLE")?,
    }
    Ok(undo)
}

/// 删除单个条目的自启（**先备份，再动手**）。
///
/// 只对注册表值与启动文件夹快捷方式开放（见文件头「为什么不支持删除服务 /
/// 计划任务」）。注册表值把原值写进还原载荷，快捷方式挪进隔离目录。
fn delete_one(t: &Target, batch: &str, dry: bool) -> Result<String, String> {
    match t {
        Target::RunValue {
            hive,
            subkey,
            value,
        } => {
            let root = hive_key(hive)?;
            let key = winreg::RegKey::predef(root)
                .open_subkey_with_flags(
                    subkey,
                    winreg::enums::KEY_QUERY_VALUE | winreg::enums::KEY_SET_VALUE,
                )
                .map_err(|e| format!("打开注册表项失败：{e}"))?;
            let original: String = key
                .get_value(value)
                .map_err(|e| format!("读取原值失败：{e}（可能已经被删掉了）"))?;

            // 备份两样东西：值本身，以及它在 StartupApproved 里的标志（可能本来就没有）
            let mut undo = serde_json::json!({
                "kind": "regvalue",
                "reg": format!("{hive}\\{subkey}!{value}"),
                "value": original,
            });
            if let Ok((h, s, n)) = approved_target(t) {
                if let Ok(Some(prev)) = read_approved_bytes(&h, &s, &n) {
                    undo["approved"] = serde_json::json!(format!("{h}\\{s}!{n}"));
                    undo["approved_prev"] = serde_json::json!(prev);
                }
            }
            let undo = undo.to_string();

            if dry {
                return Ok(undo);
            }
            key.delete_value(value)
                .map_err(|e| format!("删除注册表值失败：{e}"))?;
            // 值都没了，留着「已禁用」标志只会变成垃圾
            if let Ok((h, s, n)) = approved_target(t) {
                let _ = clear_approved(&h, &s, &n);
            }
            Ok(undo)
        }
        Target::StartupFile { path } => {
            let src = PathBuf::from(path);
            if !src.exists() {
                return Err(format!("文件不存在：{path}"));
            }
            let undo = serde_json::json!({ "kind": "file", "path": path }).to_string();
            if dry {
                return Ok(undo);
            }
            let dir = quarantine_dir(batch);
            std::fs::create_dir_all(&dir).map_err(|e| format!("创建隔离目录失败：{e}"))?;
            let file_name = src
                .file_name()
                .ok_or_else(|| "路径没有文件名".to_string())?;
            let dst = dir.join(file_name);
            std::fs::rename(&src, &dst).map_err(|e| format!("移动文件失败：{e}"))?;
            Ok(undo)
        }
        Target::Service { .. } => Err(
            "服务不支持删除 —— 删掉之后软件自己都装不回来。想让它不随开机启动，请用「禁用」，同样随时可恢复。"
                .into(),
        ),
        Target::Task { .. } => Err(
            "计划任务不支持删除 —— 依赖它的功能会静默失效。想让它不随开机启动，请用「禁用」，同样随时可恢复。"
                .into(),
        ),
    }
}

/// 记录「怎么还原」。
///
/// 一律**先读当前状态再动手**，把还原依据写进载荷；绝不在还原时再从当前系统
/// 状态反推 —— 用户点「还原」时，系统可能已经和当时完全不同了。
fn undo_payload(t: &Target) -> Result<String, String> {
    match t {
        Target::RunValue { .. } | Target::StartupFile { .. } => {
            let (hive, subkey, name) = approved_target(t)?;
            let prev = read_approved_bytes(&hive, &subkey, &name)?;
            Ok(serde_json::json!({
                "kind": "approved",
                "reg": format!("{hive}\\{subkey}!{name}"),
                "prev": prev,
            })
            .to_string())
        }
        Target::Service { key_name } => {
            let start = read_service_start(key_name)?;
            Ok(serde_json::json!({ "kind": "start", "key": key_name, "start": start }).to_string())
        }
        Target::Task { uri } => {
            // 还原方向由批次里的 `action` 决定（disable → 启用回，enable → 禁回去），
            // 所以载荷本身只需要记「是哪一条任务」。
            Ok(serde_json::json!({ "kind": "task", "uri": uri }).to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// StartupApproved 标志位
// ---------------------------------------------------------------------------

/// 「已启用」标志字节（任务管理器写的就是它）。
const APPROVED_ENABLED: u8 = 0x02;
/// 「已禁用」标志字节。
const APPROVED_DISABLED: u8 = 0x03;

/// 一个条目的「启用状态标志」在注册表里的位置：`(根键, 子键, 值名)`。
///
/// Windows 把「这条自启还拉不拉起」记在 `Explorer\StartupApproved` 下，
/// 二进制首字节 `0x02` = 启用、`0x03` = 禁用。任务管理器 / 360 / Autoruns
/// 用的都是它，所以改这里能被系统一致地识别。
fn approved_target(t: &Target) -> Result<(String, String, String), String> {
    match t {
        Target::RunValue {
            hive,
            subkey,
            value,
        } => {
            // 32 位 Run 项的状态存在 `Run32`，不是 `Run` —— 见
            // `startup::approved_leaf`。写错子键等于什么都没做。
            let leaf = crate::core::startup::approved_leaf(subkey);
            Ok((
                hive.clone(),
                format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\{leaf}"),
                value.clone(),
            ))
        }
        Target::StartupFile { path } => {
            let p = PathBuf::from(path);
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .ok_or_else(|| format!("路径没有文件名：{path}"))?;
            // 用户级 / 机器级的标志位分别存在 HKCU 与 HKLM 下，必须对上 ——
            // 写到另一个根键就是一个「永远不会被读」的地方，看起来禁用成功、实际毫无作用。
            let user_dir = crate::core::startup::startup_dirs()
                .into_iter()
                .find(|(_, is_user)| *is_user)
                .map(|(d, _)| d);
            let hive = match user_dir {
                Some(d) if p.parent() == Some(d.as_path()) => "HKCU",
                _ => "HKLM",
            };
            Ok((
                hive.to_string(),
                r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder"
                    .to_string(),
                name,
            ))
        }
        _ => Err("该来源没有独立的启用状态标志".into()),
    }
}

/// 读标志位的原始字节。键 / 值不存在返回 `Ok(None)` —— 那正是「从没被改过」。
fn read_approved_bytes(hive: &str, subkey: &str, name: &str) -> Result<Option<Vec<u8>>, String> {
    let root = hive_key(hive)?;
    let Ok(key) = winreg::RegKey::predef(root).open_subkey(subkey) else {
        return Ok(None);
    };
    match key.get_raw_value(name) {
        Ok(v) => Ok(Some(v.bytes)),
        Err(_) => Ok(None),
    }
}

/// 写「启用 / 禁用」标志位。**不碰命令行本身**，所以条目仍在列表里。
fn set_approved(t: &Target, enabled: bool) -> Result<(), String> {
    let (hive, subkey, name) = approved_target(t)?;
    let root = hive_key(&hive)?;
    let (key, _) = winreg::RegKey::predef(root)
        .create_subkey(&subkey)
        .map_err(|e| format!("创建注册表项失败：{e}"))?;
    // 12 字节：首字节是标志，后面 8 字节本来是禁用时刻的 FILETIME。
    // 我们的读取只看首字节，写 0 不影响判定，也免得引入时间格式转换的坑。
    let mut data = [0u8; 12];
    data[0] = if enabled {
        APPROVED_ENABLED
    } else {
        APPROVED_DISABLED
    };
    let v = winreg::RegValue {
        bytes: data.to_vec(),
        vtype: winreg::enums::REG_BINARY,
    };
    key.set_raw_value(name, &v)
        .map_err(|e| format!("写入启动状态失败：{e}"))
}

/// 删掉标志位（恢复成「从没被改过」的样子）。
fn clear_approved(hive: &str, subkey: &str, name: &str) -> Result<(), String> {
    let root = hive_key(hive)?;
    let Ok(key) =
        winreg::RegKey::predef(root).open_subkey_with_flags(subkey, winreg::enums::KEY_SET_VALUE)
    else {
        return Ok(()); // 键都不存在，本来就没有标志要清
    };
    match key.delete_value(name) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("清理启动状态失败：{e}")),
    }
}

/// 设置 / 取消「延迟自动启动」。**只对自动启动服务有意义**。
///
/// 走 SCM 的 `ChangeServiceConfig2`，和改启动类型一样：直接写注册表
/// 不会通知服务控制管理器。撤销载荷只记 `{"kind":"delayed","key"}`，
/// 与禁用服务的 `{"kind":"start","key","start"}` 形态不同，按 `kind` 区分。
fn delay_one(t: &Target, dry: bool, on: bool) -> Result<String, String> {
    let Target::Service { key_name } = t else {
        return Err("该来源不支持延迟启动（仅自动启动的服务支持）".into());
    };
    let start = read_service_start(key_name)?;
    if start != crate::core::startup::START_AUTO {
        return Err(format!(
            "「{key_name}」不是自动启动类型（Start={start}），延迟启动对它没有意义"
        ));
    }
    if read_service_delayed(key_name)? == on {
        return Err(if on {
            "该服务已经处于延迟启动状态".into()
        } else {
            "该服务已经处于普通自动启动状态".into()
        });
    }
    let undo = serde_json::json!({ "kind": "delayed", "key": key_name, "delayed": !on }).to_string();
    if dry {
        return Ok(undo);
    }
    set_service_delayed(key_name, on)?;
    Ok(undo)
}

fn hive_key(hive: &str) -> Result<winreg::HKEY, String> {
    match hive {
        "HKCU" => Ok(winreg::enums::HKEY_CURRENT_USER),
        "HKLM" => Ok(winreg::enums::HKEY_LOCAL_MACHINE),
        other => Err(format!("不支持的注册表根键：{other}")),
    }
}

/// 读服务当前启动类型。
pub fn read_service_start(key_name: &str) -> Result<u32, String> {
    use winreg::enums::*;
    use winreg::RegKey;
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(format!(r"SYSTEM\CurrentControlSet\Services\{key_name}"))
        .map_err(|e| format!("打开服务项失败：{e}"))?
        .get_value("Start")
        .map_err(|e| format!("读取服务启动类型失败：{e}"))
}

/// 通过 SCM 修改服务启动类型。
///
/// 不直接改注册表：那样不会通知服务控制管理器，属于不规范做法。
fn set_service_start(key_name: &str, start: u32) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Services::{
        ChangeServiceConfigW, CloseServiceHandle, OpenSCManagerW, OpenServiceW,
        SC_MANAGER_CONNECT, SERVICE_CHANGE_CONFIG, SERVICE_ERROR, SERVICE_NO_CHANGE,
        SERVICE_START_TYPE,
    };

    let name_w: Vec<u16> = key_name.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        let scm = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT)
            .map_err(|e| format!("打开服务管理器失败（需要管理员权限）：{e}"))?;

        let svc = match OpenServiceW(scm, PCWSTR(name_w.as_ptr()), SERVICE_CHANGE_CONFIG) {
            Ok(h) => h,
            Err(e) => {
                let _ = CloseServiceHandle(scm);
                return Err(format!("打开服务失败：{e}"));
            }
        };

        let r = ChangeServiceConfigW(
            svc,
            windows::Win32::System::Services::ENUM_SERVICE_TYPE(SERVICE_NO_CHANGE),
            SERVICE_START_TYPE(start),
            SERVICE_ERROR(SERVICE_NO_CHANGE),
            PCWSTR::null(),
            PCWSTR::null(),
            None,
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
        );

        let _ = CloseServiceHandle(svc);
        let _ = CloseServiceHandle(scm);
        r.map_err(|e| format!("修改服务启动类型失败：{e}"))
    }
}

/// 读服务当前是否为延迟自动启动（注册表 `DelayedAutostart`）。
fn read_service_delayed(key_name: &str) -> Result<bool, String> {
    use winreg::enums::*;
    use winreg::RegKey;
    Ok(RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(format!(r"SYSTEM\CurrentControlSet\Services\{key_name}"))
        .map_err(|e| format!("打开服务项失败：{e}"))?
        .get_value::<u32, _>("DelayedAutostart")
        .unwrap_or(0)
        == 1)
}

/// 通过 SCM 设置/取消「延迟自动启动」。
///
/// 用 `ChangeServiceConfig2W(SERVICE_CONFIG_DELAYED_AUTO_START_INFO)`，
/// 而不是写注册表 —— 写注册表不会通知 SCM，属不规范做法。
/// 该设置只在**下次开机**生效，不会打断正在运行的服务。
fn set_service_delayed(key_name: &str, on: bool) -> Result<(), String> {
    use windows::Win32::System::Services::{
        ChangeServiceConfig2W, CloseServiceHandle, OpenSCManagerW, OpenServiceW,
        SERVICE_CONFIG_DELAYED_AUTO_START_INFO, SERVICE_DELAYED_AUTO_START_INFO,
        SC_MANAGER_CONNECT, SERVICE_CHANGE_CONFIG,
    };
    use windows::core::PCWSTR;

    let name_w: Vec<u16> = key_name.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        let scm = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT)
            .map_err(|e| format!("打开服务管理器失败（需要管理员权限）：{e}"))?;

        let svc = match OpenServiceW(scm, PCWSTR(name_w.as_ptr()), SERVICE_CHANGE_CONFIG) {
            Ok(h) => h,
            Err(e) => {
                let _ = CloseServiceHandle(scm);
                return Err(format!("打开服务失败：{e}"));
            }
        };

        let info = SERVICE_DELAYED_AUTO_START_INFO {
            fDelayedAutostart: on.into(),
        };
        let r = ChangeServiceConfig2W(
            svc,
            SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
            Some(&info as *const _ as *const _),
        );

        let _ = CloseServiceHandle(svc);
        let _ = CloseServiceHandle(scm);
        r.map_err(|e| format!("设置延迟自动启动失败：{e}"))
    }
}

/// 调用 schtasks 改计划任务状态。
fn run_schtasks(verb: &str, uri: &str, flag: &str) -> Result<(), String> {
    // schtasks 接受不带前导反斜杠的任务名
    let tn = uri.trim_start_matches('\\');
    let out = std::process::Command::new("schtasks.exe")
        .args([verb, "/TN", tn, flag])
        .output()
        .map_err(|e| format!("调用 schtasks 失败：{e}"))?;
    if out.status.success() {
        return Ok(());
    }
    // 输出可能是 GBK，这里只取状态码，避免编码问题
    Err(format!(
        "schtasks 返回失败（退出码 {:?}）",
        out.status.code()
    ))
}

// ===========================================================================
// 还原
// ===========================================================================

/// 还原一个批次。返回 `(成功数, 失败数, 错误列表)`。
pub fn restore(batch: &str) -> (usize, usize, Vec<String>) {
    // 熔断闸必须在读取批次**之前**生效：
    // 演练模式下连「有没有这个批次」都不该去查，直接拒绝。
    if safety::is_dry_run() {
        return (0, 0, vec!["演练模式：已阻止还原操作".into()]);
    }

    let changes = match load_batch(batch) {
        Ok(c) => c,
        Err(e) => return (0, 0, vec![e]),
    };

    let mut ok = 0usize;
    let mut fail = 0usize;
    let mut errors = Vec::new();

    for c in &changes {
        if !c.ok {
            continue;
        }
        match undo_one(c, batch) {
            Ok(()) => ok += 1,
            Err(e) => {
                fail += 1;
                errors.push(format!("{}：{e}", c.name));
            }
        }
    }
    (ok, fail, errors)
}

/// 一条还原载荷「该怎么走」——**纯决策，不碰系统**。
///
/// 拆出来是为了可测：所有「这段载荷该走哪条路」的判断，都能在没有管理员
/// 权限、不改动任何东西的前提下断言清楚。执行部分（[`undo_one`]）于是只剩
/// 一句句系统调用，没有分支可藏 bug。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Undo {
    /// 把 `StartupApproved` 标志位写回原样（`None` = 原来就没有这个值 → 删掉它）
    Approved {
        hive: String,
        subkey: String,
        name: String,
        prev: Option<Vec<u8>>,
    },
    /// 服务启动类型写回
    ServiceStart { key: String, start: u32 },
    /// 延迟启动的撤销（`delayed` = 应该恢复成「是否延迟」）
    ServiceDelayed { key: String, delayed: bool },
    /// 计划任务：按当时的动作反向操作（`verb` 是给 schtasks 的参数）
    Task { uri: String, verb: &'static str },
    /// 被删除的注册表值：值本身 + 可能存在的标志位
    RegValue {
        hive: String,
        subkey: String,
        name: String,
        value: String,
        approved: Option<(String, String, String, Vec<u8>)>,
    },
    /// 启动文件夹快捷方式：从隔离目录挪回原位
    File { path: String },
}

/// 解析还原载荷，决定该怎么还原。
///
/// 载荷有两代形态：
///
/// * **带 `kind` 的新形态** —— 显式声明「这是什么」，按类型分派，不靠字段名猜。
/// * **不带 `kind` 的旧批次** —— 按字段名反推（函数末尾的兼容分支）。
///   旧批次是用户机器上真实存在的历史，不能因为改了格式就读不了。
fn undo_plan(c: &StartupChange) -> Result<Undo, String> {
    let v: serde_json::Value =
        serde_json::from_str(&c.undo).map_err(|e| format!("还原载荷损坏：{e}"))?;

    match v.get("kind").and_then(|x| x.as_str()).unwrap_or("") {
        "approved" => {
            let (hive, subkey, name) = parse_reg_ref(v.get("reg").and_then(|x| x.as_str()))?;
            return Ok(Undo::Approved {
                hive,
                subkey,
                name,
                prev: bytes_of(&v, "prev"),
            });
        }
        "start" => {
            return Ok(Undo::ServiceStart {
                key: req_str(&v, "key")?,
                start: req_u64(&v, "start")? as u32,
            });
        }
        "delayed" => {
            return Ok(Undo::ServiceDelayed {
                key: req_str(&v, "key")?,
                delayed: v
                    .get("delayed")
                    .and_then(|x| x.as_bool())
                    .ok_or_else(|| "还原载荷缺少延迟状态".to_string())?,
            });
        }
        "task" => {
            let uri = req_str(&v, "uri")?;
            // 方向由**当时的动作**决定：禁用过的要启用回来，启用过的要禁回去。
            // 因此载荷本身只要记「是哪一条任务」就够，不必存快照。
            let verb = match c.action.as_str() {
                "disable" => "/ENABLE",
                "enable" => "/DISABLE",
                other => return Err(format!("计划任务的「{other}」没有可回放的还原动作")),
            };
            return Ok(Undo::Task { uri, verb });
        }
        "regvalue" => {
            let (hive, subkey, name) = parse_reg_ref(v.get("reg").and_then(|x| x.as_str()))?;
            let approved = match (
                v.get("approved").and_then(|x| x.as_str()),
                bytes_of(&v, "approved_prev"),
            ) {
                (Some(a), Some(prev)) => {
                    let (ah, asub, an) = parse_reg_ref(Some(a))?;
                    Some((ah, asub, an, prev))
                }
                _ => None,
            };
            return Ok(Undo::RegValue {
                hive,
                subkey,
                name,
                value: req_str(&v, "value")?,
                approved,
            });
        }
        "file" => return Ok(Undo::File { path: req_str(&v, "path")? }),
        _ => {}
    }

    // ---- 兼容历史批次（没有 kind 字段） ----
    if let Some(uri) = v.get("uri").and_then(|x| x.as_str()) {
        // 旧形态固定按「启用回来」回放（当时只有禁用一条路）
        return Ok(Undo::Task {
            uri: uri.to_string(),
            verb: "/ENABLE",
        });
    }
    if let Some(delayed) = v.get("delayed").and_then(|x| x.as_bool()) {
        return Ok(Undo::ServiceDelayed {
            key: req_str(&v, "key")?,
            delayed,
        });
    }
    if let Some(start) = v.get("start").and_then(|x| x.as_u64()) {
        return Ok(Undo::ServiceStart {
            key: req_str(&v, "key")?,
            start: start as u32,
        });
    }
    if let Some(path) = v.get("path").and_then(|x| x.as_str()) {
        return Ok(Undo::File {
            path: path.to_string(),
        });
    }
    if let Some(value) = v.get("value").and_then(|x| x.as_str()) {
        let (hive, subkey, name) = parse_reg_ref(v.get("reg").and_then(|x| x.as_str()))?;
        return Ok(Undo::RegValue {
            hive,
            subkey,
            name,
            value: value.to_string(),
            approved: None,
        });
    }
    Err("无法识别的还原载荷".into())
}

/// 执行一条还原计划。
fn undo_one(c: &StartupChange, batch: &str) -> Result<(), String> {
    match undo_plan(c)? {
        Undo::Approved {
            hive,
            subkey,
            name,
            prev,
        } => write_approved(&hive, &subkey, &name, prev.as_deref()),
        Undo::ServiceStart { key, start } => set_service_start(&key, start),
        Undo::ServiceDelayed { key, delayed } => {
            // 已经是目标状态就当还原成功 —— 用户要的是结果，不是「又改了一遍」
            if read_service_delayed(&key).map(|d| d != delayed).unwrap_or(false) {
                set_service_delayed(&key, delayed)
            } else {
                Ok(())
            }
        }
        Undo::Task { uri, verb } => run_schtasks("/Change", &uri, verb),
        Undo::RegValue {
            hive,
            subkey,
            name,
            value,
            approved,
        } => {
            let root = hive_key(&hive)?;
            let key = winreg::RegKey::predef(root)
                .open_subkey_with_flags(
                    &subkey,
                    winreg::enums::KEY_SET_VALUE | winreg::enums::KEY_QUERY_VALUE,
                )
                .map_err(|e| format!("打开注册表项失败：{e}"))?;
            key.set_value(&name, &value.to_string())
                .map_err(|e| format!("回写注册表值失败：{e}"))?;
            if let Some((ah, asub, an, prev)) = approved {
                write_approved(&ah, &asub, &an, Some(&prev))?;
            }
            Ok(())
        }
        Undo::File { path } => restore_file(&path, batch),
    }
}

fn req_str(v: &serde_json::Value, key: &str) -> Result<String, String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("还原载荷缺少 {key}"))
}

fn req_u64(v: &serde_json::Value, key: &str) -> Result<u64, String> {
    v.get(key)
        .and_then(|x| x.as_u64())
        .ok_or_else(|| format!("还原载荷缺少 {key}"))
}

/// 把 `.json` 里的字节数组还原成 `Vec<u8>`。`null` / 缺字段 / 空数组都是 `None`。
fn bytes_of(v: &serde_json::Value, key: &str) -> Option<Vec<u8>> {
    let a = v.get(key)?.as_array()?;
    let out: Vec<u8> = a
        .iter()
        .filter_map(|x| x.as_u64().map(|n| n as u8))
        .collect();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// 解析 `HIVE\子键!值名`。
fn parse_reg_ref(s: Option<&str>) -> Result<(String, String, String), String> {
    let s = s.ok_or_else(|| "还原载荷缺少注册表位置".to_string())?;
    let (path, name) = s
        .split_once('!')
        .ok_or_else(|| format!("注册表定位串格式不对：{s}"))?;
    let (hive, subkey) = path
        .split_once('\\')
        .ok_or_else(|| format!("注册表定位串缺少根键：{s}"))?;
    Ok((hive.to_string(), subkey.to_string(), name.to_string()))
}

/// 恢复标志位：`prev` 有值就写回去，`None` 就删掉（真正的「回到原样」）。
fn write_approved(
    hive: &str,
    subkey: &str,
    name: &str,
    prev: Option<&[u8]>,
) -> Result<(), String> {
    let root = hive_key(hive)?;
    match prev {
        Some(bytes) => {
            let (key, _) = winreg::RegKey::predef(root)
                .create_subkey(subkey)
                .map_err(|e| format!("创建注册表项失败：{e}"))?;
            let v = winreg::RegValue {
                bytes: bytes.to_vec(),
                vtype: winreg::enums::REG_BINARY,
            };
            key.set_raw_value(name, &v)
                .map_err(|e| format!("回写启动状态失败：{e}"))
        }
        None => clear_approved(hive, subkey, name),
    }
}

/// 把快捷方式从隔离目录挪回启动文件夹。
fn restore_file(path: &str, batch: &str) -> Result<(), String> {
    let dst = PathBuf::from(path);
    let file_name = dst
        .file_name()
        .ok_or_else(|| "还原路径没有文件名".to_string())?;
    let src = quarantine_dir(batch).join(file_name);
    if !src.exists() {
        return Err(format!("隔离文件已不存在：{}", src.display()));
    }
    if dst.exists() {
        return Err(format!("原位置已有同名文件：{path}"));
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::rename(&src, &dst).map_err(|e| format!("移回文件失败：{e}"))
}

/// 清理某个批次的隔离文件目录（还原成功后调用）。
pub fn purge_quarantine(batch: &str) {
    let dir = quarantine_dir(batch);
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// 便捷封装：把「允许变更的 ID 集合」装上，执行，再卸掉。
///
/// 这是推荐的调用姿势 —— 白名单只在这一次操作期间存在。
pub fn apply_scoped(items: &[StartupItem], dry: bool) -> StartupResult {
    apply_scoped_with(items, dry, Mode::Disable)
}

/// 同上，但可指定动作（禁用 / 延迟启动）。
pub fn apply_scoped_with(items: &[StartupItem], dry: bool, mode: Mode) -> StartupResult {
    let ids: HashSet<String> = items.iter().map(|i| i.id.clone()).collect();
    safety::set_startup_allowlist(Some(ids));
    let r = apply_with(items, dry, mode);
    safety::set_startup_allowlist(None);
    r
}

// ===========================================================================
// 测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::SignatureStatus;

    fn item(source: StartupSource, name: &str, location: &str) -> StartupItem {
        StartupItem {
            id: crate::core::startup::stable_id(source, location, name),
            name: name.into(),
            command: r"C:\x\y.exe".into(),
            source,
            location: location.into(),
            publisher: None,
            signature: SignatureStatus::Unsigned,
            enabled: true,
            disabled_by_us: false,
            batch_id: None,
            risk: RiskLevel::Safe,
            risk_reason: "测试".into(),
            can_disable: true,
            impact_ms: None,
            signature_pending: false,
            delayed: false,
            knowledge: None,
            in_whitelist: false,
            in_blacklist: false,
            reason_prefix: String::new(),
        }
    }

    /// 32 位 Run 项的启用状态必须写到 `StartupApproved\Run32`。
    /// 写到 `Run` 的后果是「界面显示禁用成功、重启后照旧自启」——
    /// 这类静默失效比报错更糟，因为它看起来是好的。
    #[test]
    fn approved_target_uses_run32_for_32bit_keys() {
        let t32 = Target::RunValue {
            hive: "HKLM".into(),
            subkey: r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run".into(),
            value: "OldApp".into(),
        };
        let (hive, sub, name) = approved_target(&t32).unwrap();
        assert_eq!(hive, "HKLM");
        assert!(sub.ends_with(r"StartupApproved\Run32"), "实际写到：{sub}");
        assert_eq!(name, "OldApp");

        let t64 = Target::RunValue {
            hive: "HKLM".into(),
            subkey: r"Software\Microsoft\Windows\CurrentVersion\Run".into(),
            value: "NewApp".into(),
        };
        let (_, sub64, _) = approved_target(&t64).unwrap();
        assert!(sub64.ends_with(r"StartupApproved\Run"), "实际写到：{sub64}");
    }

    /// 权限预检：不存在的服务必须在**演练阶段**就报错，
    /// 而不是等用户点了「确认禁用」才弹出「拒绝访问 0x80070005」。
    #[test]
    fn probe_stops_unwritable_service_at_plan_time() {
        let t = Target::Service {
            key_name: "old666_no_such_service_xyz".into(),
        };
        let e = probe_writable(&t).expect_err("不存在的服务必须报错");
        assert!(e.contains("打不开该服务"), "{e}");

        let it = item(
            StartupSource::Service,
            "old666_no_such_service_xyz",
            r"HKLM\SYSTEM\CurrentControlSet\Services\old666_no_such_service_xyz",
        );
        let r = apply(&[it], true); // 演练
        assert!(r.dry_run);
        let c = &r.changes[0];
        assert!(!c.ok, "演练阶段就该判定为不可执行");
        let err = c.error.clone().unwrap_or_default();
        // 未提权时先被「需要管理员权限」拦下，提权后才是「打不开服务」——
        // 两种都必须发生在确认之前。
        if crate::core::is_elevated() {
            assert!(err.contains("打不开该服务"), "{err}");
        } else {
            assert!(err.contains("管理员"), "{err}");
        }
    }

    /// 普通用户的 HKCU Run 键永远可写，预检不该误报。
    #[test]
    fn probe_allows_writable_run_key() {
        let t = Target::RunValue {
            hive: "HKCU".into(),
            subkey: r"Software\Microsoft\Windows\CurrentVersion\Run".into(),
            value: "Whatever".into(),
        };
        assert!(probe_writable(&t).is_ok(), "HKCU Run 键应当可写");
    }

    #[test]
    fn parse_run_value_location() {
        let it = item(
            StartupSource::RunKey,
            "Foo",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run!Foo",
        );
        assert_eq!(
            parse_target(&it).unwrap(),
            Target::RunValue {
                hive: "HKCU".into(),
                subkey: r"Software\Microsoft\Windows\CurrentVersion\Run".into(),
                value: "Foo".into(),
            }
        );
    }

    #[test]
    fn parse_rejects_bad_locations() {
        // 缺少 ! 分隔
        assert!(parse_target(&item(StartupSource::RunKey, "A", "HKCU\\Run")).is_err());
        // 根键不认识
        assert!(parse_target(&item(StartupSource::RunKey, "A", "HKCR\\Run!A")).is_err());
        // 空值名
        assert!(parse_target(&item(StartupSource::RunKey, "A", "HKCU\\Run!")).is_err());
        // 服务定位串没有反斜杠
        assert!(parse_target(&item(StartupSource::Service, "A", "noslash")).is_err());
        // 计划任务没有名字
        assert!(parse_target(&item(StartupSource::ScheduledTask, "", "C:\\t.xml")).is_err());
    }

    #[test]
    fn parse_folder_and_task_and_service() {
        assert_eq!(
            parse_target(&item(StartupSource::StartupFolder, "a.lnk", r"C:\s\a.lnk")).unwrap(),
            Target::StartupFile {
                path: r"C:\s\a.lnk".into()
            }
        );
        assert_eq!(
            parse_target(&item(
                StartupSource::Service,
                "Foo",
                r"HKLM\SYSTEM\CurrentControlSet\Services\FooSvc"
            ))
            .unwrap(),
            Target::Service {
                key_name: "FooSvc".into()
            }
        );
        assert_eq!(
            parse_target(&item(StartupSource::ScheduledTask, r"\Microsoft\X", "C:\\t.xml")).unwrap(),
            Target::Task {
                uri: r"\Microsoft\X".into()
            }
        );
    }

    #[test]
    fn admin_requirement_is_per_source() {
        assert!(!needs_admin(&Target::StartupFile { path: "x".into() }));
        assert!(!needs_admin(&Target::RunValue {
            hive: "HKCU".into(),
            subkey: "k".into(),
            value: "v".into()
        }));
        assert!(needs_admin(&Target::RunValue {
            hive: "HKLM".into(),
            subkey: "k".into(),
            value: "v".into()
        }));
        assert!(needs_admin(&Target::Service {
            key_name: "s".into()
        }));
        assert!(needs_admin(&Target::Task { uri: "u".into() }));
    }

    /// 造一个真实存在的临时「启动文件夹条目」。
    ///
    /// 之所以不用注册表项：RunValue 的 dry-run 也要先读出原值才能给出还原载荷，
    /// 而造一个真实注册表值就等于写系统。用临时文件既真实又零副作用。
    fn temp_startup_file(tag: &str) -> (PathBuf, StartupItem) {
        let dir = std::env::temp_dir().join(format!(
            "old666-ctl-test-{}-{tag}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join(format!("{tag}.lnk"));
        std::fs::write(&f, b"dummy lnk").unwrap();
        let it = item(
            StartupSource::StartupFolder,
            &format!("{tag}.lnk"),
            &f.display().to_string(),
        );
        (f, it)
    }

    /// 核心安全断言：演练模式下 apply 绝不能真的动系统。
    #[test]
    fn dry_run_never_touches_system() {
        let (file, it) = temp_startup_file("dry");
        let r = apply(&[it], true);

        assert!(r.dry_run);
        assert!(r.batch_id.is_empty(), "演练不应产生批次 ID");
        assert!(r.changes.iter().all(|c| c.ok), "{:?}", r.changes);
        // 演练必须给出可还原载荷，否则用户看不到「打算怎么还原」
        assert!(r.changes.iter().all(|c| !c.undo.is_empty()));
        // 最关键的一条：文件必须原地不动
        assert!(file.exists(), "演练模式不该移动任何文件");

        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    /// 即使调用方忘了传 dry，安全闸也必须挡住真实写入（注册表路径）。
    #[test]
    fn real_apply_is_blocked_by_circuit_breaker() {
        let items = vec![item(
            StartupSource::RunKey,
            "NeverExists_OLD666",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run!NeverExists_OLD666",
        )];
        let r = apply(&items, false);
        assert!(!r.dry_run);
        // 熔断生效 → 没有任何一条真正成功
        assert!(
            r.changes.iter().all(|c| !c.ok),
            "测试构建下不应有任何真实变更：{:?}",
            r.changes
        );
        for c in &r.changes {
            assert!(
                c.error.as_deref().unwrap_or("").contains("演练模式"),
                "错误信息应说明被熔断拦下，实际：{:?}",
                c.error
            );
        }
    }

    /// 同一道闸对文件类目标也必须生效。
    #[test]
    fn real_apply_is_blocked_for_files_too() {
        let (file, it) = temp_startup_file("real");
        let r = apply(&[it], false);
        assert!(!r.dry_run);
        assert!(r.changes.iter().all(|c| !c.ok), "{:?}", r.changes);
        assert!(file.exists(), "熔断生效时文件必须原地不动");

        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn dangerous_item_is_refused_even_in_dry_run() {
        let mut it = item(
            StartupSource::RunKey,
            "Defender",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run!Defender",
        );
        it.risk = RiskLevel::Dangerous;
        it.can_disable = false;
        let r = apply(&[it], true);
        assert!(!r.changes[0].ok);
        assert!(r.changes[0].error.as_deref().unwrap().contains("不建议禁用"));
    }

    #[test]
    fn already_disabled_item_is_skipped() {
        let mut it = item(
            StartupSource::RunKey,
            "X",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run!X",
        );
        it.enabled = false;
        let r = apply(&[it], true);
        assert!(!r.changes[0].ok);
        assert!(r.changes[0].error.as_deref().unwrap().contains("已经处于禁用状态"));
    }

    #[test]
    fn batch_id_shape_and_uniqueness() {
        let a = new_batch_id();
        let b = new_batch_id();
        assert_ne!(a, b, "同毫秒内批次 ID 也必须不同");
        assert!(a.starts_with("sb-"));
    }

    #[test]
    fn scoped_apply_revokes_allowlist_afterwards() {
        let items = vec![item(
            StartupSource::RunKey,
            "X",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run!X",
        )];
        let _ = apply_scoped(&items, true);
        // 撤销后，安全闸应回到「只剩熔断」的状态
        let err = safety::check_startup_change("whatever").unwrap_err();
        assert!(err.contains("演练模式"), "白名单应已撤销，实际：{err}");
    }

    #[test]
    fn restore_is_blocked_under_circuit_breaker() {
        let (ok, fail, errs) = restore("sb-nonexistent");
        // 演练模式下先被熔断拦下（不会因为批次不存在而误报）
        assert_eq!(ok, 0);
        assert_eq!(fail, 0);
        assert!(errs.iter().any(|e| e.contains("演练模式")));
    }

    // ---- 延迟启动 ----

    fn svc_item(key_name: &str) -> StartupItem {
        item(
            StartupSource::Service,
            key_name,
            &format!(r"HKLM\SYSTEM\CurrentControlSet\Services\{key_name}"),
        )
    }

    #[test]
    fn mode_parses_known_actions_only() {
        assert_eq!(Mode::parse("disable"), Some(Mode::Disable));
        assert_eq!(Mode::parse(""), Some(Mode::Disable));
        assert_eq!(Mode::parse("enable"), Some(Mode::Enable));
        assert_eq!(Mode::parse("delete"), Some(Mode::Delete));
        assert_eq!(Mode::parse("remove"), Some(Mode::Delete));
        assert_eq!(Mode::parse("delay"), Some(Mode::Delayed { on: true }));
        assert_eq!(Mode::parse("undelay"), Some(Mode::Delayed { on: false }));
        assert_eq!(Mode::parse("nonsense"), None);
    }

    /// 「启用」不该走风险闸：它只是把系统恢复原样，不可能让机器更脆弱。
    #[test]
    fn enable_never_needs_risk_gate() {
        assert!(!Mode::Enable.needs_risk_gate());
        for m in [
            Mode::Disable,
            Mode::Delete,
            Mode::Delayed { on: true },
            Mode::Delayed { on: false },
        ] {
            assert!(m.needs_risk_gate(), "{:?} 应该走风险闸", m);
        }
    }

    /// 方向一致性：禁用只作用于启用的、启用只作用于禁用的、删除两者都行。
    #[test]
    fn mode_direction_requirements() {
        assert!(Mode::Disable.requires_enabled());
        assert!(Mode::Delayed { on: true }.requires_enabled());
        assert!(Mode::Enable.requires_disabled());
        assert!(!Mode::Delete.requires_enabled());
        assert!(!Mode::Delete.requires_disabled());
        // 一个动作不可能同时要求两种状态
        for m in [Mode::Disable, Mode::Enable, Mode::Delete] {
            assert!(!(m.requires_enabled() && m.requires_disabled()));
        }
    }

    // ---- 禁用 / 启用 / 删除 的方向与载荷 ----

    /// **不变式**：禁用注册表项**不能删值**。
    ///
    /// 删了值条目就从列表里消失，用户再也找不到「启用」入口；
    /// 而且软件下次启动常会把值写回来，禁用形同虚设。
    /// 所以禁用必须走 `StartupApproved` 标志位 —— 载荷里必须是 `approved`。
    #[test]
    fn disable_keeps_the_value_and_uses_approved_flag() {
        let it = item(
            StartupSource::RunKey,
            "NeverExists_OLD666",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run!NeverExists_OLD666",
        );
        let r = apply_with(&[it], true, Mode::Disable);
        assert!(r.changes[0].ok, "{:?}", r.changes);
        let undo: serde_json::Value = serde_json::from_str(&r.changes[0].undo).unwrap();
        assert_eq!(undo["kind"], "approved");
        assert!(
            undo["reg"].as_str().unwrap().contains("StartupApproved"),
            "定位串必须指向 StartupApproved，实际：{}",
            undo["reg"]
        );
        // 演练模式下不该真的写注册表 —— prev 是「键不存在」的 None
        assert!(undo["prev"].is_null());
    }

    /// 删除走的是**另一条**路径：真要删值，但必须先把原值备份进载荷。
    #[test]
    fn delete_backs_up_the_original_value() {
        // 值不存在 → 必须先报「读不到原值」而不是硬删
        let it = item(
            StartupSource::RunKey,
            "NeverExists_OLD666",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run!NeverExists_OLD666",
        );
        let r = apply_with(&[it], true, Mode::Delete);
        assert!(!r.changes[0].ok, "不存在的值不该被当成删除成功");
        assert!(r.changes[0].error.as_deref().unwrap().contains("读取原值失败"));
    }

    /// 服务 / 计划任务明确拒绝删除，并引导用户去用「禁用」。
    #[test]
    fn delete_is_refused_for_service_and_task() {
        let e = delete_one(
            &Target::Service {
                key_name: "SomeSvc".into(),
            },
            "b",
            true,
        )
        .unwrap_err();
        assert!(e.contains("不支持删除") && e.contains("禁用"), "{e}");

        let e = delete_one(
            &Target::Task {
                uri: r"\Microsoft\X".into(),
            },
            "b",
            true,
        )
        .unwrap_err();
        assert!(e.contains("不支持删除") && e.contains("禁用"), "{e}");
    }

    /// 启用已启用的条目 → 明确报错，不静默改方向。
    #[test]
    fn enable_on_enabled_item_is_refused() {
        let it = item(
            StartupSource::RunKey,
            "X",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run!X",
        );
        let r = apply_with(&[it], true, Mode::Enable);
        assert!(!r.changes[0].ok);
        assert!(r.changes[0]
            .error
            .as_deref()
            .unwrap()
            .contains("当前就是启用状态"));
    }

    /// 「启用」必须能作用在**危险项**上 —— 否则用户开不回来自己关掉的驱动服务。
    #[test]
    fn enable_is_allowed_for_dangerous_item() {
        let mut it = item(
            StartupSource::RunKey,
            "Defender",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run!Defender",
        );
        it.risk = RiskLevel::Dangerous;
        it.can_disable = false;
        it.enabled = false;
        let r = apply_with(&[it], true, Mode::Enable);
        assert!(r.changes[0].ok, "{:?}", r.changes);
    }

    /// 黑名单是用户自己表过的态 —— 它能授权禁用危险项，但不能授权**删除**。
    #[test]
    fn blacklist_authorises_disable_but_not_delete() {
        let base = || {
            let mut it = item(
                StartupSource::RunKey,
                "Defender",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run!Defender",
            );
            it.risk = RiskLevel::Dangerous;
            it.can_disable = false;
            it.in_blacklist = true;
            it
        };

        let r = apply_with(&[base()], true, Mode::Disable);
        assert!(r.changes[0].ok, "{:?}", r.changes);

        let r = apply_with(&[base()], true, Mode::Delete);
        assert!(!r.changes[0].ok, "黑名单不该授权不可逆的删除");
        assert!(r.changes[0].error.as_deref().unwrap().contains("不建议禁用"));
    }

    /// 留痕必须带上 location / command —— 条目被禁用后要从留痕重建回列表，
    /// 少了这两样就成了「只看得见名字、不知道动哪里」的空壳。
    #[test]
    fn change_records_carry_location_and_command() {
        let it = item(
            StartupSource::RunKey,
            "X",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run!X",
        );
        let r = apply_with(&[it], true, Mode::Disable);
        assert_eq!(r.changes[0].location, r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run!X");
        assert_eq!(r.changes[0].command, r"C:\x\y.exe");

        // 连失败的那条也要带上 —— 用户看的是「这一条为什么没成」
        let mut bad = item(
            StartupSource::RunKey,
            "Y",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run!Y",
        );
        bad.enabled = false;
        let r = apply_with(&[bad], true, Mode::Disable);
        assert!(!r.changes[0].ok);
        assert_eq!(r.changes[0].command, r"C:\x\y.exe");
    }

    /// 旧批次（没有 `kind` 字段）必须仍然能解析出还原动作 ——
    /// 用户机器上有真历史，不能因为换了载荷格式就读不了。
    #[test]
    fn legacy_undo_payloads_still_dispatch() {
        let mk = |action: &str, undo: &str| StartupChange {
            item_id: "x".into(),
            name: "n".into(),
            source: StartupSource::Service,
            action: action.into(),
            undo: undo.into(),
            ok: true,
            error: None,
            location: String::new(),
            command: String::new(),
        };

        assert_eq!(
            undo_plan(&mk("disable", r#"{"key":"Svc","start":2}"#)).unwrap(),
            Undo::ServiceStart {
                key: "Svc".into(),
                start: 2
            }
        );
        assert_eq!(
            undo_plan(&mk("delay", r#"{"key":"Svc","delayed":false}"#)).unwrap(),
            Undo::ServiceDelayed {
                key: "Svc".into(),
                delayed: false
            }
        );
        assert_eq!(
            undo_plan(&mk("disable", r#"{"uri":"\\Microsoft\\X","enabled":true}"#)).unwrap(),
            Undo::Task {
                uri: r"\Microsoft\X".into(),
                verb: "/ENABLE"
            }
        );
        assert_eq!(
            undo_plan(&mk("disable", r#"{"path":"C:\\s\\a.lnk"}"#)).unwrap(),
            Undo::File {
                path: r"C:\s\a.lnk".into()
            }
        );
        assert_eq!(
            undo_plan(&mk(
                "disable",
                r#"{"reg":"HKCU\\Run!A","value":"C:\\a.exe"}"#
            ))
            .unwrap(),
            Undo::RegValue {
                hive: "HKCU".into(),
                subkey: "Run".into(),
                name: "A".into(),
                value: r"C:\a.exe".into(),
                approved: None,
            }
        );

        // 缺 reg 的旧载荷要明确报错，而不是静默放过
        let e = undo_plan(&mk("disable", r#"{"value":"C:\\a.exe"}"#)).unwrap_err();
        assert!(e.contains("缺少注册表位置"), "{e}");
        // 完全不认识的载荷
        let e = undo_plan(&mk("disable", r#"{"whatever":1}"#)).unwrap_err();
        assert!(e.contains("无法识别"), "{e}");
        // 损坏的 JSON
        assert!(undo_plan(&mk("disable", "{")).is_err());
    }

    /// 计划任务的还原方向必须跟**当时的动作**相反，不能一律按「启用」回放。
    #[test]
    fn task_undo_follows_the_original_action() {
        let mk = |action: &str| StartupChange {
            item_id: "x".into(),
            name: "n".into(),
            source: StartupSource::ScheduledTask,
            action: action.into(),
            undo: r#"{"kind":"task","uri":"\\Microsoft\\X"}"#.into(),
            ok: true,
            error: None,
            location: String::new(),
            command: String::new(),
        };
        assert_eq!(
            undo_plan(&mk("disable")).unwrap(),
            Undo::Task {
                uri: r"\Microsoft\X".into(),
                verb: "/ENABLE"
            }
        );
        assert_eq!(
            undo_plan(&mk("enable")).unwrap(),
            Undo::Task {
                uri: r"\Microsoft\X".into(),
                verb: "/DISABLE"
            }
        );
        assert!(undo_plan(&mk("delay")).is_err());
    }

    /// 新载荷的完整解析：标志位（含 null = 原本没有）、删除的注册表值（含标志位备份）。
    #[test]
    fn new_undo_payloads_round_trip() {
        let mk = |undo: &str| StartupChange {
            item_id: "x".into(),
            name: "n".into(),
            source: StartupSource::RunKey,
            action: "disable".into(),
            undo: undo.into(),
            ok: true,
            error: None,
            location: String::new(),
            command: String::new(),
        };

        assert_eq!(
            undo_plan(&mk(
                r#"{"kind":"approved","reg":"HKCU\\Run!A","prev":null}"#
            ))
            .unwrap(),
            Undo::Approved {
                hive: "HKCU".into(),
                subkey: "Run".into(),
                name: "A".into(),
                prev: None,
            }
        );
        assert_eq!(
            undo_plan(&mk(
                r#"{"kind":"approved","reg":"HKCU\\Run!A","prev":[3,0,0,0]}"#
            ))
            .unwrap(),
            Undo::Approved {
                hive: "HKCU".into(),
                subkey: "Run".into(),
                name: "A".into(),
                prev: Some(vec![3, 0, 0, 0]),
            }
        );
        // 删除：原值 + 标志位都要能回放
        assert_eq!(
            undo_plan(&mk(
                r#"{"kind":"regvalue","reg":"HKCU\\Run!A","value":"C:\\a.exe","approved":"HKCU\\SA!A","approved_prev":[2,0,0,0]}"#
            ))
            .unwrap(),
            Undo::RegValue {
                hive: "HKCU".into(),
                subkey: "Run".into(),
                name: "A".into(),
                value: r"C:\a.exe".into(),
                approved: Some((
                    "HKCU".into(),
                    "SA".into(),
                    "A".into(),
                    vec![2, 0, 0, 0]
                )),
            }
        );
    }

    /// 批次 ID 来自前端，必须挡住路径穿越。
    #[test]
    fn batch_id_cannot_escape_the_data_dir() {
        for bad in ["../evil", r"..\evil", "C:\\evil", "a/b", "", "  "] {
            assert!(batch_path(bad).is_err(), "「{bad}」不该被接受");
        }
        assert!(batch_path("sb-1a2b-0-3c4d").is_ok());
    }

    /// 幽灵条目闸：目标已经不在系统里时不许重建。
    #[test]
    fn target_alive_rejects_vanished_targets() {
        assert!(!target_alive(
            StartupSource::RunKey,
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run!NeverExists_OLD666"
        ));
        assert!(!target_alive(
            StartupSource::Service,
            r"HKLM\SYSTEM\CurrentControlSet\Services\NoSuchService_OLD666"
        ));
        assert!(!target_alive(
            StartupSource::StartupFolder,
            r"C:\definitely\not\here.lnk"
        ));
        // 已有的系统服务必须认得出来
        let any = crate::core::startup::read_services()
            .into_iter()
            .find(|s| s.start == crate::core::startup::START_AUTO);
        if let Some(s) = any {
            assert!(target_alive(
                StartupSource::Service,
                &format!(r"HKLM\SYSTEM\CurrentControlSet\Services\{}", s.key_name)
            ));
        }
    }

    /// 非服务来源在延迟模式下必须逐条拒绝，不能静默跳过。
    #[test]
    fn delay_rejects_non_service_sources() {
        let it = item(
            StartupSource::RunKey,
            "Foo",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run!Foo",
        );
        let r = apply_with(&[it], true, Mode::Delayed { on: true });
        assert!(r.dry_run);
        assert!(!r.changes[0].ok);
        assert!(r.changes[0]
            .error
            .as_deref()
            .unwrap_or("")
            .contains("不支持延迟启动"));
    }

    /// 不存在的服务必须在演练模式就被拒绝，并给出可读原因。
    #[test]
    fn delay_of_missing_service_fails_in_dry_run() {
        let it = svc_item("NoSuchService_OLD666");
        let r = apply_with(&[it], true, Mode::Delayed { on: true });
        assert!(!r.changes[0].ok);
        assert!(r.changes[0].error.is_some());
    }

    /// 撤销载荷的两种形态必须能区分：禁用存 start，延迟存 delayed。
    #[test]
    fn undo_payload_shape_distinguishes_modes() {
        let disable = change_one(
            &Target::Service {
                key_name: "x".into(),
            },
            "b",
            true,
            Mode::Disable,
        );
        assert!(disable.is_err(), "假服务读不到 Start 就该报错");

        // 用延迟模式验证载荷键位（读不到服务同样报错，因此只验证错误分支）
        let delay = change_one(
            &Target::Service {
                key_name: "x".into(),
            },
            "b",
            true,
            Mode::Delayed { on: true },
        );
        assert!(delay.is_err());
    }

    /// 取消延迟：演练模式同样不动系统，action 名要写进留痕。
    #[test]
    fn undelay_action_name_is_recorded() {
        let it = svc_item("NoSuchService_OLD666");
        let r = apply_with(&[it], true, Mode::Delayed { on: false });
        assert_eq!(r.changes[0].action, "undelay");
    }

    /// 危险服务即使走延迟也不许动。
    #[test]
    fn delay_respects_dangerous_flag() {
        let mut it = svc_item("SomeSvc");
        it.risk = RiskLevel::Dangerous;
        it.can_disable = false;
        let r = apply_with(&[it], true, Mode::Delayed { on: true });
        assert!(!r.changes[0].ok);
        assert!(r.changes[0].error.as_deref().unwrap().contains("不建议禁用"));
    }
}
