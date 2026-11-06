//! 启动项枚举 —— 四类来源统一成一个 `StartupItem` 列表。
//!
//! | 来源 | 定位方式 | 启用状态判定 |
//! |---|---|---|
//! | 注册表 Run/RunOnce | `HKCU`/`HKLM` × `Wow6432Node`，共 6 处 | `Explorer\StartupApproved\Run` 的二进制首字节奇偶 |
//! | 启动文件夹 | 用户 + 公共 两个 Startup 目录 | `StartupApproved\StartupFolder` |
//! | 自动启动服务 | `HKLM\SYSTEM\CurrentControlSet\Services` 里 `Start == 2` | `Start == 4` 为已禁用 |
//! | 计划任务 | `System32\Tasks` 下 XML，取 Boot/Logon 触发器 | XML 里的 `<Enabled>` |
//!
//! ## 为什么服务走注册表而不是 SCM
//!
//! `EnumServicesStatusExW` 只能给出「服务是否在跑」，要拿启动类型还得对每个服务
//! 单独 `QueryServiceConfigW`。注册表里的 `Start` 值就是启动类型本身，
//! 一次遍历几百个子键即可，快一个数量级。
//! **但禁用操作仍走 SCM 的 `ChangeServiceConfig`**（见 `startup_ctl.rs`），
//! 因为直接改注册表不会通知服务控制管理器，属于不规范做法。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::core::signature;
use crate::core::startup_risk::{self, assess, exe_from_command, expand_env};
use crate::core::types::{SignatureStatus, StartupItem, StartupScan, StartupSource};

// ===========================================================================
// 工具
// ===========================================================================

/// 稳定 ID：来源 + 定位键 + 名称 的 FNV-1a 哈希。
///
/// 跨扫描不变，禁用与还原靠它对齐；换机器也不会撞车（定位键含绝对路径）。
pub fn stable_id(source: StartupSource, location: &str, name: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let seed = format!("{}|{}|{}", source.as_str(), location, name);
    for b in seed.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:016x}", h)
}

/// 注册表路径是否表示「已禁用」。
///
/// Task Manager 把禁用项写成 `03 00 00 00 …`，启用项写成 `02 00 00 00 …`，
/// 判据就是首字节的最低位。键不存在 = 从未被改动 = 启用。
fn approved_is_disabled(data: &[u8]) -> bool {
    match data.first() {
        Some(b) => b & 0x01 == 1,
        None => false,
    }
}

/// 读注册表里的二进制值。
fn read_approved(root: winreg::HKEY, sub: &str) -> HashMap<String, bool> {
    use winreg::RegKey;

    let mut out = HashMap::new();
    let Ok(key) = RegKey::predef(root).open_subkey(sub) else {
        return out;
    };
    for (name, val) in key.enum_values().filter_map(Result::ok) {
        out.insert(name.to_ascii_lowercase(), approved_is_disabled(&val.bytes));
    }
    out
}

/// 解析形如 `@%SystemRoot%\system32\foo.dll,-100` 的间接字符串。
fn resolve_indirect(s: &str) -> String {
    let s = s.trim();
    if !s.starts_with('@') {
        return s.to_string();
    }
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::SHLoadIndirectString;

    let src: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buf = [0u16; 512];
    let r = unsafe { SHLoadIndirectString(PCWSTR(src.as_ptr()), &mut buf, None) };
    if r.is_ok() {
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        if n > 0 {
            return String::from_utf16_lossy(&buf[..n]);
        }
    }
    // 解析不了就退回原始串，至少不是空白
    s.to_string()
}

/// 取可执行文件路径（用于查签名）。
fn exe_path_of(command: &str) -> Option<String> {
    let p = exe_from_command(command)?;
    let p = expand_env(&p);
    if Path::new(&p).exists() {
        Some(p)
    } else {
        None
    }
}

/// 未做签名校验的原始条目。
///
/// 拆成两步的原因：签名校验（`WinVerifyTrust` + `CryptQueryObject`）单条约 1–5 ms，
/// 148 条串行就是 0.5–1 s。而枚举本身（注册表 / 文件系统 / XML）只要几十毫秒。
/// 把重活摘出来并行跑，首屏耗时可降一个数量级。
#[derive(Debug, Clone)]
pub struct RawItem {
    pub source: StartupSource,
    pub name: String,
    pub command: String,
    pub location: String,
    pub enabled: bool,
    /// 附加到风险理由前面的前缀（如计划任务的「开机触发；」）
    pub reason_prefix: String,
    /// 仅服务有意义：注册表 `DelayedAutostart`。其他来源恒为 false。
    pub delayed: bool,
}

/// 给原始条目补上签名与风险分级。
///
/// * `deep = false`（快速扫描）—— 只跑不需要签名的规则链。规则链无解的条目
///   先给一个「正在确认发布者…」的临时分级，`signature_pending = true`，
///   等 UI 再调一次 [`verify_pending`] 补上真实结论。
/// * `deep = true` —— 一次性把签名也验完（CLI 与后台任务用）。
///
/// **为什么必须分两步**：`WinVerifyTrust` 要哈希整个文件才能验证 Authenticode。
/// 实测本机 148 个启动项里有 16 个需要验签，合计 **601 MB**
/// （LM Studio 200 MB、WorkBuddyAI 195 MB、FeverGames 78 MB…），
/// 串行 0.6 s、冷缓存下并行反而更慢（8 个线程抢同一块盘）。
/// 首屏不该为它阻塞。
fn hydrate(raw: &RawItem, deep: bool) -> StartupItem {
    let cheap = startup_risk::assess_cheap(&raw.name, &raw.command, raw.source);

    let (sig, assessment, pending) = match cheap {
        // 规则链已定论 —— 不验签，签名状态如实记为「无法判定」
        Some(a) => (SignatureStatus::Unknown, a, false),
        None if !deep => (
            SignatureStatus::Unknown,
            startup_risk::assess_provisional(&raw.name, &raw.command, raw.source),
            true,
        ),
        None => {
            let exe = exe_path_of(&raw.command);
            let sig = signature::verify_optional(exe.as_deref());
            let a = assess(&raw.name, &raw.command, raw.source, &sig);
            (sig, a, false)
        }
    };

    let publisher = match &sig {
        SignatureStatus::Valid(v) => Some(v.clone()),
        _ => None,
    };
    let can_disable = assessment.can_disable();
    let risk_reason = compose_reason(&raw.reason_prefix, &assessment.reason);
    StartupItem {
        id: stable_id(raw.source, &raw.location, &raw.name),
        name: raw.name.clone(),
        command: raw.command.clone(),
        source: raw.source,
        location: raw.location.clone(),
        publisher,
        signature: sig,
        enabled: raw.enabled,
        disabled_by_us: false,
        batch_id: None,
        risk: assessment.risk,
        risk_reason,
        can_disable,
        impact_ms: None,
        signature_pending: pending,
        delayed: raw.delayed,
        knowledge: None,
        in_whitelist: false,
        in_blacklist: false,
        reason_prefix: raw.reason_prefix.clone(),
    }
}

