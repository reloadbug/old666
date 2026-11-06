//! 系统关键进程 / 服务 / 驱动 **硬编码黑名单**。
//!
//! 这是整个产品的安全红线：命中此表的进程**永远不会**被结束，
//! 不参与打分、不受阈值影响、用户也无法通过白名单反向解锁。
//!
//! 设计原则：
//! 1. 宁可多列（误伤一个可结束进程只是少优化一点），绝不漏列（漏一个可能蓝屏）。
//! 2. 名称匹配一律 **不区分大小写**、**去掉 `.exe` 后缀**后比较。
//! 3. 除了名称，还叠加「路径 + 签名」双重判据做兜底（见 `is_protected`）。

use std::collections::HashSet;
use std::sync::OnceLock;

/// 进程名黑名单（小写，不含 `.exe`）。
///
/// 覆盖：内核与会话基础设施、安全/杀软、显卡与输入、存储与网络栈、
/// 音频、虚拟化、以及杀错必蓝屏或必掉网/掉声的组件。
const DENY_NAMES: &[&str] = &[
    // ================= 内核与会话基础设施 =================
    "system",
    "system idle process",
    "registry",
    "memory compression",
    "secure system",
    "idle",
    "smss",              // 会话管理器
    "csrss",             // 客户端/服务器运行时
    "wininit",           // Windows 启动应用
    "winlogon",          // 登录
    "services",          // 服务控制管理器
    "lsass",             // 本地安全认证 —— 杀它立即重启
    "lsaiso",            // LSA 隔离
    "svchost",           // 服务宿主，内含大量关键服务
    "fontdrvhost",       // 字体驱动宿主
    "dwm",               // 桌面窗口管理器 —— 杀它桌面全黑
    "wudfhost",          // 用户模式驱动框架
    "ntoskrnl",
    "ci",                // 代码完整性
    "conhost",           // 控制台宿主
    "runtimebroker",     // 系统 API 代理
    "sihost",            // Shell 基础设施宿主
    "shellexperiencehost",
    "startmenuexperiencehost",
    "searchhost",
    "textinputhost",
    "applicationframehost",
    "usoclient",
    "taskhostw",         // 任务宿主
    "wscript",
    "cscript",
    "wmiprvse",          // WMI 提供程序宿主
    "dllhost",           // COM+ 代理
    "spoolsv",           // 打印后台 —— 杀它打印全废
    "explorer",          // 资源管理器 —— 杀它桌面/任务栏消失
    "sppsvc",            // 软件保护
    "wlanext",           // WLAN 扩展
    "wmiregistrationservice",
    "lsm",
    "nissrv",            // 网络检查服务

    // ================= 安全 / 杀软（结束它们会立刻报警并可能失保护） =================
    "msmpeng",           // Defender 反恶意软件引擎
    "nissrv",
    "securityhealthservice",
    "securityhealthsystray",
    "smartscreen",
    "mpcmdrun",
    "mssense",           // Defender for Endpoint
    "windefend",
    "sense",
    "sgrmbroker",        // 系统守护者
    "trustedinstaller",  // 组件安装器
    "msiexec",           // 安装服务
    "wscbroker",
    "wscmon",
    "mpdefendercoreservice", // Defender 核心服务（Win11 常驻，实际进程名就是这个全称）
    "mpdefendercore",
    "defendersessionhelper",
    "defenderaiplatformhost",
    "mpdlpservice",
    "securityhealthservice",
    "securityhealthsystray",
    "windowsdefender",
    "eset", "ekrn", "egui",  // 第三方杀软（强杀会被判定为攻击）
    "avp", "avpui",          // Kaspersky
    "mcshield", "mcuicnt",   // McAfee
    "avast", "aswidsagent",
    "avgnt", "avguard",      // Avira
    "360tray", "360sd", "zhudongfangyu", "360safe",
    "huorong", "hipsdaemon", "wsctrl",  // 火绒
    "qqpcmgr", "qmpersonalcenter",

    // ================= 外壳 / 桌面组件 =================
    // 这些是 Windows 11 的桌面挂件与游戏服务，杀了任务栏会缺块
    "widgetboard",
    "widgetservice",
    "widgets",
    "gamingservices",
    "gamingservicesnet",
    "gamebar",
    "gamebarpresencewriter",
    "xboxgip", "xboxgamebarwidgets",
    "openconsole",           // Windows Terminal 的控制台宿主
    "windows.terminal",      // 杀它终端全关
    "systemsettings",
    "shellexperiencehost",
    "startmenuexperiencehost",
    "lockapp",
    "logonui",
    "userinit",
    "fontdrvhost",

    // ================= 远程控制 / 会话 =================
    // 结束它们等于把用户的远程通道掐了
    "screenconnect.windowsclient",
    "screenconnect.clientservice",
    "mstsc",
    "rdpclip",
    "rdpservice",
    "teamviewer", "teamviewer_service",
    "anydesk",
    "todesk", "todesk_daemon",
    "sunloginclient", "sunloginremote",
    "rustdesk",
    "vncserver", "winvnc",

    // ================= 显卡 / 输入 / 外设 =================
    "nvvsvc", "nvdisplay.container", "nvcontainer", "nvsphelper64", "nvtelemetry",
    "nvxdsync", "nvidia web helper", "nvbackend",
    "nvidia overlay", "nvidia share", "nvtopps", "nvnodejslauncher",
    "atiesrxx", "atieclxx", "amdow", "radeonsoftware", "amdrsserv",
    "igfxem", "igfxtray", "igfxpers", "igfxhk",
    "audiodg",           // 音频设备图 —— 杀它没声音
    "ctfmon",            // 输入法框架 —— 杀它打不了中文
    "tabtip",
    "inputmethod",
    "chsime", "chsime",
    "sogoucloud", "sgtool", "sogouinput",
    "qqpinyin", "qqpy",
    "baidupinyin",
    // 常见第三方输入法（装了就得让它活着，否则用户打不了字）
    "iflyinput", "iflyplatform", "iflyime",
    "weixininput", "wxinput",
    "huaweiime", "hwpinyin",
    "rime", "weasel", "squirrel",
    "wermgr",            // Windows 错误报告

    // ================= 存储 / 文件系统 =================
    "ntfs",
    "volsnap",
    "vssvc",             // 卷影复制
    "searchindexer",     // 索引器（可优化但不可强杀）
    "searchprotocolhost",
    "searchfilterhost",
    "defrag",
    "diskpart",
    "fsutil",
    "bitlockerdeviceencryption",
    "bdservicehost",

    // ================= 网络栈 =================
    "dnscache",
    "dhcp",
    "nsi",
    "netprofm",
    "nlasvc",
    "lmhsvc",
    "netbt",
    "wlansvc",
    "wlansec",
    "dot3svc",
    "mpssvc",            // Windows 防火墙
    "bfe",               // 基础筛选引擎
    "ikeman",
    "policyagent",
    "remoteaccess",
    "sharedaccess",      // ICS —— 杀它热点断
    "vpnagent", "vpnservice",
    "openvpn", "openvpnserv",
    "wireguard",

    // ================= 虚拟化 / 容器 =================
    "vmms", "vmcompute", "vmwp",           // Hyper-V
    "vboxservice", "vboxtray",             // VirtualBox 增强
    "vmnat", "vmnetdhcp", "vmware-authd",  // VMware
    "docker", "dockerd", "com.docker.backend", "vpnkit",
    "wslservice", "wsl", "wslhost", "lxssmanager",
    "hns",               // 主机网络服务

    // ================= 电源 / 热管理 =================
    "powermgr",
    "acpi",
    "intelppm",
    "amdppm",

    // ================= 关键用户态服务宿主 =================
    "sppsvc",
    "wuauclt",           // Windows 更新
    "usoclient",
    "tiworker",
    "trustedinstaller",
    "cryptsvc",
    "themes",
    "audiosrv",
    "audioendpointbuilder",
    "deviceassociationframework",
    "devicesetupmanager",
    "plugplay",
    "dcomlaunch",
    "rpcss",
    "rpceptmapper",
    "profsvc",
    "gpsvc",             // 组策略
    "appinfo",           // UAC 提权依赖 —— 杀它无法提权
    "seclogon",
    "schedule",          // 任务计划
    "eventlog",          // 事件日志 —— 杀它系统不稳
    "event system",
    "wevtsvc",
    "wdiservicehost",
    "wdisystemhost",
];

