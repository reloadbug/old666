//! 开机耗时归因引擎。
//!
//! ## 为什么不自写 ETW 消费者
//!
//! Windows 自己就在跑一套开机性能诊断（`Microsoft-Windows-Diagnostics-Performance`），
//! 它已经把「开机总耗时 / 主路径耗时 / 桌面后台耗时」以及每个阶段的细分算好，
//! 并写进了 `Operational` 事件日志。自建 ETW 会话需要常驻提权进程、要处理会话丢失、
//! 还得自己维护 provider GUID —— 收益为零，风险为正。
//!
//! 因此这里直接**读官方结论**，只在最后一步做「归因到用户能动手的对象」。
//!
//! ## 数据来源
//!
//! | 事件 ID | 含义 | 用途 |
//! |---|---|---|
//! | 100 | 开机性能记录 | 分段耗时 + 是否降级 |
//! | 101 | 应用启动慢 | 降级归因（app） |
//! | 102 | 驱动加载慢 | 降级归因（driver） |
//! | 103 | 服务启动慢 | 降级归因（service） |
//! | 109 | 设备初始化慢 | 降级归因（device） |
//!
//! ## 解析策略：通用字段提取，不写死 schema
//!
//! 事件 XML 里所有负载都是 `<Data Name="X">值</Data>`。不同 Windows 版本、
//! 不同 provider 版本的字段集合并不一致，所以这里**不做强类型反序列化**，
//! 而是把全部字段抽成一个大小写/空格不敏感的表，再按需取用。
//! 字段缺失只会让对应分段消失，不会让整个模块报错。

use crate::core::types::{BootCulprit, BootPhase, BootRecord, BootTimeline};

/// 开机性能诊断通道。
pub const CHANNEL: &str = "Microsoft-Windows-Diagnostics-Performance/Operational";

/// 打开通道时的错误码（日志被关闭 / 通道不存在 / 查询串非法）。
const ERROR_EVT_INVALID_CHANNEL_PATH: i32 = 15000;
const ERROR_EVT_INVALID_QUERY: i32 = 15001;
const ERROR_EVT_CHANNEL_NOT_FOUND: i32 = 15007;
const ERROR_INSUFFICIENT_BUFFER: i32 = 122;
const ERROR_NO_MORE_ITEMS: i32 = 259;

/// 事件日志通道查询必须给 XPath；空串会被判为 `ERROR_EVT_INVALID_QUERY`。
/// `*` 表示「所有事件」。
const ALL_EVENTS: &str = "*";

// ===========================================================================
// 事件日志读取
// ===========================================================================

/// 查询通道内的事件，返回事件 XML 列表。
///
/// `xpath` 为空表示不过滤。`limit` 限制拉取条数（防止日志很大时卡住）。
fn query_xml(xpath: &str, limit: usize) -> Result<Vec<String>, String> {
    use windows::core::PCWSTR;
    use windows::Win32::System::EventLog::{
        EvtClose, EvtNext, EvtQuery, EvtQueryChannelPath, EvtQueryReverseDirection,
    };

    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let channel_w = wide(CHANNEL);
    let xpath_owned = if xpath.trim().is_empty() {
        ALL_EVENTS.to_string()
    } else {
        xpath.to_string()
    };
    let xpath_w = wide(&xpath_owned);

    unsafe {
        let query = EvtQuery(
            None,
            PCWSTR(channel_w.as_ptr()),
            PCWSTR(xpath_w.as_ptr()),
            (EvtQueryChannelPath.0 | EvtQueryReverseDirection.0) as u32,
        )
        .map_err(|e| {
            let code = e.code().0;
            match code {
                ERROR_EVT_CHANNEL_NOT_FOUND | ERROR_EVT_INVALID_CHANNEL_PATH => {
                    "系统未启用开机性能诊断日志".to_string()
                }
                ERROR_EVT_INVALID_QUERY => {
                    format!("开机诊断日志查询语句非法（0x{code:08X}）")
                }
                _ => format!("打开开机诊断日志失败（0x{code:08X}）"),
            }
        })?;

        let mut out = Vec::new();
        let mut last_err: Option<i32> = None;
        // 每批取 16 条；EvtNext 是流式的，取完会自动推进
        loop {
            if out.len() >= limit {
                break;
            }
            let mut batch = [0isize; 16];
            let mut returned: u32 = 0;
            match EvtNext(query, &mut batch, 0, 0, &mut returned) {
                Ok(()) => {}
                Err(e) => {
                    let code = e.code().0;
                    // ERROR_NO_MORE_ITEMS 表示取完了，属正常结束；
                    // 其它错误也停止，但已取到的条目照常返回
                    if code != ERROR_NO_MORE_ITEMS {
                        last_err = Some(code);
                    }
                    break;
                }
            }
            if returned == 0 {
                break;
            }
            for &raw in batch.iter().take(returned as usize) {
                if raw == 0 {
                    continue;
                }
                let handle = windows::Win32::System::EventLog::EVT_HANDLE(raw);
                if let Some(xml) = render_xml(handle) {
                    out.push(xml);
                }
                let _ = EvtClose(handle);
                if out.len() >= limit {
                    break;
                }
            }
        }
        let _ = EvtClose(query);

        // 一条都没取到、且确实是错误而非「日志为空」时，把错误码透出去
        if out.is_empty() {
            if let Some(c) = last_err {
                return Err(format!("读取开机诊断日志失败（0x{c:08X}）"));
            }
        }
        Ok(out)
    }
}