/// 把来源前缀与风险理由拼起来。前缀为空时原样返回理由，不引入多余分隔。
fn compose_reason(prefix: &str, reason: &str) -> String {
    if prefix.is_empty() {
        reason.to_string()
    } else {
        format!("{prefix}{reason}")
    }
}

/// 并行补全，保持输入顺序。
///
/// 只在**不验签名**时真正并行（规则链是纯 CPU 计算，毫秒级）。
/// 一旦进入 deep 模式，签名校验是 I/O 密集的，多线程只会让磁头来回跳，
/// 因此退化为串行 —— 实测 8 线程 2.1 s，串行 0.6 s。
fn hydrate_all(raws: Vec<RawItem>, deep: bool) -> Vec<StartupItem> {
    let n = raws.len();
    if n == 0 {
        return Vec::new();
    }
    if deep {
        return raws.iter().map(|r| hydrate(r, true)).collect();
    }
    let hw = std::thread::available_parallelism()
        .map(|v| v.get())
        .unwrap_or(4)
        .clamp(1, 8);
    let threads = hw.min(n);
    if threads <= 1 {
        return raws.iter().map(|r| hydrate(r, false)).collect();
    }
    let chunk = n.div_ceil(threads);

    let mut indexed: Vec<(usize, StartupItem)> = Vec::with_capacity(n);
    std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(threads);
        for (ci, part) in raws.chunks(chunk).enumerate() {
            let base = ci * chunk;
            handles.push(s.spawn(move || {
                part.iter()
                    .enumerate()
                    .map(|(i, r)| (base + i, hydrate(r, false)))
                    .collect::<Vec<_>>()
            }));
        }
        for h in handles {
            match h.join() {
                Ok(v) => indexed.extend(v),
                // 某个线程 panic 不该让整次扫描失败：丢掉它负责的那批，
                // 上层看到的是「条目变少」，而不是整个页面报错。
                Err(_) => {}
            }
        }
    });
    indexed.sort_by_key(|(i, _)| *i);
    indexed.into_iter().map(|(_, it)| it).collect()
}

/// 补齐尚未校验签名的条目（原地更新），返回实际更新的条数。
///
/// `only_ids` 为 `Some` 时只处理这些 ID —— UI 通常只对当前可见/筛选后的行调用，
/// 避免为一个折叠起来的 200 MB 文件白读一遍。
///
/// 补齐后会**重新排序**：风险档可能从「谨慎」升到「可安全禁用」，
/// 不重排的话列表顺序会和颜色标签对不上。
pub fn verify_pending(items: &mut [StartupItem], only_ids: Option<&HashSet<String>>) -> usize {
    let mut updated = 0;
    for it in items.iter_mut() {
        if !it.signature_pending {
            continue;
        }
        if let Some(set) = only_ids {
            if !set.contains(&it.id) {
                continue;
            }
        }
        let exe = exe_path_of(&it.command);
        let sig = signature::verify_optional(exe.as_deref());
        let a = assess(&it.name, &it.command, it.source, &sig);
        it.publisher = match &sig {
            SignatureStatus::Valid(v) => Some(v.clone()),
            _ => None,
        };
        it.signature = sig;
        it.risk = a.risk;
        it.can_disable = a.can_disable();
        // 前缀必须带上 —— 否则计划任务的「登录触发；」会被丢掉
        it.risk_reason = compose_reason(&it.reason_prefix, &a.reason);
        it.signature_pending = false;
        updated += 1;
    }
    if updated > 0 {
        // 定档用了知识库，签名补完后必须**重新贴一遍**再排序。
        // 否则「先快速出列表、再补验签名」得到的理由，会跟一次深扫的结果不一致
        // —— 实测 360画报 就是这样：补验后理由退化成签名描述，
        //    而深扫给的是知识库结论「内置说明：360 画报，可以安全禁用」。
        crate::core::knowledge::apply_all(items);
        crate::core::startup_lists::Lists::load().apply_all(items);
        sort_items(items);
    }
    updated
}

// ===========================================================================
// 重建「被我们禁用的条目」
// ===========================================================================

/// 把「被本程序禁用、因而从系统里消失」的条目重建回列表，返回重建条数。
///
/// ## 为什么必须做这件事
///
/// 注册表项与启动文件夹的禁用走的是 `StartupApproved` 标志位，命令行本身还在，
/// 所以扫描天然还能看见它们（`enabled = false`）。但**服务**和**计划任务**
/// 没有独立的标志位 —— 服务禁用后 `Start = 4`，就不再满足这里的枚举条件
/// （`Start == START_AUTO`），于是彻底从列表里消失。
///
/// 结果就是：用户点了禁用，条目不见了，「启用」的入口也一起不见了 ——
/// 禁用成了单程票。这里用变更留痕把它补回来。
///
/// ## 判定规则
///
/// 批次按修改时间**从新到旧**遍历，每个条目**第一次**出现的那条成功记录
/// 说了算（同批次内后写的先看）。这样「禁用 → 启用 → 再禁用」的最终状态
/// 一定由最新那条决定，不会被更早的记录盖掉。
///
/// 只有 `action == "disable"` 的条目才会被重建：
/// * `enable` 说明现在它是启用的，扫描本来就看得见；
/// * `delete` 是用户主动「彻底删掉」，不该再冒出来（要找回请走变更记录）。
///
/// 另外还会查一次目标是否真的还在（[`crate::core::startup_ctl::target_alive`]）——
/// 软件卸载之后，留痕里那条记录还在，但注册表值 / 服务早就没了，
/// 不查就会在列表里留下一堆点「启用」必然失败的死条目。
fn merge_disabled_by_us(items: &mut Vec<StartupItem>) -> usize {
    let existing: HashSet<String> = items.iter().map(|i| i.id.clone()).collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut revived: Vec<StartupItem> = Vec::new();

    for batch in crate::core::startup_ctl::list_batches() {
        let Ok(changes) = crate::core::startup_ctl::load_batch(&batch) else {
            continue;
        };
        for c in changes.iter().rev() {
            if !c.ok || !seen.insert(c.item_id.clone()) {
                continue;
            }
            if c.action != "disable" || existing.contains(&c.item_id) {
                continue;
            }
            // 旧批次没有 location / command —— 重建不出「该动哪里」，
            // 与其显示一个点不动的空壳，不如不显示
            if c.location.trim().is_empty() || c.command.trim().is_empty() {
                continue;
            }
            if !crate::core::startup_ctl::target_alive(c.source, &c.location) {
                continue;
            }
            if revived.len() >= 500 {
                break; // 留痕理论上不会长到这一步，兜个底免得列表被撑爆
            }
            revived.push(StartupItem {
                id: c.item_id.clone(),
                name: c.name.clone(),
                command: c.command.clone(),
                source: c.source,
                location: c.location.clone(),
                publisher: None,
                signature: SignatureStatus::Unknown,
                enabled: false,
                disabled_by_us: true,
                batch_id: Some(batch.clone()),
                // 风险档不再重算：它此刻不在系统里，重算也没有意义。
                // 关键是它能被「一键启用」，所以 can_disable 必须为 true。
                risk: crate::core::types::RiskLevel::Unknown,
                risk_reason: "由老六禁用的启动项，条目已从系统移除，可一键启用".into(),
                can_disable: true,
                impact_ms: None,
                signature_pending: false,
                delayed: false,
                knowledge: None,
                in_whitelist: false,
                in_blacklist: false,
                reason_prefix: String::new(),
            });
        }
    }

    let n = revived.len();
    items.extend(revived);
    n
}