/// 进程名黑名单的模糊规则：命中任一子串即视为系统组件。
///
/// 用于兜住第三方安全软件与厂商服务（名称带版本号/随机后缀，无法穷举）。
const DENY_PREFIXES: &[&str] = &[
    "msmpeng",
    "mssense",
    "nissrv",
    "sgrm",
    "securityhealth",
    "windefend",
    "csrss",
    "wininit",
    "winlogon",
    "lsass",
    "smss",
    "svchost",
    "dwm",
    "audiodg",
];

/// 系统关键 **服务名** 黑名单（小写）。用于 `sc query` / SCM 场景。
pub const CRITICAL_SERVICES: &[&str] = &[
    "rpcss", "dcomlaunch", "rpcendpointmapper", "plugplay", "power",
    "themes", "audioendpointbuilder", "audiosrv", "bfe", "mpssvc",
    "eventlog", "schedule", "gpsvc", "profsvc", "seclogon", "appinfo",
    "wuauserv", "bits", "cryptsvc", "trustedinstaller", "msiserver",
    "windefend", "wscsvc", "securityhealthservice", "samss", "lanmanserver",
    "lanmanworkstation", "dnscache", "dhcp", "nsi", "netprofm", "nlasvc",
    "lmhosts", "lmhsvc", "wlansvc", "wlansec", "dot3svc", "termservice",
    "vss", "swprv", "sppsvc", "w32time", "wdiservicehost", "wdisystemhost",
    "deviceassociationframework", "devicesetupmanager", "dcomlaunch",
];

