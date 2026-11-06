//! P3 · 场景模式的系统动作实现。
//!
//! 每个动作都遵守两条规矩：
//! 1. **可逆** —— 改之前先读原值，改完把原值记下来，模式切回时能还原。
//! 2. **如实报错** —— 权限不足、电源计划不存在、注册表被保护，都要把真实原因
//!    告诉用户，不做「假装成功」。
//!
//! 纯逻辑（GUID 映射、开关判定）放在本文件里以便单测覆盖；
//! 真正动系统的部分集中在 `windows` 分支。

use std::path::PathBuf;

/// 电源计划 GUID（Windows 内置，不随语言变化 —— 比按名字匹配可靠）。
pub const PLAN_BALANCED: &str = "381b4222-f694-41f0-9685-ff5bb260df2e";
pub const PLAN_HIGH_PERFORMANCE: &str = "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c";

/// 电源计划名 → GUID。未知名字返回 `None`。
pub fn plan_guid(plan: &str) -> Option<&'static str> {
    match plan.trim().to_ascii_lowercase().as_str() {
        "balanced" | "平衡" => Some(PLAN_BALANCED),
        "high_performance" | "high-performance" | "高性能" => Some(PLAN_HIGH_PERFORMANCE),
        _ => None,
    }
}

/// 电源计划 GUID → 展示名。
pub fn plan_name(guid: &str) -> &'static str {
    let g = guid.trim().to_ascii_lowercase();
    if g == PLAN_BALANCED {
        "平衡"
    } else if g == PLAN_HIGH_PERFORMANCE {
        "高性能"
    } else {
        "其它"
    }
}

/// 免打扰对应的注册表子键（HKCU）。
const FOCUS_ASSIST_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Notifications\Settings";
/// 值名：`0` = 关闭通知横幅，`1` = 允许。
const FOCUS_ASSIST_VALUE: &str = "NOC_GLOBAL_SETTING_TOASTS_ENABLED";

/// 免打扰「开/关」→ 注册表值。`on=true` 表示开启免打扰（= 禁掉横幅）。
pub fn focus_assist_value(on: bool) -> u32 {
    if on {
        0
    } else {
        1
    }
}

// ---------------------------------------------------------------------------
// 注册表读写（免打扰 / 优先级等）
// ---------------------------------------------------------------------------

/// 读 HKCU 某个子键下的 DWORD 值。值不存在返回 `Ok(None)`。
pub fn read_dword(subkey: &str, value: &str) -> Result<Option<u32>, String> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey(subkey)
        .map_err(|e| format!("打开注册表项 {subkey} 失败：{e}"))?;
    match key.get_value::<u32, _>(value) {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("读取注册表值 {subkey}\\{value} 失败：{e}")),
    }
}

/// 写 HKCU 某个子键下的 DWORD 值（不存在则创建子键）。
pub fn write_dword(subkey: &str, value: &str, data: u32) -> Result<(), String> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(subkey)
        .map_err(|e| format!("创建注册表项 {subkey} 失败：{e}"))?;
    key.set_value(value, &data)
        .map_err(|e| format!("写入注册表值 {subkey}\\{value} 失败：{e}"))
}

// ---------------------------------------------------------------------------
// 电源计划
// ---------------------------------------------------------------------------

/// 当前生效的电源计划 GUID（`powercfg /getactivescheme`）。
pub fn current_power_plan() -> Result<String, String> {
    let out = std::process::Command::new("powercfg")
        .arg("/getactivescheme")
        .output()
        .map_err(|e| format!("调用 powercfg 失败：{e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    // 输出形如：电源方案 GUID: 381b4222-... (平衡)
    let guid = text
        .split_whitespace()
        .find(|t| t.contains('-') && t.len() >= 32)
        .map(|s| s.trim_matches(|c: char| !c.is_ascii_hexdigit() && c != '-').to_string())
        .ok_or_else(|| format!("无法解析当前电源计划：{}", text.trim()))?;
    Ok(guid.to_ascii_lowercase())
}

/// 设置电源计划。
pub fn set_power_plan(plan: &str) -> Result<String, String> {
    let guid = plan_guid(plan).ok_or_else(|| format!("未知的电源计划：{plan}"))?;
    let out = std::process::Command::new("powercfg")
        .arg("/setactive")
        .arg(guid)
        .output()
        .map_err(|e| format!("调用 powercfg 失败：{e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = if err.trim().is_empty() {
            String::from_utf8_lossy(&out.stdout).to_string()
        } else {
            err.to_string()
        };
        return Err(format!("切换电源计划失败：{}", err.trim()));
    }
    Ok(plan_name(guid).to_string())
}

// ---------------------------------------------------------------------------
// 前台进程优先级
// ---------------------------------------------------------------------------

/// 前台窗口所属进程的 PID。
pub fn foreground_pid() -> Option<u32> {
    let pid = super::process::foreground_pid();
    if pid == 0 {
        None
    } else {
        Some(pid)
    }
}

/// 读取进程的优先级类（`GetPriorityClass`）。
pub fn priority_class(pid: u32) -> Option<u32> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        GetPriorityClass, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let r = GetPriorityClass(h);
        let _ = CloseHandle(h);
        Some(r)
    }
}