// ===========================================================================
// ① 注册表 Run / RunOnce
// ===========================================================================

/// `(根键, 精确子键路径, 根键名)`。
///
/// `location` 写成 `HKCU\Software\...\Run!值名`，用 `!` 分隔 ——
/// 这样禁用时能精确定位到「哪个键里的哪个值」，而不是靠显示名猜。
const RUN_KEYS: &[(winreg::HKEY, &str, &str)] = &[
    (
        winreg::enums::HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\CurrentVersion\Run",
        "HKCU",
    ),
    (
        winreg::enums::HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\CurrentVersion\RunOnce",
        "HKCU",
    ),
    (
        winreg::enums::HKEY_LOCAL_MACHINE,
        r"Software\Microsoft\Windows\CurrentVersion\Run",
        "HKLM",
    ),
    (
        winreg::enums::HKEY_LOCAL_MACHINE,
        r"Software\Microsoft\Windows\CurrentVersion\RunOnce",
        "HKLM",
    ),
    (
        winreg::enums::HKEY_LOCAL_MACHINE,
        r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run",
        "HKLM",
    ),
    (
        winreg::enums::HKEY_LOCAL_MACHINE,
        r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\RunOnce",
        "HKLM",
    ),
];

/// `StartupApproved` 下该用哪个子键来存/读某个 Run 键的启用状态。
///
/// **32 位与 64 位的标志位是分开的**：`WOW6432Node\...\Run` 里的值，
/// 任务管理器把状态写在 `StartupApproved\Run32`，而不是 `Run`。
/// 用错子键的后果是双向的 —— 读的时候把「已禁用」看成「已启用」
/// （用户以为还能再禁一次），写的时候更是「看起来禁用成功、实际毫无作用」。
///
/// 扫描与执行都走这一个函数，避免两边各写一套判断而再次错位。
pub fn approved_leaf(subkey: &str) -> &'static str {
    let s = subkey.to_ascii_lowercase();
    let once = s.contains("runonce");
    let x86 = s.contains("wow6432node");
    match (once, x86) {
        (true, true) => "RunOnce32",
        (true, false) => "RunOnce",
        (false, true) => "Run32",
        (false, false) => "Run",
    }
}

fn scan_run_keys() -> Vec<RawItem> {
    use winreg::RegKey;

    let mut out = Vec::new();
    for (root, sub, hive) in RUN_KEYS {
        let Ok(key) = RegKey::predef(*root).open_subkey(sub) else {
            continue;
        };
        // 每个 Run 键对应自己的 StartupApproved 子键（见 `approved_leaf`）
        let leaf = approved_leaf(sub);
        let approved = read_approved(
            *root,
            &format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\{leaf}"),
        );

        for (name, _) in key.enum_values().filter_map(Result::ok) {
            // 跳过 Windows 自己写的空占位值
            if name.starts_with('*') || name.trim().is_empty() {
                continue;
            }
            let cmd: String = match key.get_value(&name) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // 和 `normalize_image_path` 同理：注册表字符串可能在 NUL 之后
            // 带着上一次写入留下的残渣，展示前必须切干净。
            let cmd = trim_reg_string(&cmd).trim().to_string();
            if cmd.is_empty() {
                continue;
            }
            let disabled = approved
                .get(&name.to_ascii_lowercase())
                .copied()
                .unwrap_or(false);
            out.push(RawItem {
                source: StartupSource::RunKey,
                name: name.clone(),
                command: cmd,
                location: format!("{hive}\\{sub}!{name}"),
                enabled: !disabled,
                reason_prefix: String::new(),
                delayed: false,
            });
        }
    }
    out
}

// ===========================================================================
// ② 启动文件夹
// ===========================================================================

/// 启动文件夹的两个真实位置：`(目录, 是否用户级)`。
///
/// 公开出去是因为「禁用」要写 `StartupApproved\StartupFolder`，而用户级 /
/// 机器级的标志位分别存在 HKCU 与 HKLM 下 —— 写错根键会得到一个
/// 「看起来禁用成功、实际毫无作用」的结果，必须由同一处来源判定。
pub fn startup_dirs() -> Vec<(PathBuf, bool)> {
    // (目录, 是否用户级)
    let mut v = Vec::new();
    if let Ok(appdata) = std::env::var("APPDATA") {
        v.push((
            PathBuf::from(appdata)
                .join(r"Microsoft\Windows\Start Menu\Programs\Startup"),
            true,
        ));
    }
    if let Ok(pd) = std::env::var("PROGRAMDATA") {
        v.push((
            PathBuf::from(pd).join(r"Microsoft\Windows\Start Menu\Programs\Startup"),
            false,
        ));
    }
    v
}