/// 系统关键 **驱动** 黑名单（小写）。P2 阶段的服务/驱动管理会用到。
pub const CRITICAL_DRIVERS: &[&str] = &[
    "ntfs", "volsnap", "volmgr", "volmgrx", "fvevol", "disk", "partmgr",
    "storport", "stornvme", "storahci", "iastorv", "ndis", "tcpip",
    "netbt", "afd", "http", "wanarp", "npfs", "msfs", "fltmgr",
    "ksecdd", "ksecpkg", "cng", "ci", "clfs", "tm", "acpi", "pci",
    "intelppm", "amdppm", "processr", "usbhub", "usbport", "usbxhci",
    "hidclass", "hidusb", "kbdclass", "mouclass", "dxgkrnl", "dxgmms2",
    "watchdog", "wdf01000", "wdfldr", "win32k", "win32kbase", "win32kfull",
];

/// 系统保护目录前缀（小写）。位于其中的进程一律高度可疑。
const SYSTEM_DIRS: &[&str] = &[
    "c:\\windows\\system32\\",
    "c:\\windows\\syswow64\\",
    "c:\\windows\\winsxs\\",
    "c:\\windows\\systemapps\\",
    "c:\\windows\\servicing\\",
    "c:\\windows\\boot\\",
];

/// 归一化进程名：去 `.exe`、去首尾空白、转小写。
pub fn normalize(name: &str) -> String {
    let n = name.trim().to_ascii_lowercase();
    n.strip_suffix(".exe").unwrap_or(&n).to_string()
}

fn deny_set() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| DENY_NAMES.iter().copied().collect())
}

/// 名称是否直接命中硬黑名单。
pub fn is_denied_name(name: &str) -> bool {
    let n = normalize(name);
    if n.is_empty() {
        return true; // 拿不到名字的进程一律不碰
    }
    if deny_set().contains(n.as_str()) {
        return true;
    }
    DENY_PREFIXES.iter().any(|p| n.starts_with(p))
}

/// 路径是否位于系统保护目录。
pub fn is_system_path(path: &str) -> bool {
    let p = path.trim().to_ascii_lowercase().replace('/', "\\");
    SYSTEM_DIRS.iter().any(|d| p.starts_with(d))
}

/// 进程 PID 是否属于必须保护的内核/系统伪进程。
///
/// - PID 0 = System Idle Process
/// - PID 4 = System
pub fn is_protected_pid(pid: u32) -> bool {
    pid <= 4
}

/// 黑名单条目总数（用于报告展示）。
pub fn coverage() -> (usize, usize, usize) {
    (DENY_NAMES.len(), CRITICAL_SERVICES.len(), CRITICAL_DRIVERS.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denies_core_processes() {
        for n in ["lsass.exe", "LSASS", "csrss.exe", "svchost.exe", "System", "explorer.exe"] {
            assert!(is_denied_name(n), "{n} 必须被拦截");
        }
    }

    #[test]
    fn denies_pseudo_pids() {
        assert!(is_protected_pid(0));
        assert!(is_protected_pid(4));
        assert!(!is_protected_pid(1000));
    }

    #[test]
    fn empty_name_is_denied() {
        assert!(is_denied_name(""));
        assert!(is_denied_name("   "));
    }

    #[test]
    fn allows_normal_apps() {
        for n in ["notepad.exe", "chrome.exe", "wechat.exe", "steam.exe"] {
            assert!(!is_denied_name(n), "{n} 不应被硬拦截");
        }
    }

    #[test]
    fn detects_system_path() {
        assert!(is_system_path("C:\\Windows\\System32\\foo.dll"));
        assert!(is_system_path("c:/windows/system32/bar.exe"));
        assert!(!is_system_path("C:\\Program Files\\App\\app.exe"));
    }
}