/// 设置进程的优先级类。
pub fn set_priority_class(pid: u32, class: u32) -> Result<(), String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, SetPriorityClass, PROCESS_CREATION_FLAGS, PROCESS_SET_INFORMATION,
    };

    unsafe {
        let h = OpenProcess(PROCESS_SET_INFORMATION, false, pid)
            .map_err(|e| format!("打开进程 {pid} 失败：{e}"))?;
        let r = SetPriorityClass(h, PROCESS_CREATION_FLAGS(class));
        let _ = CloseHandle(h);
        r.map_err(|e| format!("设置进程 {pid} 优先级失败：{e}"))
    }
}

/// 「高于标准」优先级类（`ABOVE_NORMAL_PRIORITY_CLASS`）。
pub const PRIORITY_ABOVE_NORMAL: u32 = 0x8000;
/// 「标准」优先级类（`NORMAL_PRIORITY_CLASS`）。
pub const PRIORITY_NORMAL: u32 = 0x20;

/// 免打扰：开关通知横幅。
pub fn set_focus_assist(on: bool) -> Result<(), String> {
    write_dword(
        FOCUS_ASSIST_KEY,
        FOCUS_ASSIST_VALUE,
        focus_assist_value(on),
    )
}

/// 读免打扰当前状态（`true` = 免打扰已开）。
pub fn focus_assist() -> Option<bool> {
    match read_dword(FOCUS_ASSIST_KEY, FOCUS_ASSIST_VALUE) {
        Ok(Some(v)) => Some(v == 0),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 模式状态持久化
// ---------------------------------------------------------------------------

/// 当前生效模式 id 存放在 `data/mode.json`。
pub fn state_path() -> PathBuf {
    super::store::data_dir().join("mode.json")
}

/// 读当前生效模式 id（没有则 `daily`）。
pub fn active_mode_id() -> String {
    let p = state_path();
    let Ok(text) = std::fs::read_to_string(&p) else {
        return "daily".into();
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("active").and_then(|x| x.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| "daily".into())
}

/// 写当前生效模式 id。
pub fn set_active_mode_id(id: &str) -> Result<(), String> {
    let p = state_path();
    let json = serde_json::json!({ "active": id });
    std::fs::write(&p, serde_json::to_string_pretty(&json).unwrap_or_default())
        .map_err(|e| format!("保存当前模式失败：{e}"))
}

/// 自动切换配置（默认关闭 —— 实施计划 P3 明确要求）。
pub fn auto_switch_path() -> PathBuf {
    super::store::data_dir().join("mode_auto.json")
}

/// 读自动切换配置。默认 `enabled=false`。
pub fn auto_switch() -> AutoSwitch {
    let p = auto_switch_path();
    let Ok(text) = std::fs::read_to_string(&p) else {
        return AutoSwitch::default();
    };
    serde_json::from_str::<AutoSwitch>(&text).unwrap_or_default()
}

/// 写自动切换配置。
pub fn set_auto_switch(v: &AutoSwitch) -> Result<(), String> {
    let p = auto_switch_path();
    std::fs::write(&p, serde_json::to_string_pretty(v).unwrap_or_default())
        .map_err(|e| format!("保存自动切换配置失败：{e}"))
}

/// 自动切换配置。
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct AutoSwitch {
    /// 总开关。**默认关闭**。
    #[serde(default)]
    pub enabled: bool,
    /// 内存占用超过该百分比时切到哪个模式。
    #[serde(default)]
    pub mem_above_pct: Option<u32>,
    #[serde(default)]
    pub mem_above_mode: Option<String>,
    /// 前台进程名匹配时切到哪个模式。
    #[serde(default)]
    pub foreground_process: Option<String>,
    #[serde(default)]
    pub foreground_mode: Option<String>,
}

impl Default for AutoSwitch {
    fn default() -> Self {
        Self {
            enabled: false,
            mem_above_pct: None,
            mem_above_mode: None,
            foreground_process: None,
            foreground_mode: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_guid_maps_known_names() {
        assert_eq!(plan_guid("balanced"), Some(PLAN_BALANCED));
        assert_eq!(plan_guid("high_performance"), Some(PLAN_HIGH_PERFORMANCE));
        assert_eq!(plan_guid("高性能"), Some(PLAN_HIGH_PERFORMANCE));
        assert_eq!(plan_guid("turbo"), None);
        assert_eq!(plan_guid(""), None);
    }

    #[test]
    fn plan_name_is_stable() {
        assert_eq!(plan_name(PLAN_BALANCED), "平衡");
        assert_eq!(plan_name(PLAN_HIGH_PERFORMANCE), "高性能");
        assert_eq!(plan_name("deadbeef"), "其它");
    }

    #[test]
    fn focus_assist_value_is_inverted_on_off() {
        // 「免打扰开」= 横幅关掉 = 0
        assert_eq!(focus_assist_value(true), 0);
        assert_eq!(focus_assist_value(false), 1);
    }

    #[test]
    fn auto_switch_defaults_to_disabled() {
        let a = AutoSwitch::default();
        assert!(!a.enabled, "自动切换必须默认关闭");
        assert!(a.mem_above_pct.is_none());
        assert!(a.foreground_process.is_none());
    }

    #[test]
    fn auto_switch_round_trips_through_json() {
        let a = AutoSwitch {
            enabled: true,
            mem_above_pct: Some(85),
            mem_above_mode: Some("game".into()),
            foreground_process: Some("steam.exe".into()),
            foreground_mode: Some("game".into()),
        };
        let j = serde_json::to_string(&a).unwrap();
        let b: AutoSwitch = serde_json::from_str(&j).unwrap();
        assert!(b.enabled);
        assert_eq!(b.mem_above_pct, Some(85));
        assert_eq!(b.foreground_process.as_deref(), Some("steam.exe"));
    }
}