/// 从 `.lnk` 里尽力挖出目标路径。
///
/// `.lnk` 是二进制格式，正规做法要 `IShellLink` COM。但这里的目标路径**只用于展示**
/// 与查签名，禁用操作只是把 `.lnk` 挪走，不依赖目标。
/// 因此用启发式：扫描文件里的 ASCII 与 UTF-16LE 可打印串，取最长的那个 `.exe` 路径。
fn lnk_target(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;

    let mut best: Option<String> = None;
    let mut consider = |s: String| {
        let s = s.trim().to_string();
        if s.len() < 6 || !s.to_ascii_lowercase().contains(".exe") {
            return;
        }
        // 必须像个路径：带盘符或 UNC
        let looks_like_path = {
            let b = s.as_bytes();
            (b.len() > 2 && b[1] == b':' && b[0].is_ascii_alphabetic()) || s.starts_with("\\\\")
        };
        if !looks_like_path {
            return;
        }
        if best.as_ref().map(|b| s.len() > b.len()).unwrap_or(true) {
            best = Some(s);
        }
    };

    // ASCII 串
    let mut cur = Vec::new();
    for &b in &bytes {
        if (0x20..0x7f).contains(&b) {
            cur.push(b);
        } else {
            if cur.len() >= 6 {
                consider(String::from_utf8_lossy(&cur).to_string());
            }
            cur.clear();
        }
    }
    if cur.len() >= 6 {
        consider(String::from_utf8_lossy(&cur).to_string());
    }

    // UTF-16LE 串
    let mut i = 0;
    while i + 1 < bytes.len() {
        let mut units: Vec<u16> = Vec::new();
        while i + 1 < bytes.len() {
            let u = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
            if u >= 0x20 && u < 0x7f {
                units.push(u);
                i += 2;
            } else {
                break;
            }
        }
        if units.len() >= 6 {
            consider(String::from_utf16_lossy(&units));
        }
        i += 2;
    }

    best
}

fn scan_startup_folders() -> Vec<RawItem> {
    let approved_user = read_approved(
        winreg::enums::HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder",
    );
    let approved_machine = read_approved(
        winreg::enums::HKEY_LOCAL_MACHINE,
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder",
    );

    let mut out = Vec::new();
    for (dir, is_user) in startup_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.filter_map(Result::ok) {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let file_name = entry.file_name().to_string_lossy().to_string();
            // desktop.ini 是文件夹元数据，不是启动项
            if file_name.eq_ignore_ascii_case("desktop.ini") {
                continue;
            }
            let cmd = lnk_target(&p).unwrap_or_else(|| p.display().to_string());
            let approved = if is_user {
                &approved_user
            } else {
                &approved_machine
            };
            let disabled = approved
                .get(&file_name.to_ascii_lowercase())
                .copied()
                .unwrap_or(false);
            out.push(RawItem {
                source: StartupSource::StartupFolder,
                name: file_name,
                command: cmd,
                location: p.display().to_string(),
                enabled: !disabled,
                reason_prefix: String::new(),
                delayed: false,
            });
        }
    }
    out
}

// ===========================================================================
// ③ 自动启动服务
// ===========================================================================

/// 服务启动类型（注册表 `Start` 值）。
pub const START_BOOT: u32 = 0;
pub const START_SYSTEM: u32 = 1;
pub const START_AUTO: u32 = 2;
pub const START_MANUAL: u32 = 3;
pub const START_DISABLED: u32 = 4;

/// 一个服务的原始信息（枚举与禁用都要用）。
#[derive(Debug, Clone)]
pub struct ServiceRaw {
    pub key_name: String,
    pub display: String,
    pub image_path: String,
    pub start: u32,
    pub delayed: bool,
    /// `Parameters\ServiceDll` —— 只有 svchost 承载的服务才有。
    ///
    /// **为什么必须读它**：svchost 服务的 `ImagePath` 只是
    /// `svchost.exe -k <组名>`，一行都看不出它是谁家的东西。
    /// 于是「360 驱动大师」「360 温度监控」「迅雷下载基础服务」这类
    /// 第三方服务，因为路径落在 `C:\Windows\System32\svchost.exe` 上，
    /// 全被当成「位于系统目录的系统服务」判了高风险 ——
    /// 105 个自动服务里 101 个被判「不建议禁用」，一大半是这么来的。
    pub service_dll: Option<String>,
}

/// 读取全部服务注册项。
pub fn read_services() -> Vec<ServiceRaw> {
    use winreg::enums::*;
    use winreg::RegKey;

    let Ok(root) = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SYSTEM\CurrentControlSet\Services")
    else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for name in root.enum_keys().filter_map(Result::ok) {
        let Ok(k) = root.open_subkey(&name) else {
            continue;
        };
        let start: u32 = k.get_value("Start").unwrap_or(START_MANUAL);
        let image_path: String = k
            .get_value("ImagePath")
            .or_else(|_| k.get_value("ImagePath".to_string()))
            .unwrap_or_default();
        let display_raw: String = k.get_value("DisplayName").unwrap_or_default();
        let delayed: u32 = k.get_value("DelayedAutostart").unwrap_or(0);
        let display = if display_raw.is_empty() {
            name.clone()
        } else {
            resolve_indirect(trim_reg_string(&display_raw))
        };
        let service_dll = read_service_dll(&k, &image_path);
        out.push(ServiceRaw {
            key_name: name,
            display,
            image_path,
            start,
            delayed: delayed == 1,
            service_dll,
        });
    }
    out
}

/// 取 svchost 承载服务的真实实现模块（`Parameters\ServiceDll`）。
///
/// 只有 `ImagePath` 指向 svchost 时才去读 —— 其他服务的 `ImagePath` 本身就是
/// 真实路径，`ServiceDll` 反而是无关紧要的附属信息。
fn read_service_dll(k: &winreg::RegKey, image_path: &str) -> Option<String> {
    if !image_path.to_ascii_lowercase().contains("svchost.exe") {
        return None;
    }
    let raw: String = k
        .open_subkey("Parameters")
        .ok()?
        .get_value("ServiceDll")
        .ok()?;
    let path = normalize_image_path(&raw);
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

fn scan_services() -> Vec<RawItem> {
    let mut out = Vec::new();
    for s in read_services() {
        // 只关心自动启动：Boot/System 驱动不是用户能动的东西，Manual 不影响开机
        if s.start != START_AUTO {
            continue;
        }
        // 驱动的 ImagePath 是 \SystemRoot\... 形式，补全后才有意义
        let cmd = normalize_image_path(&s.image_path);
        // svchost 承载的服务：`command` 换成真实实现模块。
        //
        // 这样做有两个好处，都是「用户到底看到了什么」层面的：
        //   1. 风险判定拿到了真路径 —— 否则一切 svchost 服务都被当成系统服务；
        //   2. 列表里显示的是 `...\360DrvMgr\DrvMgrService.dll`，
        //      而不是一行毫无信息量的 `svchost.exe -k Drv_Mgr_Service`。
        // 服务本身是谁、怎么禁用/还原，走的是 `location`（服务注册表键），
        // 不受这里换不换路径影响。
        let cmd = s.service_dll.clone().unwrap_or(cmd);
        out.push(RawItem {
            source: StartupSource::Service,
            name: s.display.clone(),
            command: cmd,
            location: format!("HKLM\\SYSTEM\\CurrentControlSet\\Services\\{}", s.key_name),
            enabled: true,
            reason_prefix: if s.delayed {
                "延迟启动，对开机影响较小；".to_string()
            } else {
                String::new()
            },
            delayed: s.delayed,
        });
    }
    out
}

/// 注册表字符串按 Windows 语义在第一个 NUL 处结束。
///
/// 实测 360 温度监控的 `ServiceDll` 值里带着 `\0ice` 尾巴 ——
/// 安装程序用短字符串覆盖长字符串时没缩短缓冲区留下的残渣
/// （`ice` 正是上一次写入时 `Service` 的最后三个字母）。
/// 不截断的话这个尾巴会一路传到界面上，用户看到的是
/// `...\TMService.dll ice` 这种莫名其妙的命令行。
fn trim_reg_string(s: &str) -> &str {
    match s.find('\0') {
        Some(i) => &s[..i],
        None => s,
    }
}

/// 把服务/驱动的 `ImagePath` 规整成真实文件路径。
///
/// 注册表里存的是内核对象路径，形态有 `\SystemRoot\...`、`\??\C:\...`、
/// `\System32\...` 等几种。**必须按前缀处理，不能全局替换** ——
/// 否则 `\System32\` 会把已经展开的 `C:\Windows\System32\` 再套一层。
fn normalize_image_path(p: &str) -> String {
    let mut s = trim_reg_string(p).trim().to_string();
    if s.is_empty() {
        return String::new();
    }
    if let Some(rest) = s.strip_prefix(r"\??\") {
        s = rest.to_string();
    }
    if let Some(rest) = s.strip_prefix(r"\SystemRoot\") {
        s = format!(r"C:\Windows\{rest}");
    } else if let Some(rest) = s.strip_prefix(r"\System32\") {
        s = format!(r"C:\Windows\System32\{rest}");
    } else if let Some(rest) = s.strip_prefix(r"\SystemRoot") {
        s = format!(r"C:\Windows{rest}");
    }
    expand_env(&s)
}

// ===========================================================================
// ④ 计划任务
// ===========================================================================

fn tasks_dir() -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    PathBuf::from(root).join(r"System32\Tasks")
}