/// 把单个事件渲染成 XML 字符串。
fn render_xml(handle: windows::Win32::System::EventLog::EVT_HANDLE) -> Option<String> {
    use windows::Win32::System::EventLog::{EvtRender, EvtRenderEventXml};

    unsafe {
        let mut used: u32 = 0;
        let mut props: u32 = 0;
        // 第一次调用只为拿长度，必然返回「缓冲区不足」
        let probe = EvtRender(None, handle, EvtRenderEventXml.0, 0, None, &mut used, &mut props);
        let need = if used > 0 {
            used
        } else {
            match probe {
                Err(e) if e.code().0 == ERROR_INSUFFICIENT_BUFFER => return None,
                Err(_) => return None,
                Ok(()) => return None,
            }
        };
        if need == 0 {
            return None;
        }

        let mut buf = vec![0u16; (need / 2) as usize + 2];
        let mut used2: u32 = 0;
        let ok = EvtRender(
            None,
            handle,
            EvtRenderEventXml.0,
            (buf.len() * 2) as u32,
            Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
            &mut used2,
            &mut props,
        );
        if ok.is_err() {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

// ===========================================================================
// XML 字段抽取（纯函数，可单测）
// ===========================================================================

/// 从事件 XML 中抽出全部 `<Data Name="X">值</Data>` 字段。
///
/// 键做了归一化：转小写 + 去掉空格与连字符，因此 `Total Time` / `TotalTime` /
/// `total_time` 都能命中同一个键。
pub fn extract_fields(xml: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    // 只找 <Data ...> 开标签，避免把 <Data> 之外的内容误吞
    while let Some(open_rel) = xml[pos..].find("<Data") {
        let open = pos + open_rel;
        // 标签必须在 <Data 后紧跟空白或 '>'，否则可能是 <DataFoo>
        let after = xml[open + 5..].chars().next();
        if !matches!(after, Some(c) if c.is_whitespace() || c == '>') {
            pos = open + 5;
            continue;
        }
        let Some(tag_end_rel) = xml[open..].find('>') else {
            break;
        };
        let tag_end = open + tag_end_rel;
        let tag = &xml[open..tag_end];
        // 自闭合标签没有内容，跳过
        if tag.ends_with('/') {
            pos = tag_end + 1;
            continue;
        }
        let Some(close_rel) = xml[tag_end..].find("</Data>") else {
            break;
        };
        let close = tag_end + close_rel;
        let raw_value = &xml[tag_end + 1..close];

        if let Some(name) = attr_value(tag, "Name") {
            out.push((normalize_key(&name), decode_entities(raw_value.trim())));
        }
        pos = close + 7;
    }
    out
}

/// 取标签内某个属性的值（支持单双引号）。
fn attr_value(tag: &str, attr: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{}=", attr.to_ascii_lowercase());
    let i = lower.find(&needle)? + needle.len();
    let rest = tag[i..].trim_start();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let inner = &rest[1..];
    let end = inner.find(quote)?;
    Some(inner[..end].to_string())
}

/// 键归一化：小写 + 去掉空格/连字符/下划线。
pub fn normalize_key(k: &str) -> String {
    k.chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != '_')
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// 解码 XML 里最常见的 5 个实体。
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// 从事件 XML 里取事件 ID。
pub fn extract_event_id(xml: &str) -> Option<u32> {
    let i = xml.find("<EventID")?;
    let tag_end = xml[i..].find('>')? + i;
    let close = xml[tag_end..].find("</EventID>")? + tag_end;
    xml[tag_end + 1..close].trim().parse().ok()
}

/// 从 `<TimeCreated SystemTime='...'/>` 取时间戳。
pub fn extract_time_created(xml: &str) -> String {
    let Some(i) = xml.find("<TimeCreated") else {
        return String::new();
    };
    let Some(end) = xml[i..].find("/>") else {
        return String::new();
    };
    let tag = &xml[i..i + end];
    attr_value(tag, "SystemTime").unwrap_or_default()
}

/// 在字段表里按归一化键查值（大小写/空格不敏感）。
fn field<'a>(fields: &'a [(String, String)], keys: &[&str]) -> Option<&'a str> {
    for k in keys {
        let nk = normalize_key(k);
        if let Some((_, v)) = fields.iter().find(|(key, _)| *key == nk) {
            return Some(v.as_str());
        }
    }
    None
}

fn num(fields: &[(String, String)], keys: &[&str]) -> Option<u64> {
    field(fields, keys).and_then(|v| v.trim().parse::<u64>().ok())
}

fn truthy(fields: &[(String, String)], keys: &[&str]) -> bool {
    match field(fields, keys) {
        Some(v) => {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        }
        None => false,
    }
}

// ===========================================================================
// 事件 → 业务结构
// ===========================================================================

/// 主路径分段定义：`(字段名, 机器键, 中文标签, 是否用户可优化)`
///
/// 顺序即真实开机时序。这些字段全部来自事件 100 的实测 schema
/// （`BootTsVersion=2`），每一项都是 Windows 自己统计出来的原始数字，
/// 不做任何加权或推算。
///
/// **刻意不包含** `BootKernelInitTime`：实测该字段恒为 30 ms 左右，
/// 它并不是「内核初始化耗时」，用它当分段会严重低估内核阶段。
/// 真正对应内核阶段的是 `OtherKernelInitDuration`。
const PHASE_DEFS: &[(&str, &str, &str, bool)] = &[
    ("OSLoaderDuration", "loader", "引导加载（UEFI / 引导管理器）", false),
    ("OtherKernelInitDuration", "kernel", "内核初始化", false),
    ("BootDriverInitTime", "drivers", "驱动加载", true),
    ("BootDevicesInitTime", "devices", "设备枚举", false),
    ("BootSmssInitTime", "smss", "会话管理器初始化", false),
    (
        "BootCriticalServicesInitTime",
        "services",
        "关键服务启动",
        true,
    ),
    (
        "BootMachineProfileProcessingTime",
        "machine_profile",
        "机器配置加载",
        false,
    ),
    (
        "BootUserProfileProcessingTime",
        "profile",
        "用户配置加载",
        true,
    ),
    ("BootExplorerInitTime", "explorer", "桌面初始化", true),
];

/// 把事件 100 的字段表解析成一条开机记录（不含归因）。
pub fn record_from_event(xml: &str) -> Option<BootRecord> {
    let fields = extract_fields(xml);
    if fields.is_empty() {
        return None;
    }

    let boot_ms = num(&fields, &["BootTime", "Boot Duration"]).unwrap_or(0);
    let main_path_ms = num(&fields, &["MainPathBootTime"]).unwrap_or(0);
    let post_boot_ms = num(&fields, &["BootPostBootTime"]).unwrap_or(0);
    if boot_ms == 0 && main_path_ms == 0 {
        return None;
    }

    let mut phases: Vec<BootPhase> = Vec::new();
    let mut known_sum: u64 = 0;
    for (field, key, label, actionable) in PHASE_DEFS {
        let Some(ms) = num(&fields, &[*field]) else {
            continue;
        };
        if ms == 0 {
            continue;
        }
        known_sum = known_sum.saturating_add(ms);
        phases.push(BootPhase {
            key: (*key).to_string(),
            label: (*label).to_string(),
            ms,
            kind: "main".into(),
            actionable: *actionable,
        });
    }

    // 主路径里有没被细分的部分，诚实列出来，而不是硬塞进某个阶段。
    // 这样「主路径各段之和」与系统给出的 MainPathBootTime 精确相等，
    // 时间线的误差只可能来自系统自身的口径，不来自我们的估算。
    if main_path_ms > known_sum {
        let rest = main_path_ms - known_sum;
        if rest >= 50 {
            phases.push(BootPhase {
                key: "other_main".into(),
                label: "主路径其他开销".into(),
                ms: rest,
                kind: "main".into(),
                actionable: false,
            });
        }
    }

    // 桌面就绪后的后台收尾单独成段
    if post_boot_ms > 0 {
        phases.push(BootPhase {
            key: "postboot".into(),
            label: "桌面就绪后的后台收尾".into(),
            ms: post_boot_ms,
            kind: "post".into(),
            actionable: true,
        });
    }

    Some(BootRecord {
        boot_ms,
        main_path_ms,
        post_boot_ms,
        // 用 BootStartTime 而不是 TimeCreated：前者是本次开机的起点，
        // 后者只是「事件被写进日志的时刻」，归因事件靠前者对齐。
        start_time: field(&fields, &["BootStartTime"]).unwrap_or_default().to_string(),
        end_time: field(&fields, &["BootEndTime"]).unwrap_or_default().to_string(),
        degraded: truthy(
            &fields,
            &["BootIsDegradation", "BootIsStepDegradation", "BootIsGradualDegradation"],
        ),
        startup_apps: num(&fields, &["BootNumStartupApps"]).unwrap_or(0) as u32,
        phases,
        culprits: Vec::new(),
    })
}

/// 归因事件携带的开机起点（用于把归因精确挂到某一次开机上）。
pub fn culprit_boot_start(xml: &str) -> String {
    let fields = extract_fields(xml);
    field(&fields, &["StartTime"]).unwrap_or_default().to_string()
}

/// 把降级事件（101/102/103/109…）解析成归因条目。
pub fn culprit_from_event(xml: &str) -> Option<BootCulprit> {
    let id = extract_event_id(xml)?;
    let kind = match id {
        101 => "app",
        102 => "driver",
        103 => "service",
        109 => "device",
        // 104–108 是后台优化维护事件，不是「谁拖慢了我」，不参与归因
        _ => return None,
    };
    let fields = extract_fields(xml);
    let name = field(&fields, &["Name", "FileName"])?.to_string();
    if name.is_empty() {
        return None;
    }
    let total_ms = num(&fields, &["TotalTime", "Total Time"]).unwrap_or(0);
    let degradation_ms = num(&fields, &["DegradationTime", "Degradation Time"]).unwrap_or(0);
    Some(BootCulprit {
        name,
        friendly_name: field(&fields, &["FriendlyName"]).unwrap_or_default().to_string(),
        kind: kind.to_string(),
        total_ms,
        degradation_ms,
        path: field(&fields, &["Path"]).unwrap_or_default().to_string(),
        company: field(&fields, &["CompanyName"]).unwrap_or_default().to_string(),
    })
}

/// 读取最近 `limit` 次开机的时间线。
pub fn timeline(limit: usize) -> BootTimeline {
    let limit = limit.clamp(1, 50);
    let xmls = match query_xml("", limit * 16) {
        Ok(v) => v,
        Err(note) => {
            return BootTimeline {
                available: false,
                note,
                ..Default::default()
            }
        }
    };

    let mut records: Vec<BootRecord> = Vec::new();
    // 归因按「开机起点」分组，键与 BootRecord.start_time 精确对应
    let mut culprits_by_boot: std::collections::HashMap<String, Vec<BootCulprit>> =
        std::collections::HashMap::new();

    for xml in &xmls {
        match extract_event_id(xml) {
            Some(100) => {
                if let Some(r) = record_from_event(xml) {
                    records.push(r);
                }
            }
            Some(_) => {
                if let Some(c) = culprit_from_event(xml) {
                    let key = culprit_boot_start(xml);
                    culprits_by_boot.entry(key).or_default().push(c);
                }
            }
            None => {}
        }
    }

    if records.is_empty() {
        return BootTimeline {
            available: false,
            note: format!(
                "暂无开机记录。该日志需要重启一次才会写入；若长期为空，\
                 可在管理员命令行执行 wevtutil sl {CHANNEL} /e:true 开启。"
            ),
            ..Default::default()
        };
    }

    records.truncate(limit);

    // 精确挂载：只把「本次开机」产生的归因挂到本次记录上。
    // 靠 BootStartTime 全等匹配，不做时间区间推断 —— 实测两者字符串完全一致。
    for r in records.iter_mut() {
        if let Some(mut mine) = culprits_by_boot.remove(&r.start_time) {
            mine.sort_by(|a, b| b.degradation_ms.cmp(&a.degradation_ms));
            // 同一对象可能被记多条，按名字去重，保留拖慢最多的那条
            let mut seen = std::collections::HashSet::new();
            mine.retain(|c| seen.insert(c.name.to_ascii_lowercase()));
            mine.truncate(20);
            r.culprits = mine;
        }
    }

    let times: Vec<u64> = records
        .iter()
        .map(|r| r.boot_ms)
        .filter(|v| *v > 0)
        .collect();
    let avg_ms = if times.is_empty() {
        0
    } else {
        times.iter().sum::<u64>() / times.len() as u64
    };

    BootTimeline {
        best_ms: times.iter().copied().min().unwrap_or(0),
        worst_ms: times.iter().copied().max().unwrap_or(0),
        avg_ms,
        records,
        available: true,
        note: String::new(),
    }
}

/// 只取最近一次开机。
pub fn latest() -> Option<BootRecord> {
    timeline(1).records.into_iter().next()
}

// ===========================================================================
// 测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 事件 100 的真实 schema 片段（字段名与真机导出的完全一致）。
    const EVENT_100: &str = r#"<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'>
<System><Provider Name='Microsoft-Windows-Diagnostics-Performance'/>
<EventID>100</EventID>
<TimeCreated SystemTime='2026-10-14T14:06:46.2870971Z'/>
</System>
<EventData>
<Data Name='BootTsVersion'>2</Data>
<Data Name='BootStartTime'>2026-10-14T14:04:39.8392618Z</Data>
<Data Name='BootEndTime'>2026-10-14T14:06:42.4240587Z</Data>
<Data Name='SystemBootInstance'>165</Data>
<Data Name='BootTime'>105828</Data>
<Data Name='MainPathBootTime'>18545</Data>
<Data Name='BootKernelInitTime'>30</Data>
<Data Name='BootDriverInitTime'>1117</Data>
<Data Name='BootDevicesInitTime'>650</Data>
<Data Name='BootSmssInitTime'>3438</Data>
<Data Name='BootCriticalServicesInitTime'>182</Data>
<Data Name='BootUserProfileProcessingTime'>429</Data>
<Data Name='BootMachineProfileProcessingTime'>29</Data>
<Data Name='BootExplorerInitTime'>4470</Data>
<Data Name='BootNumStartupApps'>6</Data>
<Data Name='BootPostBootTime'>87283</Data>
<Data Name='BootIsDegradation'>true</Data>
<Data Name='OSLoaderDuration'>1034</Data>
<Data Name='OtherKernelInitDuration'>1753</Data>
</EventData></Event>"#;

    #[test]
    fn extract_fields_reads_data_pairs() {
        let f = extract_fields(EVENT_100);
        assert!(f.iter().any(|(k, v)| k == "boottime" && v == "105828"));
        assert!(f.iter().any(|(k, v)| k == "bootisdegradation" && v == "true"));
    }

    #[test]
    fn key_normalization_is_space_and_case_insensitive() {
        assert_eq!(normalize_key("Total Time"), "totaltime");
        assert_eq!(normalize_key("total_time"), "totaltime");
        assert_eq!(normalize_key("TotalTime"), "totaltime");
        assert_eq!(normalize_key("Degradation-Time"), "degradationtime");
    }

    #[test]
    fn event_id_is_parsed() {
        assert_eq!(extract_event_id(EVENT_100), Some(100));
    }

    #[test]
    fn time_created_is_parsed() {
        assert_eq!(extract_time_created(EVENT_100), "2026-10-14T14:06:46.2870971Z");
    }

    #[test]
    fn record_uses_boot_start_time_not_log_time() {
        let r = record_from_event(EVENT_100).expect("应能解析");
        // 必须是 BootStartTime —— 归因事件靠这个字段对齐
        assert_eq!(r.start_time, "2026-10-14T14:04:39.8392618Z");
        assert_ne!(r.start_time, extract_time_created(EVENT_100));
        assert_eq!(r.end_time, "2026-10-14T14:06:42.4240587Z");
    }

    #[test]
    fn record_splits_phases_and_sums_exactly_to_main_path() {
        let r = record_from_event(EVENT_100).expect("应能解析");
        assert_eq!(r.boot_ms, 105828);
        assert_eq!(r.main_path_ms, 18545);
        assert_eq!(r.post_boot_ms, 87283);
        assert_eq!(r.startup_apps, 6);
        assert!(r.degraded, "BootIsDegradation=true 应识别为降级");

        // 主路径各段之和必须精确等于 MainPathBootTime（误差为 0，不是「小于 10%」）
        let main_sum: u64 = r
            .phases
            .iter()
            .filter(|p| p.kind == "main")
            .map(|p| p.ms)
            .sum();
        assert_eq!(main_sum, r.main_path_ms, "主路径分段之和必须精确相等");

        // 全部分段之和必须精确等于 BootTime
        let all_sum: u64 = r.phases.iter().map(|p| p.ms).sum();
        assert_eq!(all_sum, r.boot_ms, "分段总和必须精确等于开机总耗时");

        // 顺序：主路径在前、后台收尾在最后
        let keys: Vec<&str> = r.phases.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys.first(), Some(&"loader"));
        assert_eq!(keys.last(), Some(&"postboot"));
        assert_eq!(keys.iter().filter(|k| **k == "postboot").count(), 1);
    }

    #[test]
    fn kernel_init_time_field_is_deliberately_not_a_phase() {
        // 实测 BootKernelInitTime 恒为 30ms 左右，不是「内核初始化」，
        // 用它当分段会严重低估。这里锁死这个决定，防止以后被「顺手加回去」。
        let r = record_from_event(EVENT_100).unwrap();
        assert!(
            r.phases.iter().all(|p| p.key != "kernel" || p.ms != 30),
            "不应把 BootKernelInitTime(30ms) 当成内核阶段"
        );
        let kernel = r.phases.iter().find(|p| p.key == "kernel").unwrap();
        assert_eq!(kernel.ms, 1753, "内核阶段应取 OtherKernelInitDuration");
    }

    #[test]
    fn missing_fields_do_not_break_parsing() {
        let xml = r#"<Event><System><EventID>100</EventID></System><EventData>
        <Data Name='BootTime'>9000</Data><Data Name='MainPathBootTime'>9000</Data>
        <Data Name='OSLoaderDuration'>4000</Data></EventData></Event>"#;
        let r = record_from_event(xml).unwrap();
        assert_eq!(r.post_boot_ms, 0);
        assert!(r.phases.iter().all(|p| p.key != "postboot"));
        assert!(!r.degraded);
        assert_eq!(r.start_time, "", "缺 BootStartTime 时不应 panic");
        // 仍然精确配平
        assert_eq!(r.phases.iter().map(|p| p.ms).sum::<u64>(), 9000);
    }

    #[test]
    fn overlapping_official_fields_never_produce_negative_remainder() {
        // 万一某次开机系统给的细分之和超过 MainPath，也不能出现下溢或负值
        let xml = r#"<Event><System><EventID>100</EventID></System><EventData>
        <Data Name='BootTime'>1000</Data><Data Name='MainPathBootTime'>100</Data>
        <Data Name='BootDriverInitTime'>400</Data>
        <Data Name='BootExplorerInitTime'>400</Data></EventData></Event>"#;
        let r = record_from_event(xml).unwrap();
        assert!(r.phases.iter().all(|p| p.ms > 0));
        assert!(r.phases.iter().all(|p| p.key != "other_main"));
    }

    #[test]
    fn culprit_kind_is_mapped_by_event_id() {
        let mk = |id: u32| {
            format!(
                r#"<Event><System><EventID>{id}</EventID>
                <TimeCreated SystemTime='2026-10-14T14:06:46.2871231Z'/></System><EventData>
                <Data Name='StartTime'>2026-10-14T14:04:39.8392618Z</Data>
                <Data Name='Name'>SomeApp.exe</Data>
                <Data Name='FriendlyName'>Some App</Data>
                <Data Name='TotalTime'>3000</Data>
                <Data Name='DegradationTime'>2500</Data>
                <Data Name='Path'>C:\x\SomeApp.exe</Data>
                <Data Name='CompanyName'>Some Corp</Data>
                </EventData></Event>"#
            )
        };
        assert_eq!(culprit_from_event(&mk(101)).unwrap().kind, "app");
        assert_eq!(culprit_from_event(&mk(102)).unwrap().kind, "driver");
        assert_eq!(culprit_from_event(&mk(103)).unwrap().kind, "service");
        assert_eq!(culprit_from_event(&mk(109)).unwrap().kind, "device");

        let c = culprit_from_event(&mk(101)).unwrap();
        assert_eq!(c.name, "SomeApp.exe");
        assert_eq!(c.friendly_name, "Some App");
        assert_eq!(c.total_ms, 3000);
        assert_eq!(c.degradation_ms, 2500);
        assert_eq!(c.path, r"C:\x\SomeApp.exe");
        assert_eq!(c.company, "Some Corp");

        // 归因事件必须能报出它属于哪次开机
        assert_eq!(
            culprit_boot_start(&mk(101)),
            "2026-10-14T14:04:39.8392618Z"
        );

        // 104–108 是后台优化维护事件，不该被当成「拖慢开机的元凶」
        assert!(culprit_from_event(&mk(105)).is_none());
        // 100 是主记录，也不该走归因分支
        assert!(culprit_from_event(EVENT_100).is_none());
    }

    #[test]
    fn empty_or_junk_xml_yields_none() {
        assert!(record_from_event("").is_none());
        assert!(record_from_event("<Event/>").is_none());
        // 有字段但没有任何耗时字段 → 不是一条有效开机记录
        let xml = r#"<Event><EventData><Data Name='Foo'>1</Data></EventData></Event>"#;
        assert!(record_from_event(xml).is_none());
    }

    #[test]
    fn entities_are_decoded() {
        let xml = r#"<Event><EventData>
        <Data Name='Name'>A &amp; B &lt;x&gt;</Data>
        <Data Name='TotalTime'>1</Data></EventData></Event>"#;
        let f = extract_fields(xml);
        assert!(f.iter().any(|(_, v)| v == "A & B <x>"));
    }

    /// 真机冒烟：日志可能为空，但**绝不能 panic**，且结论必须自洽。
    #[test]
    fn timeline_never_panics_and_is_self_consistent() {
        let t = timeline(5);
        if t.available {
            assert!(!t.records.is_empty());
            assert!(t.best_ms <= t.avg_ms && t.avg_ms <= t.worst_ms);
            for r in &t.records {
                // 分段之和必须精确等于总耗时（本模块的核心正确性保证）
                let sum: u64 = r.phases.iter().map(|p| p.ms).sum();
                assert_eq!(sum, r.boot_ms, "分段配平失败：{} != {}", sum, r.boot_ms);
                // 归因必须按开机起点精确挂载，不允许「串味」
                for c in &r.culprits {
                    assert!(!c.name.is_empty());
                }
            }
        } else {
            assert!(!t.note.is_empty(), "不可用时必须给出原因");
        }
    }
}