/// 读任务 XML（文件是 UTF-16LE 带 BOM）。
fn read_task_xml(p: &Path) -> Option<String> {
    let bytes = std::fs::read(p).ok()?;
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        Some(String::from_utf16_lossy(&units))
    } else if bytes.len() >= 3 && bytes[0] == 0xEF && bytes[1] == 0xBB && bytes[2] == 0xBF {
        Some(String::from_utf8_lossy(&bytes[3..]).to_string())
    } else {
        Some(String::from_utf8_lossy(&bytes).to_string())
    }
}

/// 取 XML 里第一个标签的文本内容。
fn tag_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let i = xml.find(&open)? + open.len();
    let j = xml[i..].find(&close)? + i;
    Some(xml[i..j].trim().to_string())
}

fn scan_tasks() -> Vec<RawItem> {
    let root = tasks_dir();
    let mut out = Vec::new();
    walk_tasks(&root, &root, &mut out, 0);
    out
}

fn walk_tasks(dir: &Path, root: &Path, out: &mut Vec<RawItem>, depth: usize) {
    if depth > 8 || out.len() > 400 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.filter_map(Result::ok) {
        let p = entry.path();
        if p.is_dir() {
            walk_tasks(&p, root, out, depth + 1);
            continue;
        }
        let Some(xml) = read_task_xml(&p) else {
            continue;
        };
        // 只收开机 / 登录触发的任务
        let boot = xml.contains("<BootTrigger");
        let logon = xml.contains("<LogonTrigger");
        if !boot && !logon {
            continue;
        }
        let enabled = tag_text(&xml, "Enabled")
            .map(|v| !v.eq_ignore_ascii_case("false"))
            .unwrap_or(true);

        let uri = tag_text(&xml, "URI").unwrap_or_else(|| {
            p.strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('/', "\\")
        });
        let command = tag_text(&xml, "Command").unwrap_or_default();
        let args = tag_text(&xml, "Arguments").unwrap_or_default();
        let full = if args.is_empty() {
            command.clone()
        } else {
            format!("{command} {args}")
        };
        if command.is_empty() {
            continue;
        }
        let trigger = if boot { "开机" } else { "登录" };
        out.push(RawItem {
            source: StartupSource::ScheduledTask,
            name: uri,
            command: full,
            location: p.display().to_string(),
            enabled,
            reason_prefix: format!("{trigger}触发；"),
            delayed: false,
        });
    }
}

// ===========================================================================
// 汇总
// ===========================================================================

/// 扫描全部启动项（**快速模式**，不校验签名）。
///
/// 首屏用这个：148 个条目实测约 100 ms。待验签的条目会带
/// `signature_pending = true`，UI 应随后调 [`verify_pending`] 补齐。
pub fn scan() -> StartupScan {
    scan_with(false)
}

/// 扫描全部启动项并一次性验完签名（CLI / 后台任务用）。
///
/// 比快速模式多 0.6–2 s，取决于待验文件体积。
pub fn scan_deep() -> StartupScan {
    scan_with(true)
}

fn scan_with(deep: bool) -> StartupScan {
    let t0 = Instant::now();
    let mut stage: Vec<(String, u64)> = Vec::new();
    let mark = |name: &str, t: Instant, stage: &mut Vec<(String, u64)>| {
        stage.push((name.to_string(), t.elapsed().as_millis() as u64));
    };

    // 第一阶段：快速枚举原始条目（注册表 / 文件系统 / XML）
    let mut raws: Vec<RawItem> = Vec::new();

    let t = Instant::now();
    raws.extend(scan_run_keys());
    mark("注册表 Run 键", t, &mut stage);

    let t = Instant::now();
    raws.extend(scan_startup_folders());
    mark("启动文件夹", t, &mut stage);

    let t = Instant::now();
    raws.extend(scan_services());
    mark("自动启动服务", t, &mut stage);

    let t = Instant::now();
    raws.extend(scan_tasks());
    mark("计划任务", t, &mut stage);

    // 第二阶段：分级（deep 时含签名校验）
    let t = Instant::now();
    let mut items = hydrate_all(raws, deep);
    mark(
        if deep { "签名与风险分级" } else { "风险分级" },
        t,
        &mut stage,
    );

    // 第三阶段：把「被我们禁用、因而从系统里消失」的条目重建回来
    let t = Instant::now();
    let revived = merge_disabled_by_us(&mut items);
    mark(&format!("重建禁用记录（{revived}）"), t, &mut stage);

    // 第四阶段：贴说明与名单标记。
    //
    // **必须在排序之前**：知识库不只是「加一段说明」，它还会用人工维护的建议
    // 去校正风险档（见 `knowledge::reconcile_risk`）—— 风险档变了，
    // 排序键也就变了。早先这两步排在 `sort_items` 之后，
    // 结果列表里会冒出「红的排在绿的后面」这种破序。
    let t = Instant::now();
    crate::core::knowledge::apply_all(&mut items);
    crate::core::startup_lists::Lists::load().apply_all(&mut items);
    mark("知识库与名单", t, &mut stage);

    // 第五阶段：排序。风险低 → 高，同档按名字。
    sort_items(&mut items);

    let pending_signatures = items.iter().filter(|i| i.signature_pending).count();

    let mut counts: Vec<(String, usize)> = Vec::new();
    for s in [
        StartupSource::RunKey,
        StartupSource::StartupFolder,
        StartupSource::Service,
        StartupSource::ScheduledTask,
    ] {
        let n = items.iter().filter(|i| i.source == s).count();
        counts.push((s.as_str().to_string(), n));
    }

    let disable_candidates = count_disable_candidates(&items);

    StartupScan {
        items,
        counts,
        disable_candidates,
        elapsed_ms: t0.elapsed().as_millis() as u64,
        stage_ms: stage,
        needs_admin: !crate::core::is_elevated(),
        pending_signatures,
    }
}

/// 风险低的排前面（用户最该看的在最上），同级按来源、再按名称。
fn sort_items(items: &mut [StartupItem]) {
    let rank = |r: crate::core::types::RiskLevel| match r {
        crate::core::types::RiskLevel::Safe => 0,
        crate::core::types::RiskLevel::Caution => 1,
        crate::core::types::RiskLevel::Unknown => 2,
        crate::core::types::RiskLevel::Dangerous => 3,
    };
    items.sort_by(|a, b| {
        rank(a.risk)
            .cmp(&rank(b.risk))
            .then_with(|| a.source.as_str().cmp(b.source.as_str()))
            .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
    });
}

/// 「可安全禁用」候选数：已启用 + 允许禁用 + 已定为 Safe + 没被用户加白。
///
/// 白名单是「这个我要留着」的明确表态，风险引擎说可禁用也得听用户的 ——
/// 否则「一键选中推荐项」会把用户刚加白的条目又勾上。
pub fn count_disable_candidates(items: &[StartupItem]) -> usize {
    items
        .iter()
        .filter(|i| {
            i.enabled
                && i.can_disable
                && i.risk == crate::core::types::RiskLevel::Safe
                && !i.in_whitelist
        })
        .count()
}

/// 把开机归因里的对象名，回填到同名启动项上（UI 直接显示「它拖慢了 1.2 秒」）。
pub fn attach_impact(items: &mut [StartupItem], culprits: &[crate::core::types::BootCulprit]) {
    for it in items.iter_mut() {
        let exe_base = exe_from_command(&it.command)
            .map(|p| {
                p.rsplit(['\\', '/'])
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
            })
            .unwrap_or_default();
        let name_l = it.name.to_ascii_lowercase();
        let hit = culprits.iter().find(|c| {
            let cn = c.name.to_ascii_lowercase();
            (!exe_base.is_empty() && (cn == exe_base || cn.contains(&exe_base)))
                || (!name_l.is_empty() && cn == name_l)
        });
        if let Some(c) = hit {
            let ms = if c.degradation_ms > 0 {
                c.degradation_ms
            } else {
                c.total_ms
            };
            it.impact_ms = Some(ms);
        }
    }
}

// ===========================================================================
// 测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::RiskLevel;

    #[test]
    fn stable_id_is_deterministic_and_source_sensitive() {
        let a = stable_id(StartupSource::RunKey, "loc", "name");
        let b = stable_id(StartupSource::RunKey, "loc", "name");
        let c = stable_id(StartupSource::Service, "loc", "name");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn approved_leaf_maps_32bit_keys_to_run32() {
        // 64 位
        assert_eq!(
            approved_leaf(r"Software\Microsoft\Windows\CurrentVersion\Run"),
            "Run"
        );
        assert_eq!(
            approved_leaf(r"Software\Microsoft\Windows\CurrentVersion\RunOnce"),
            "RunOnce"
        );
        // 32 位：状态在 Run32，写错子键就是「看起来禁用成功、实际毫无作用」
        assert_eq!(
            approved_leaf(r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run"),
            "Run32"
        );
        assert_eq!(
            approved_leaf(r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\RunOnce"),
            "RunOnce32"
        );
        // 大小写不敏感（注册表路径不区分大小写）
        assert_eq!(
            approved_leaf(r"software\wow6432node\microsoft\windows\currentversion\run"),
            "Run32"
        );
    }

    #[test]
    fn approved_flag_reads_low_bit() {
        assert!(!approved_is_disabled(&[0x02, 0, 0, 0]));
        assert!(approved_is_disabled(&[0x03, 0, 0, 0]));
        // 空值 = 未改动 = 启用
        assert!(!approved_is_disabled(&[]));
        assert!(!approved_is_disabled(&[0x06, 0, 0, 0]));
    }

    #[test]
    fn image_path_normalization_expands_systemroot() {
        assert_eq!(
            normalize_image_path(r"\SystemRoot\System32\drivers\ndis.sys"),
            r"C:\Windows\System32\drivers\ndis.sys"
        );
        assert_eq!(
            normalize_image_path(r"\System32\drivers\tcpip.sys"),
            r"C:\Windows\System32\drivers\tcpip.sys"
        );
        assert_eq!(
            normalize_image_path(r"\??\C:\foo\bar.sys"),
            r"C:\foo\bar.sys"
        );
        // 已经是完整路径的不能被二次包裹
        assert_eq!(
            normalize_image_path(r"C:\Windows\System32\x.exe"),
            r"C:\Windows\System32\x.exe"
        );
        // 带引号的路径也不能被拆坏
        assert_eq!(
            normalize_image_path(r#""C:\Program Files\A\a.exe" -k x"#),
            r#""C:\Program Files\A\a.exe" -k x"#
        );
        assert_eq!(normalize_image_path(""), "");
    }

    #[test]
    fn tag_text_extracts_uri_and_enabled() {
        let xml = "<Task><RegistrationInfo><URI>\\Microsoft\\X</URI></RegistrationInfo>\
                   <Settings><Enabled>false</Enabled></Settings></Task>";
        assert_eq!(tag_text(xml, "URI").unwrap(), "\\Microsoft\\X");
        assert_eq!(tag_text(xml, "Enabled").unwrap(), "false");
        assert!(tag_text(xml, "Nope").is_none());
    }

    #[test]
    fn services_reader_returns_plausible_data() {
        let list = read_services();
        // 真机上必然有服务；即使权限受限也不该 panic
        if list.is_empty() {
            return;
        }
        assert!(list.iter().any(|s| s.key_name.eq_ignore_ascii_case("RpcSs")
            || s.key_name.eq_ignore_ascii_case("Dhcp")
            || s.key_name.eq_ignore_ascii_case("Dnscache")
            || s.key_name.eq_ignore_ascii_case("EventLog")),
            "至少应能看到若干系统服务");
        // 启动类型必须落在合法区间
        assert!(list.iter().all(|s| s.start <= 4));
    }

    /// 全量扫描必须能跑通、不 panic，且结果自洽。
    #[test]
    fn full_scan_is_consistent() {
        let s = scan();
        let total: usize = s.counts.iter().map(|(_, n)| n).sum();
        assert_eq!(total, s.items.len(), "分来源计数之和必须等于总数");
        // 每个条目都要有 id 与理由
        for it in &s.items {
            assert_eq!(it.id.len(), 16, "{} 的 id 形状不对", it.name);
            assert!(!it.risk_reason.is_empty(), "{} 缺少风险理由", it.name);
            assert_eq!(
                it.can_disable,
                it.risk != RiskLevel::Dangerous,
                "{} 的 can_disable 与风险档不一致",
                it.name
            );
        }
        // 排序：风险低的必须排在前面
        let rank = |r: RiskLevel| match r {
            RiskLevel::Safe => 0,
            RiskLevel::Caution => 1,
            RiskLevel::Unknown => 2,
            RiskLevel::Dangerous => 3,
        };
        let ranks: Vec<u8> = s.items.iter().map(|i| rank(i.risk)).collect();
        assert!(ranks.windows(2).all(|w| w[0] <= w[1]), "排序不是风险递增");
    }

    /// 安全红线：任何「不建议禁用」的条目都不得可禁用。
    #[test]
    fn dangerous_items_are_never_disableable() {
        let s = scan();
        for it in s.items.iter().filter(|i| i.risk == RiskLevel::Dangerous) {
            assert!(!it.can_disable, "{} 是危险项却允许禁用", it.name);
        }
    }

    /// 系统目录里的服务**只有在知识库明确说「可以禁用」时才允许是绿的**。
    ///
    /// 绿标意味着「一键优化会把它勾上」，没有人工背书就把它交给批量禁用，
    /// 是 P2 最不能犯的错。反过来，像诊断策略服务（DPS）、地图服务（MapsBroker）
    /// 这类 Windows 自带组件，知识库里已经写明可关，锁死成红色只会让
    /// 「一键优化」变成摆设 —— 所以这里要求的是「有背书」，不是「一律不准绿」。
    #[test]
    fn system_service_is_safe_only_with_curated_knowledge() {
        let s = scan();
        let mut checked = 0usize;
        for it in s.items.iter().filter(|i| i.source == StartupSource::Service) {
            let p = exe_from_command(&it.command)
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !p.starts_with("c:\\windows\\") {
                continue;
            }
            checked += 1;
            if it.risk != RiskLevel::Safe {
                continue;
            }
            let kb = it
                .knowledge
                .as_ref()
                .unwrap_or_else(|| panic!("系统服务 {} 是绿标，却没有知识库说明", it.name));
            assert_eq!(
                kb.advice, "可以禁用",
                "系统服务 {} 是绿标，但知识库说的是「{}」",
                it.name, kb.advice
            );
            assert!(!kb.generic, "系统服务 {} 的绿标不能来自兜底条目", it.name);
        }
        assert!(checked > 20, "样本太少（{checked}），这条断言会失去意义");
    }

    /// **界面上「风险标」和「说明面板的建议」不许互相打架。**
    ///
    /// 这是用户截图里那个 bug 的通用形式：360画报 的红标写着「安全软件，禁用会
    /// 影响系统正常使用」，而说明面板里写着「可以禁用」—— 同一屏两个相反结论。
    ///
    /// 三档建议各自**允许**出现的风险档：
    /// * `可以禁用` → 只能绿或黄。出现红色就是当场打脸。
    /// * `谨慎处理` → 只能黄或红。绿标是「放心关」，与「谨慎」互相矛盾。
    /// * `建议保留` → 只能黄或红。
    ///
    /// 「谨慎处理 + 红」是**允许**的，因为两者说的不是一件事：说明书说的是
    /// 「这类东西得小心」，红色说的是「本程序替你挡住了」—— 比如打印后台处理、
    /// 讯飞输入法，知识库写「谨慎处理」没问题，但硬底线不允许用户一键关掉。
    #[test]
    fn knowledge_advice_never_contradicts_visible_risk() {
        let s = scan();
        let mut bad = Vec::new();
        let mut with_kb = 0usize;
        for it in s.items.iter() {
            let Some(kb) = it.knowledge.as_ref() else {
                continue;
            };
            with_kb += 1;
            let allowed: &[RiskLevel] = match kb.advice.as_str() {
                "可以禁用" => &[RiskLevel::Safe, RiskLevel::Caution],
                "谨慎处理" | "建议保留" => &[RiskLevel::Caution, RiskLevel::Dangerous],
                // 「暂无建议」不表态，不参与约束
                _ => continue,
            };
            if !allowed.contains(&it.risk) {
                bad.push(format!(
                    "{}：说明「{}」写的是「{}」，列表上却是 {:?}（{}）",
                    it.name, kb.title, kb.advice, it.risk, it.risk_reason
                ));
            }
        }
        assert!(
            bad.is_empty(),
            "风险标与说明建议互相矛盾，共 {} 条：\n  {}",
            bad.len(),
            bad.join("\n  ")
        );
        assert!(with_kb > 60, "命中知识库的样本太少（{with_kb}）");
    }

    #[test]
    fn impact_is_attached_by_name_match() {
        use crate::core::types::BootCulprit;
        let mut items = vec![hydrate(
            &RawItem {
                source: StartupSource::RunKey,
                name: "Foo".into(),
                command: r"C:\Program Files\Foo\foo.exe".into(),
                location: "loc".into(),
                enabled: true,
                reason_prefix: String::new(),
                delayed: false,
            },
            false,
        )];
        attach_impact(
            &mut items,
            &[BootCulprit {
                name: "foo.exe".into(),
                kind: "app".into(),
                total_ms: 3000,
                degradation_ms: 1200,
                ..Default::default()
            }],
        );
        assert_eq!(items[0].impact_ms, Some(1200));

        attach_impact(
            &mut items,
            &[BootCulprit {
                name: "unrelated.exe".into(),
                kind: "app".into(),
                total_ms: 1,
                degradation_ms: 1,
                ..Default::default()
            }],
        );
        assert_eq!(items[0].impact_ms, Some(1200), "不匹配时不应清空已有归因");
    }

    #[test]
    fn hydrate_all_preserves_input_order() {
        let raws: Vec<RawItem> = (0..37)
            .map(|i| RawItem {
                source: StartupSource::RunKey,
                name: format!("item-{i:02}"),
                command: format!(r"C:\nope\missing-{i}.exe"),
                location: format!("HKCU\\X!item-{i}"),
                enabled: true,
                reason_prefix: String::new(),
                delayed: false,
            })
            .collect();
        let items = hydrate_all(raws.clone(), false);
        assert_eq!(items.len(), raws.len(), "并行不应丢条目");
        for (a, b) in items.iter().zip(raws.iter()) {
            assert_eq!(a.name, b.name, "并行补全必须保持输入顺序");
        }
    }

    #[test]
    fn hydrate_all_handles_empty_and_single() {
        assert!(hydrate_all(Vec::new(), false).is_empty());
        let one = hydrate_all(
            vec![RawItem {
                source: StartupSource::Service,
                name: "S".into(),
                command: r"C:\nope\s.exe".into(),
                location: r"HKLM\SYSTEM\CurrentControlSet\Services\S".into(),
                enabled: true,
                reason_prefix: "延迟启动，对开机影响较小；".into(),
                delayed: true,
            }],
            false,
        );
        assert_eq!(one.len(), 1);
        assert!(one[0].risk_reason.starts_with("延迟启动"));
    }

    #[test]
    fn reason_prefix_is_kept_in_output() {
        let it = hydrate(
            &RawItem {
                source: StartupSource::ScheduledTask,
                name: r"\Microsoft\X".into(),
                command: r"C:\nope\x.exe".into(),
                location: "loc".into(),
                enabled: true,
                reason_prefix: "开机触发；".into(),
                delayed: false,
            },
            false,
        );
        assert!(it.risk_reason.starts_with("开机触发；"), "{}", it.risk_reason);
    }

    /// 快速扫描必须把「待验签」如实标出来，而不是假装已定论。
    ///
    /// 例外：命中**具体**知识库条目的条目。知识库是人工维护的，不依赖签名
    /// 就能给结论（360画报是壁纸工具、DPS 是诊断策略服务，这些不需要验签名才知道），
    /// 这类条目在快速扫描里就已经定档，不必等「确认发布者」。
    /// 兜底条目不算 —— 它连自己都只是「名字像」，不足以盖掉那句实话。
    #[test]
    fn fast_scan_marks_pending_signatures() {
        let fast = scan_with(false);
        let deep = scan_with(true);
        assert_eq!(fast.items.len(), deep.items.len());

        for it in fast.items.iter().filter(|i| i.signature_pending) {
            let kb_decided = it
                .knowledge
                .as_ref()
                .map_or(false, |k| !k.generic && k.advice != "暂无建议");
            if kb_decided {
                continue;
            }
            assert!(
                it.risk_reason.contains("正在确认发布者"),
                "待验签条目必须给出明确措辞，实际：{}",
                it.risk_reason
            );
            // 待验签时不允许给出 Safe —— 那等于没验就下结论
            assert_ne!(it.risk, crate::core::types::RiskLevel::Safe);
        }
        // 已定论的条目不该被标成待验签
        for it in fast.items.iter().filter(|i| !i.signature_pending) {
            assert!(!it.risk_reason.contains("正在确认发布者"));
        }
        // 深度扫描不允许残留待验签状态
        assert!(deep.items.iter().all(|i| !i.signature_pending));
        assert_eq!(deep.pending_signatures, 0);
    }

    /// **核心等价性**：`快速扫描 + verify_pending` 必须与 `深度扫描` 完全一致。
    ///
    /// 这条断言保证「分两步」纯粹是性能优化，没有改变任何判定结果 ——
    /// 否则两阶段模型就是在偷偷降低判定质量。
    #[test]
    fn fast_then_verify_equals_deep() {
        let mut fast = scan_with(false);
        let deep = scan_with(true);

        let pending = fast.pending_signatures;
        let n = verify_pending(&mut fast.items, None);
        assert_eq!(n, pending, "verify_pending 应恰好补齐全部待验签条目");
        assert!(fast.items.iter().all(|i| !i.signature_pending));

        // 顺序也应一致（两者用了同一个排序）
        for (a, b) in fast.items.iter().zip(deep.items.iter()) {
            assert_eq!(a.id, b.id, "两步结果顺序不一致");
            assert_eq!(a.risk, b.risk, "{} 的风险档不一致", a.name);
            assert_eq!(a.can_disable, b.can_disable, "{} 的可禁用性不一致", a.name);
            assert_eq!(a.signature, b.signature, "{} 的签名状态不一致", a.name);
            assert_eq!(a.publisher, b.publisher, "{} 的发布者不一致", a.name);
            assert_eq!(a.risk_reason, b.risk_reason, "{} 的风险理由不一致", a.name);
        }
        assert_eq!(
            count_disable_candidates(&fast.items),
            count_disable_candidates(&deep.items),
            "可安全禁用候选数在两步模型下必须一致"
        );
    }

    /// 快速扫描的耗时必须显著低于深度扫描（否则分两步就没有意义）。
    #[test]
    fn fast_scan_is_much_faster_than_deep() {
        let t = Instant::now();
        let fast = scan_with(false);
        let fast_ms = t.elapsed().as_millis();

        if fast.pending_signatures == 0 {
            // 本机没有需要验签的条目，这条测试无从比较
            return;
        }
        let t = Instant::now();
        let deep = scan_with(true);
        let deep_ms = t.elapsed().as_millis();
        assert!(deep.items.iter().all(|i| !i.signature_pending));
        // 深度扫描要先读几百 MB 的文件，至少慢一个数量级；
        // 这里放宽到 2 倍，避免在快盘上误报
        assert!(
            deep_ms >= fast_ms || fast_ms < 300,
            "快速扫描 {fast_ms} ms 相对深度扫描 {deep_ms} ms 没有优势"
        );
    }

    /// `verify_pending` 必须支持按 ID 过滤（UI 只验可见行）。
    #[test]
    fn verify_pending_respects_id_filter() {
        let mut scan = scan_with(false);
        let pending_before = scan.pending_signatures;
        let Some(target) = scan
            .items
            .iter()
            .find(|i| i.signature_pending)
            .map(|i| i.id.clone())
        else {
            return; // 本机无待验签条目
        };

        let mut only = HashSet::new();
        only.insert(target.clone());
        let n = verify_pending(&mut scan.items, Some(&only));
        assert_eq!(n, 1, "按 ID 过滤时应只处理一个");

        let done = scan.items.iter().find(|i| i.id == target).unwrap();
        assert!(!done.signature_pending);
        assert!(!done.risk_reason.contains("正在确认发布者"));

        // 剩下的可以一次补齐
        let n = verify_pending(&mut scan.items, None);
        assert_eq!(n, pending_before - 1);
        assert!(scan.items.iter().all(|i| !i.signature_pending));
    }
}
