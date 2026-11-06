//! 启动项风险分级规则库。
//!
//! ## 设计立场
//!
//! 分级只回答一个问题：**「禁用它会怎样」**。因此规则的重心在「什么绝对不能动」，
//! 而不是「什么看起来可疑」。凡是可能属于安全软件、音频、输入法、触控板、加密、
//! 显卡驱动的条目，一律进 `Dangerous`，宁可漏放也不误禁。
//!
//! 四档语义：
//!
//! | 档位 | 语义 | UI 行为 |
//! |---|---|---|
//! | `Safe` | 禁用后不影响系统正常使用 | 默认勾选，绿标 |
//! | `Caution` | 可能影响某个功能 | 可勾选，黄标 + 理由 |
//! | `Dangerous` | 禁用会破坏系统功能 | 勾选框置灰，红标 |
//! | `Unknown` | 信息不足，无法判断 | 可勾选但默认不选，灰标 |
//!
//! ## 与 `blacklist.rs` 的分工
//!
//! `blacklist.rs` 管的是「进程能不能结束」，是硬红线；
//! 这里管的是「启动项能不能禁用」，是建议分级。
//! 两者的危险名单有交集但不共用，因为**禁用启动项比结束进程温和得多** ——
//! 结束进程是即时破坏，禁用启动项只是下次开机不启动，且随时可还原。

use crate::core::types::{RiskLevel, SignatureStatus, StartupSource};

/// 命令行的可执行文件路径抽取结果。
/// 从命令行 / `ImagePath` 里取出真实文件路径。
///
/// 三种已知的脏数据都要挡住：
///
/// 1. **带引号**：`"C:\Program Files\A B\app.exe" --run`
/// 2. **不带引号但路径含空格** —— 不能按空格切，得找第一个以可执行扩展名
///    结尾的片段。早先只认 `.exe`，于是 360 驱动大师的
///    `C:\Program Files (x86)\360\360DrvMgr\DrvMgrService.dll` 退化成
///    `C:\Program`，而 `C:\Program` 看着像「根目录下的一个文件」，
///    被当成系统组件 —— 整条服务因此判成「位于系统目录的自动启动服务」。
///    服务与驱动的命令行本来就是 DLL / SYS，扩展名表必须带上。
/// 3. **NUL 之后的残渣**：实测 360 温度监控的 `ServiceDll` 值是
///    `C:\Program Files (x86)\360\360TptMon\TMService.dll\0ice` ——
///    安装程序用短字符串覆盖长字符串时没缩短缓冲区，`ice` 是上一次写入
///    `Service` 的最后三个字母。Windows 语义里字符串到 NUL 就结束，
///    不截断的话界面上就会显示 `...\TMService.dll ice` 这种怪东西。
pub fn exe_from_command(cmd: &str) -> Option<String> {
    let s = match cmd.find('\0') {
        Some(i) => &cmd[..i],
        None => cmd,
    }
    .trim();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_prefix('"') {
        let end = rest.find('"')?;
        return Some(rest[..end].to_string());
    }
    // 没有引号：找第一个以可执行扩展名结尾的片段
    const EXTS: [&str; 7] = [".exe", ".dll", ".sys", ".com", ".bat", ".cmd", ".scr"];
    let lower = s.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut best: Option<usize> = None;
    for ext in EXTS {
        let mut from = 0usize;
        while let Some(rel) = lower[from..].find(ext) {
            let i = from + rel;
            let end = i + ext.len();
            // 扩展名后面必须断开，免得 `.com` 命中 `.company`、`.exe` 命中 `.execonfig`
            if end == bytes.len() || !bytes[end].is_ascii_alphanumeric() {
                best = Some(best.map_or(end, |b| b.min(end)));
                break;
            }
            from = i + 1;
        }
    }
    if let Some(end) = best {
        return Some(s[..end].to_string());
    }
    // 连扩展名都没有：只能取第一个空白前的部分
    Some(s.split_whitespace().next()?.to_string())
}

/// 展开命令行里的环境变量（`%ProgramFiles%` / `%SystemRoot%` 等）。
pub fn expand_env(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let bytes: Vec<char> = path.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '%' {
            if let Some(rel) = bytes[i + 1..].iter().position(|c| *c == '%') {
                let name: String = bytes[i + 1..i + 1 + rel].iter().collect();
                if !name.is_empty() && !name.contains(' ') {
                    let val = std::env::var(&name).ok();
                    if let Some(v) = val {
                        out.push_str(&v);
                        i = i + rel + 2;
                        continue;
                    }
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// 绝不能禁用的启动项特征（命中即 `Dangerous`）。
///
/// 每条都对应一个真实会出问题的场景，不是拍脑袋加的。
const MUST_KEEP_KEYWORDS: &[(&str, &str)] = &[
    // —— 安全 ——
    ("windows defender", "Windows Defender 是系统杀毒，禁用等于裸奔"),
    ("defender", "Windows Defender 组件，禁用会失去实时防护"),
    ("securityhealth", "Windows 安全中心，禁用后面板无法工作"),
    ("security center", "安全中心"),
    ("kaspersky", "第三方杀毒软件"),
    ("eset", "第三方杀毒软件"),
    ("avast", "第三方杀毒软件"),
    ("avira", "第三方杀毒软件"),
    ("mcafee", "第三方杀毒软件"),
    ("norton", "第三方杀毒软件"),
    ("bitdefender", "第三方杀毒软件"),
    // 360 只能认「安全产品本体」的具体标识，**绝不能用裸 `360`**。
    // 360 有一大票非安全产品：360压缩、360画报(壁纸)、360驱动大师、360温度监控、
    // 360浏览器……裸词会把它们统统判成「安全软件」并禁止禁用 ——
    // 实测 360画报(360huabao.exe)、360压缩更新器(360zipUpdate.exe)、
    // 360驱动大师(DrvMgrService.dll) 全部因此被误判，而前两个连 360 自己的
    // 启动项管理都允许禁止。
    ("360safe", "360 安全卫士"),
    ("360tray", "360 安全卫士托盘，禁用后失去实时防护"),
    ("360sd", "360 杀毒，禁用后失去实时防护"),
    ("360rp", "360 杀毒实时防护组件"),
    ("360hips", "360 主动防御驱动"),
    ("zhudongfangyu", "360 主动防御服务，禁用后失去行为拦截"),
    ("360安全卫士", "360 安全卫士"),
    ("360杀毒", "360 杀毒"),
    ("360total", "360 Total Security"),
    ("qhsafe", "360 安全组件"),
    ("huorong", "火绒安全软件"),
    ("sylame", "火绒安全软件组件"),
    // 同理不能裸用 `tencent`：腾讯视频、腾讯会议、腾讯文档都会中招。
    ("qqpctray", "QQ 电脑管家托盘"),
    ("qqpcrtp", "QQ 电脑管家实时防护"),
    ("tencentpcmanager", "腾讯电脑管家"),
    ("腾讯电脑管家", "腾讯电脑管家"),
    // —— 加密 / 凭据 ——
    ("bitlocker", "磁盘加密，禁用可能导致无法解锁"),
    ("bdesvc", "BitLocker 加密服务"),
    ("credential", "凭据管理，禁用会导致无法登录或保存密码"),
    ("vaultsvc", "凭据保管库"),
    // —— 音频 ——
    ("audiosrv", "Windows 音频服务，禁用后没有声音"),
    ("audioendpointbuilder", "音频端点管理"),
    ("realtek", "声卡驱动组件"),
    ("waves", "音频增强组件"),
    ("nahimic", "音频增强组件"),
    ("dolby", "音频增强组件"),
    ("conexant", "声卡驱动组件"),
    ("voicemeeter", "虚拟声卡"),
    // —— 输入 ——
    // 不能用裸 `ime`：`quicktime`、`multimedia`、`runtime` 里都含这三个字母。
    // 实测 QuickTime 被判「输入法，禁用后无法打中文」、多媒体类调度服务
    // （Multimedia Class Scheduler）同样中招 —— 理由荒唐到用户一眼就能看穿。
    ("imewdbld", "输入法词库组件"),
    ("inputmethod", "输入法组件"),
    ("输入法", "输入法，禁用后无法打中文"),
    ("ifly", "讯飞输入法"),
    // 同理不能裸用 `sogou`：搜狗浏览器、搜狗导航会被误判成输入法。
    ("sogoupinyin", "搜狗拼音输入法，禁用后无法打中文"),
    ("sogoupy", "搜狗拼音输入法组件"),
    ("sogouime", "搜狗输入法组件"),
    ("sginput", "搜狗输入法组件"),
    ("sgtool", "搜狗输入法工具"),
    ("搜狗拼音", "搜狗拼音输入法"),
    ("搜狗输入法", "搜狗输入法"),
    ("weixininput", "微信输入法"),
    ("ctfmon", "文本输入服务，禁用后无法切换输入法"),
    ("textinputhost", "文本输入宿主"),
    ("tabletinput", "手写与触控输入"),
    ("touch keyboard", "触摸键盘"),
    ("touchpad", "触控板"),
    ("synaptics", "触控板驱动"),
    ("syntp", "触控板驱动"),
    ("elan", "触控板驱动"),
    ("alps", "触控板驱动"),
    ("precision touchpad", "精密触控板"),
    // —— 显卡 ——
    ("display driver", "显卡驱动组件"),
    ("nvdisplay", "NVIDIA 显示服务，禁用可能导致黑屏或分辨率异常"),
    ("nvidia display", "NVIDIA 显示服务"),
    ("nvcontainer", "NVIDIA 容器服务，禁用后控制面板失效"),
    ("radeon", "AMD 显卡驱动组件"),
    ("amd external events", "AMD 显卡驱动组件"),
    ("igfx", "Intel 核显驱动组件"),
    ("intel graphics", "Intel 显卡驱动组件"),
    // —— 系统核心 ——
    ("rpc", "远程过程调用，系统基石，禁用会大面积故障"),
    ("dcom", "DCOM 服务"),
    ("plug and play", "即插即用，禁用后外设不识别"),
    ("power management", "电源管理，禁用后休眠/关机异常"),
    ("powrprof", "电源管理组件"),
    ("windows management instrumentation", "WMI，大量软件依赖"),
    ("winmgmt", "WMI 服务"),
    ("task scheduler", "计划任务服务"),
    ("event log", "事件日志服务，禁用后系统排障能力丧失"),
    ("dhcp", "自动获取 IP，禁用后上不了网"),
    // 不能裸用 `dns`：Bonjour 的 mDNSResponder.exe 里就含这三个字母，
    // 实测被判成「DNS 客户端，禁用后无法解析域名」—— 而 Bonjour 只是
    // 局域网设备发现，禁掉对上网毫无影响。
    ("dns client", "DNS 客户端，禁用后无法解析域名"),
    ("dnscache", "DNS 缓存服务，禁用后无法解析域名"),
    ("netprofm", "网络配置服务，禁用后网络状态异常"),
    ("nlasvc", "网络位置感知服务"),
    ("network list", "网络列表服务"),
    ("wlan", "无线网络组件"),
    ("bluetooth", "蓝牙组件"),
    ("print spooler", "打印后台处理，禁用后无法打印"),
    ("printworkflow", "打印工作流"),
    ("shell hardware detection", "外壳硬件检测"),
    ("shell experience", "外壳体验组件"),
    ("explorer", "资源管理器，禁用后桌面消失"),
    ("start menu", "开始菜单"),
    ("searchindexer", "系统搜索索引"),
    ("wsearch", "系统搜索服务"),
    ("fontdrvhost", "字体驱动宿主，禁用后界面字体异常"),
    ("font cache", "字体缓存服务"),
    ("themes", "主题服务，禁用后界面变成经典样式"),
    ("user profile", "用户配置服务"),
    ("group policy", "组策略"),
    ("application information", "应用信息服务"),
    ("cryptographic", "加密服务，大量系统功能依赖"),
    ("superfetch", "内存预读，禁用后程序启动变慢"),
    ("sysmain", "内存预读服务"),
    ("windows update", "系统更新，禁用后无法打补丁（有安全风险）"),
    ("wuauserv", "Windows 更新服务"),
    ("windows installer", "Windows Installer，禁用后无法安装/卸载软件"),
    ("msiserver", "Windows Installer 服务"),
];

/// 命中即 `Caution`：可以禁用，但**明确会影响某个功能**，得让用户自己判断。
///
/// 为什么单独分出这一层：`launcher` 这类词以前直接躺在 `SAFE` 里，
/// 于是《三角洲行动》的启动器被判成「可安全禁用」—— 用户一勾一点，
/// 下次开机游戏就起不来了。`Safe` 的语义是「禁用后不影响正常使用」，
/// 这些词够不上：它们只是「不致命」，不是「没代价」。
const CAUTION_KEYWORDS: &[(&str, &str)] = &[
    (
        "launcher",
        "程序启动器：禁用后该软件不会随开机自动就绪，游戏类可能因此无法进入",
    ),
    (
        "tzautoupdate",
        "Windows 时区自动更新：禁用后时区与夏令时规则不会自动同步",
    ),
    (
        "directxdatabaseupdater",
        "DirectX 图形缓存维护任务：禁用后游戏首次加载会略慢",
    ),
];

/// 明确「可以安全禁用」的启动项特征（命中即 `Safe`）。
///
/// 都是「自己会更新自己」的后台更新器、厂商预装的助手/推广程序。
/// 禁用的代价是「下次不自动检查更新」，用户手动打开软件时更新照样会跑。
const SAFE_KEYWORDS: &[(&str, &str)] = &[
    ("googleupdate", "Google 自动更新器，禁用后 Chrome 仍可正常使用"),
    ("googlecrashhandler", "Google 崩溃上报，纯后台"),
    ("microsoftedgeupdate", "Edge 自动更新器，浏览器本身不受影响"),
    ("officeclicktorun", "Office 后台更新器，禁用后 Office 仍可使用"),
    ("adobe", "Adobe 更新助手 / 创意云常驻，纯后台"),
    ("acrotray", "Adobe Acrobat 托盘助手"),
    ("acrobat", "Adobe Acrobat 更新组件"),
    ("jusched", "Java 自动更新检查器，老式启动项"),
    ("java update", "Java 更新检查器"),
    ("sunjavaupdatesched", "Java 更新检查器"),
    ("adobearm", "Adobe 阅读器后台"),
    ("quicktime", "QuickTime 托盘"),
    ("ituneshelper", "iTunes 助手，纯后台"),
    ("bonjour", "Bonjour 服务，只有局域网发现功能需要"),
    ("steam", "Steam 客户端自启（想开机就启动游戏平台的用户可保留）"),
    ("epicgames", "Epic 启动器自启"),
    ("battle.net", "战网自启"),
    ("discord", "Discord 自启"),
    ("spotify", "Spotify 自启"),
    ("zoom", "Zoom 自启"),
    ("skype", "Skype 自启"),
    ("teams", "Teams 自启"),
    ("onedrive", "OneDrive 同步客户端自启（不影响文件本身）"),
    ("dropbox", "Dropbox 同步自启"),
    ("baidunetdisk", "百度网盘自启"),
    ("xunlei", "迅雷自启"),
    ("thunder", "迅雷自启"),
    ("qqprotect", "QQ 安全防护组件（不影响 QQ 聊天）"),
    ("updater", "自动更新程序，属于典型可禁用项"),
    ("update.exe", "自动更新程序"),
    ("autoupdate", "自动更新程序"),
    ("softwareupdate", "厂商更新程序"),
    // 这里**曾经**有裸 `assistant` / `helper`，后来删掉了 —— 和裸 `360` 是同一类错。
    // 这两个词太泛，任何名字里带它们的**系统组件**都会被判成「厂商预装助手，可安全禁用」：
    // 实测 `IP Helper`（IPv6 隧道与网络辅助服务）、`Program Compatibility Assistant`
    // 都中过招。光凭「名字里有个 helper」就下「可以关」的结论是不负责任的，
    // 这类泛化词现在只在知识库里作为**兜底说明**存在（`generic: true`），
    // 而且明确不许改写风险档。真要给某个助手类程序定档，就写具体标识。
    ("oem", "厂商预装组件"),
    ("push", "消息推送组件"),
    ("news", "资讯推送组件"),
    ("promotion", "推广组件"),
    ("gamecenter", "厂商游戏中心"),
    ("gamemanager", "厂商游戏管理器"),
    ("virtualassistant", "厂商语音助手"),
    ("cortana", "语音助手"),
    ("xbox", "Xbox 相关后台组件"),
    ("nvidia share", "NVIDIA 录屏组件，与显示驱动无关"),
    ("nvidia overlay", "NVIDIA 游戏内覆盖层，与显示驱动无关"),
    ("geforce experience", "NVIDIA 附加组件"),
    ("gamingservices", "Xbox 游戏服务后台"),
];

/// 可疑特征 —— 不作为 `Dangerous`，而是把 `Safe` 降级到 `Caution`。
///
/// 注意：这张表**按原样子串匹配完整命令行/路径**（不走 `hits` 的左词边界）。
/// 因为这里的键本身就是带分隔符或连字符的路径 / 参数片段
/// （`\temp\`、`-encodedcommand`），本来就有天然边界；
/// 而 `powershell` 要能命中 `C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe`
/// 这种中间连写的目录名，强加词边界反而会漏判。
const SUSPICIOUS_PATTERNS: &[(&str, &str)] = &[
    ("\\temp\\", "路径位于临时目录，通常是安装残留"),
    ("\\tmp\\", "路径位于临时目录"),
    ("\\appdata\\local\\temp", "路径位于临时目录"),
    ("-encodedcommand", "使用编码命令，可能隐藏真实行为"),
    ("-windowstyle hidden", "隐藏窗口运行"),
    ("powershell", "通过 PowerShell 启动，行为不透明"),
    ("cmd.exe /c", "通过命令解释器启动"),
    ("mshta", "通过 mshta 启动，常被滥用"),
    ("rundll32", "通过 rundll32 启动，需确认目标 DLL"),
    ("wscript", "通过脚本宿主启动"),
    ("cscript", "通过脚本宿主启动"),
    ("regsvr32", "通过 regsvr32 注册组件"),
];

/// 判定结果：风险档 + 中文理由 + 是否允许禁用。
pub struct Assessment {
    pub risk: RiskLevel,
    pub reason: String,
}

impl Assessment {
    fn new(risk: RiskLevel, reason: impl Into<String>) -> Self {
        Self {
            risk,
            reason: reason.into(),
        }
    }
    /// `Dangerous` 不允许禁用，其余允许。
    pub fn can_disable(&self) -> bool {
        self.risk != RiskLevel::Dangerous
    }
}

/// 把内核对象路径归一成 `windows\system32\...` 这样的相对尾巴。
///
/// 兼容的书写形式：
/// - 带盘符：`c:\windows\system32\x.dll`
/// - 相对形式（驱动的 `ImagePath` 常这么写）：`system32\x.dll`、`System32\drivers\peauth.sys`
/// - 前缀变体：`\SystemRoot\system32\x.sys`、`\??\C:\windows\x.exe`
fn system_relative(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase().replace('/', "\\");
    let mut p = lower.as_str();
    for pre in ["\\??\\", "\\\\?\\"] {
        if let Some(r) = p.strip_prefix(pre) {
            p = r;
        }
    }
    // 去掉盘符，`d:\windows\...` 和 `c:\windows\...` 一视同仁
    let b = p.as_bytes();
    if b.len() > 2 && b[1] == b':' && b[2] == b'\\' {
        p = &p[3..];
    }
    // `\SystemRoot\...` 这类写法剥掉前缀后还留着开头那个反斜杠
    p = p.trim_start_matches('\\');
    if let Some(r) = p.strip_prefix("systemroot\\") {
        p = r;
    }
    p.to_string()
}

/// 判断路径是不是**系统自身**的组件（服务 / 驱动）。
///
/// 「直接位于系统目录」是关键：`C:\Windows\System32\kernel32.dll` 是系统文件；
/// 而 `C:\Windows\System32\360DrvMgr\DrvMgrService.dll` 是第三方厂商把
/// 自家模块塞进了 System32 的**子目录**，它照样是第三方程序。
///
/// 早先只判断「路径里含 `\windows\system32\`」，于是 360驱动大师、
/// 360温度监控这类服务全被判成「位于系统目录的自动启动服务」而**不建议禁用** ——
/// 与知识库里的「可以禁用」当场打架，也正是用户报的那一类识别错误。
fn is_system_component(raw: &str) -> bool {
    let rel = system_relative(raw);
    // `windows\` 可有可无：服务的 ImagePath 可能直接从 `system32\` 起头
    let tail = rel.strip_prefix("windows\\").unwrap_or(rel.as_str());

    // 驱动目录整体算系统组件：内核态的 `.sys` 一律保守处理。
    if is_kernel_driver(raw) {
        return true;
    }
    // 其余要求**直接**落在 windows / system32 / syswow64 下，不能再有子目录
    ["", "system32\\", "syswow64\\"].iter().any(|d| {
        tail.strip_prefix(d)
            .map_or(false, |rest| !rest.is_empty() && !rest.contains('\\'))
    })
}

/// 是不是内核态驱动（`System32\drivers\*.sys`）。
///
/// 单独拎出来是因为它是**硬底线**：驱动一禁，轻则外设失效，重则蓝屏起不来，
/// 所以连知识库都不许改写这个判定。
fn is_kernel_driver(raw: &str) -> bool {
    let rel = system_relative(raw);
    let tail = rel.strip_prefix("windows\\").unwrap_or(rel.as_str());
    tail.ends_with(".sys") && tail.contains("\\drivers\\")
}

/// 关键词命中判断：纯 ASCII 关键词要求**左词边界**。
///
/// 为什么：厂商普遍把自己的品牌名拼在通用词**前面** ——
/// `sogou`+`explorer`、`quick`+`time`、`multi`+`media`、`m`+`DNSResponder`。
/// 纯子串匹配会被这一整类「半路截胡」：
/// `explorer` 命中 `sogouexplorer`（搜狗浏览器被判「资源管理器」）、
/// `ime` 命中 `quicktime` / `multimedia`（被判「输入法，禁用后无法打中文」）、
/// `dns` 命中 `mDNSResponder`（Bonjour 被判「DNS 客户端」）。
/// 要求「前面不能紧跟 ASCII 字母数字」，一次挡住整类。
///
/// **不要求右边界**：品牌**前缀**是合法的，`360total`→`360totalsecurity`、
/// `ifly`→`iflyime`、`360safe`→`360safetray` 都得能命中。
/// 右侧的截胡（`tencent`→`tencentvideo`）靠关键词本身写具体化来解决。
///
/// 含空格或中文的关键词（`windows defender`、`360安全卫士`）按原样子串匹配。
fn hits(hay: &str, key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    let need_left_boundary = key.bytes().all(|b| b.is_ascii() && !b.is_ascii_whitespace());
    if !need_left_boundary {
        return hay.contains(key);
    }
    let hb = hay.as_bytes();
    let kb = key.as_bytes();
    if kb.len() > hb.len() {
        return false;
    }
    (0..=(hb.len() - kb.len())).any(|i| {
        // 命中位置前面要么是开头，要么不能是 ASCII 字母数字
        &hb[i..i + kb.len()] == kb && (i == 0 || !hb[i - 1].is_ascii_alphanumeric())
    })
}

/// 硬底线判定：给**已经算好的** `path_l` / `hay` 用。
fn hard_floor_of(path_l: &str, hay: &str) -> Option<Assessment> {
    if let Some((_, why)) = MUST_KEEP_KEYWORDS.iter().find(|(k, _)| hits(hay, k)) {
        return Some(Assessment::new(
            RiskLevel::Dangerous,
            format!("{why}，禁用会影响系统正常使用"),
        ));
    }
    if is_kernel_driver(path_l) {
        return Some(Assessment::new(
            RiskLevel::Dangerous,
            "内核态驱动，禁用可能导致系统无法启动或外设失效",
        ));
    }
    None
}

/// 不可被知识库改写的硬底线。
///
/// 知识库（尤其是用户自己补的那份）是人工维护的，写错一条就可能把杀毒软件
/// 放行 —— 用户勾一下、重启，机器就裸奔了。所以下面两类判定**永远优先于知识库**：
///
/// * 安全软件 / 凭据 / 音频 / 输入法 / 显卡驱动（`MUST_KEEP_KEYWORDS`）
/// * 内核态驱动（`System32\drivers\*.sys`）
///
/// 反过来，像「位于系统目录的自动启动服务」这种**保守但可协商**的判定
/// 就不在硬底线里：诊断策略服务（DPS）、地图服务（MapsBroker）这类
/// Windows 自带组件，微软自己都说可以关，一律锁死会把「一键优化」变成摆设。
/// 这类判定允许被知识库条目改写，而知识库条目是用户看得见、改得动的。
pub fn hard_floor(name: &str, command: &str, source: StartupSource) -> Option<Assessment> {
    let _ = source;
    let path = exe_from_command(command)
        .map(|p| expand_env(&p))
        .unwrap_or_default();
    let path_l = lower(&path);
    let base_l = lower(path_l.rsplit(['\\', '/']).next().unwrap_or_default());
    let hay = format!("{} {}", lower(name), base_l);
    hard_floor_of(&path_l, &hay)
}

/// 只靠「名字 + 命令行 + 来源」就能定级的规则链。
///
/// 返回 `Some` 表示已经定论，**不需要**再去验数字签名；
/// 返回 `None` 表示规则链走到头也没结论，必须靠签名兜底。
///
/// 之所以要拆出这一层：`WinVerifyTrust` 单条约 15 ms，148 条启动项串行要 2.2 s。
/// 而实测其中 105 条是自动启动服务、绝大多数位于 `C:\Windows\`，
/// 早就被「系统目录服务一律不建议禁用」这条规则拦下了 ——
/// 再花 15 ms 去验一个用不到的签名，纯属浪费。
pub fn assess_cheap(
    name: &str,
    command: &str,
    source: StartupSource,
) -> Option<Assessment> {
    let path = exe_from_command(command)
        .map(|p| expand_env(&p))
        .unwrap_or_default();
    let path_l = lower(&path);
    // 关键字只匹配「显示名 + 可执行文件基名」，不匹配参数部分。
    // 否则 `power` 会命中 `powershell.exe`、`amd` 会命中路径里的 `amd64`。
    let base_l = lower(path_l.rsplit(['\\', '/']).next().unwrap_or_default());
    let hay = format!("{} {}", lower(name), base_l);
    let cmd_l = lower(command);

    let in_system_dir = is_system_component(&path_l);

    // ---- 1. 硬性不可禁用 ----
    if let Some(floor) = hard_floor_of(&path_l, &hay) {
        return Some(floor);
    }
    // 系统目录里的服务，一律保守处理
    if source == StartupSource::Service && in_system_dir {
        return Some(Assessment::new(
            RiskLevel::Dangerous,
            "位于系统目录的自动启动服务，禁用风险高",
        ));
    }

    // ---- 2. 可以禁用，但有明确代价（黄标，交给用户判断）----
    if let Some((_, why)) = CAUTION_KEYWORDS.iter().find(|(k, _)| hits(&hay, k)) {
        return Some(Assessment::new(RiskLevel::Caution, *why));
    }

    // ---- 3. 明确可安全禁用 ----
    if let Some((_, why)) = SAFE_KEYWORDS.iter().find(|(k, _)| hits(&hay, k)) {
        // 若同时命中可疑特征，降一档
        if let Some((_, sus)) = SUSPICIOUS_PATTERNS
            .iter()
            .find(|(k, _)| cmd_l.contains(k) || path_l.contains(k))
        {
            return Some(Assessment::new(
                RiskLevel::Caution,
                format!("{why}；但{sus}，建议先确认来源"),
            ));
        }
        return Some(Assessment::new(RiskLevel::Safe, *why));
    }

    // ---- 4. 可疑特征 ----
    if let Some((_, sus)) = SUSPICIOUS_PATTERNS
        .iter()
        .find(|(k, _)| cmd_l.contains(k) || path_l.contains(k))
    {
        return Some(Assessment::new(
            RiskLevel::Caution,
            format!("{sus}，禁用前请确认这是你自己安装的程序"),
        ));
    }

    // ---- 5. 系统目录兜底（同样不需要签名）----
    if in_system_dir {
        return Some(Assessment::new(
            RiskLevel::Dangerous,
            "位于系统目录，属于系统组件，不建议禁用",
        ));
    }

    None
}

/// 签名还没校验出来时的**临时**分级。
///
/// 只在「快速扫描」路径下用：先让列表立刻出现在屏幕上，
/// 等签名异步补完后再用 [`assess`] 覆盖。因此这里的措辞必须让用户
/// 一眼看出「还没定论」，不能给出一个看起来已确定的结论。
pub fn assess_provisional(name: &str, command: &str, source: StartupSource) -> Assessment {
    if let Some(a) = assess_cheap(name, command, source) {
        return a;
    }
    let path = exe_from_command(command)
        .map(|p| expand_env(&p))
        .unwrap_or_default();
    let path_l = lower(&path);
    let in_user_profile = path_l.contains("\\users\\") || path_l.contains("\\appdata\\");
    if in_user_profile {
        Assessment::new(
            RiskLevel::Caution,
            "正在确认发布者…（位于用户目录，多半是你自己装的程序）",
        )
    } else {
        Assessment::new(RiskLevel::Caution, "正在确认发布者…")
    }
}

/// 对单个启动项做风险分级。
///
/// * `name`     —— 启动项显示名
/// * `command`  —— 命令行 / 服务二进制路径
/// * `source`   —— 来源（计划任务与服务的判定口径略有不同）
/// * `sig`      —— 可执行文件的签名状态
pub fn assess(
    name: &str,
    command: &str,
    source: StartupSource,
    sig: &SignatureStatus,
) -> Assessment {
    // 先走不需要签名的规则链；能定论就不看签名
    if let Some(a) = assess_cheap(name, command, source) {
        return a;
    }

    // ---- 5. 只能靠签名兜底 ----
    let path = exe_from_command(command)
        .map(|p| expand_env(&p))
        .unwrap_or_default();
    let path_l = lower(&path);
    let in_user_profile = path_l.contains("\\users\\") || path_l.contains("\\appdata\\");

    match sig {
        SignatureStatus::Valid(vendor) => {
            if in_user_profile {
                Assessment::new(
                    RiskLevel::Safe,
                    format!("{vendor} 签名，且装在用户目录，属于你自己安装的程序，开机自启非必需"),
                )
            } else {
                Assessment::new(
                    RiskLevel::Caution,
                    format!("{vendor} 签名的第三方程序，禁用前请确认你是否需要它开机自启"),
                )
            }
        }
        SignatureStatus::Unsigned => {
            if in_user_profile {
                Assessment::new(
                    RiskLevel::Caution,
                    "无数字签名，位于用户目录；可能是安装残留，建议确认后再禁用",
                )
            } else {
                Assessment::new(
                    RiskLevel::Caution,
                    "无数字签名且不在用户目录，来源不明确，建议先确认来源",
                )
            }
        }
        SignatureStatus::Invalid => Assessment::new(
            RiskLevel::Caution,
            "签名校验失败（文件可能被修改或证书过期），建议先确认来源",
        ),
        SignatureStatus::Unknown => {
            if path.is_empty() {
                Assessment::new(
                    RiskLevel::Unknown,
                    "拿不到可执行文件路径，无法判断影响，默认不动它",
                )
            } else {
                Assessment::new(
                    RiskLevel::Unknown,
                    "无法读取签名信息（文件可能已被删除或权限不足）",
                )
            }
        }
    }
}

// ===========================================================================
// 测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(v: &str) -> SignatureStatus {
        SignatureStatus::Valid(v.to_string())
    }

    #[test]
    fn exe_path_extraction_handles_quotes_and_spaces() {
        assert_eq!(
            exe_from_command(r#""C:\Program Files\A B\app.exe" --run"#).unwrap(),
            r"C:\Program Files\A B\app.exe"
        );
        assert_eq!(
            exe_from_command(r"C:\Windows\System32\svchost.exe -k netsvcs").unwrap(),
            r"C:\Windows\System32\svchost.exe"
        );
        assert_eq!(
            exe_from_command(r"C:\a b\c.exe /x").unwrap(),
            r"C:\a b\c.exe"
        );
        assert!(exe_from_command("").is_none());
    }

    #[test]
    fn env_expansion_works_and_is_safe_on_unknown() {
        std::env::set_var("OLD666_TEST_DIR", r"C:\zzz");
        assert_eq!(expand_env(r"%OLD666_TEST_DIR%\a.exe"), r"C:\zzz\a.exe");
        // 未知变量保持原样，不能吃掉内容
        assert_eq!(
            expand_env(r"%NOPE_NOT_SET_XYZ%\a.exe"),
            r"%NOPE_NOT_SET_XYZ%\a.exe"
        );
        assert_eq!(expand_env("no vars here"), "no vars here");
        // 落单的 % 不能死循环
        assert_eq!(expand_env("50% off"), "50% off");
    }

    #[test]
    fn defender_is_never_disableable() {
        let a = assess(
            "Windows Defender",
            r"C:\Program Files\Windows Defender\MsMpEng.exe",
            StartupSource::Service,
            &valid("Microsoft Corporation"),
        );
        assert_eq!(a.risk, RiskLevel::Dangerous);
        assert!(!a.can_disable());
    }

    #[test]
    fn audio_and_input_and_touchpad_are_dangerous() {
        for (n, c) in [
            ("Realtek Audio Service", r"C:\Windows\System32\RtkAudUService64.exe"),
            ("iFly Input Service", r"C:\Program Files\iFly\iflyime.exe"),
            ("Synaptics Touchpad", r"C:\Program Files\Synaptics\SynTP\SynTPEnh.exe"),
            ("BitLocker", r"C:\Windows\System32\bdesvc.dll"),
            ("NVIDIA Display Container", r"C:\Windows\System32\nvdisplay.container.exe"),
        ] {
            let a = assess(n, c, StartupSource::Service, &valid("X"));
            assert_eq!(a.risk, RiskLevel::Dangerous, "{n} 应为不建议禁用");
        }
    }

    #[test]
    fn update_helpers_are_safe() {
        for (n, c) in [
            ("Google Update", r"C:\Program Files\Google\Update\GoogleUpdate.exe"),
            ("Adobe Acrobat Update", r"C:\Program Files\Adobe\AcroTray.exe"),
            ("Microsoft Edge Update", r"C:\Program Files\EdgeUpdate\MicrosoftEdgeUpdate.exe"),
        ] {
            let a = assess(n, c, StartupSource::RunKey, &valid("X"));
            assert_eq!(a.risk, RiskLevel::Safe, "{n} 应为可安全禁用");
            assert!(a.can_disable());
        }
    }

    #[test]
    fn signed_user_installed_app_is_safe() {
        let a = assess(
            "SomeApp",
            r"C:\Users\me\AppData\Local\SomeApp\app.exe",
            StartupSource::RunKey,
            &valid("Some Vendor Co."),
        );
        assert_eq!(a.risk, RiskLevel::Safe);
        assert!(a.reason.contains("Some Vendor Co."));
    }

    #[test]
    fn unsigned_user_app_is_caution_not_safe() {
        let a = assess(
            "Mystery",
            r"C:\Users\me\AppData\Local\Mystery\m.exe",
            StartupSource::RunKey,
            &SignatureStatus::Unsigned,
        );
        assert_eq!(a.risk, RiskLevel::Caution);
        assert!(a.can_disable(), "谨慎档仍允许用户主动禁用");
    }

    #[test]
    fn suspicious_pattern_downgrades_safe_to_caution() {
        // 名字里带 updater（本应 Safe），但用 powershell 隐藏窗口启动
        let a = assess(
            "FakeUpdater",
            r"powershell -windowstyle hidden -encodedcommand ZgBvAG8A",
            StartupSource::RunKey,
            &SignatureStatus::Unsigned,
        );
        assert_eq!(a.risk, RiskLevel::Caution);
        assert!(a.reason.contains("隐藏窗口") || a.reason.contains("编码命令"));
    }

    #[test]
    fn system_dir_service_is_dangerous_even_if_unknown_vendor() {
        let a = assess(
            "SomeUnknownSvc",
            r"C:\Windows\System32\unknownsvc.exe",
            StartupSource::Service,
            &SignatureStatus::Unsigned,
        );
        assert_eq!(a.risk, RiskLevel::Dangerous);
    }

    #[test]
    fn missing_path_is_unknown_and_not_recommended() {
        let a = assess("Ghost", "", StartupSource::RunKey, &SignatureStatus::Unknown);
        assert_eq!(a.risk, RiskLevel::Unknown);
        assert!(a.can_disable());
    }

    #[test]
    fn dangerous_is_the_only_non_disableable_level() {
        for (risk, expected) in [
            (RiskLevel::Safe, true),
            (RiskLevel::Caution, true),
            (RiskLevel::Unknown, true),
            (RiskLevel::Dangerous, false),
        ] {
            let a = Assessment::new(risk, "x");
            assert_eq!(a.can_disable(), expected, "{risk:?}");
        }
    }

    /// 廉价规则链必须能在**不看签名**的前提下定论大多数条目 ——
    /// 这是把首次扫描从 2.2 s 压到 0.1 s 的关键。
    #[test]
    fn cheap_rules_decide_without_signature() {
        // 系统目录服务 → 直接 Dangerous
        assert!(assess_cheap(
            "SomeSvc",
            r"C:\Windows\System32\svchost.exe -k netsvcs",
            StartupSource::Service
        )
        .is_some());
        // 已知安全项 → 直接 Safe
        assert!(assess_cheap(
            "Google Update",
            r"C:\Program Files\Google\Update\GoogleUpdate.exe",
            StartupSource::RunKey
        )
        .is_some());
        // 必须保留项 → 直接 Dangerous
        assert!(assess_cheap(
            "Realtek Audio",
            r"C:\Windows\System32\RtkAudUService64.exe",
            StartupSource::Service
        )
        .is_some());
    }

    /// 反过来：真正需要签名的场景必须返回 `None`，不能瞎猜。
    #[test]
    fn cheap_rules_return_none_when_signature_is_required() {
        for (n, c, s) in [
            (
                "SomeVendorApp",
                r"C:\Program Files\VendorX\app.exe",
                StartupSource::RunKey,
            ),
            (
                "MyTool",
                r"C:\Users\me\AppData\Local\MyTool\t.exe",
                StartupSource::RunKey,
            ),
            ("Ghost", "", StartupSource::RunKey),
        ] {
            assert!(
                assess_cheap(n, c, s).is_none(),
                "{n} 应该需要签名才能定级"
            );
        }
    }

    /// 廉价链与完整链的结论必须一致 —— 拆成两层不能改变判定结果。
    #[test]
    fn cheap_and_full_agree_when_cheap_decides() {
        let cases = [
            ("Windows Defender", r"C:\Windows\System32\MsMpEng.exe", StartupSource::Service),
            ("Google Update", r"C:\Program Files\Google\Update\GoogleUpdate.exe", StartupSource::RunKey),
            ("SomeSvc", r"C:\Windows\System32\svchost.exe", StartupSource::Service),
            ("Thunder", r"C:\Program Files\Thunder\Thunder.exe", StartupSource::RunKey),
        ];
        for (n, c, s) in cases {
            let cheap = assess_cheap(n, c, s);
            if let Some(cheap) = cheap {
                // 无论给什么签名状态，完整链都必须返回同一个结论
                for sig in [
                    SignatureStatus::Unsigned,
                    valid("Some Vendor"),
                    SignatureStatus::Invalid,
                    SignatureStatus::Unknown,
                ] {
                    let full = assess(n, c, s, &sig);
                    assert_eq!(
                        full.risk, cheap.risk,
                        "{n} 在签名={sig:?} 时结论不一致"
                    );
                }
            }
        }
    }
}

/// 进程识别回归 —— 全部来自**实测误判**，每一条背后都有一次用户看得见的错。
///
/// 用户原话：「对进程识别错误，这是典型的360压缩，可以完全安全的禁用或者
/// 删除注册表启动项」。截图里 `360huabao.exe` 被标成「不建议禁用／安全软件，
/// 禁用会影响系统正常使用」。根因是关键词表里有一条裸 `("360", "安全软件")`，
/// 而匹配是**子串**匹配 —— 凡是名字里带 360 的都被判成安全软件。
///
/// 这个模块的存在意义：这类错误**单测抓不到**（每条规则单独看都对），
/// 只有拿真实进程名去打整条规则链才暴露。所以下面全部都走 `assess_cheap`
/// 的完整链路，而不是去测某个 `hits()` 的局部行为。
#[cfg(test)]
mod misgrade_regression {
    use super::*;

    /// 一个「签了名、来源看着正常」的状态，用来把签名因素从判定里排除掉。
    fn full(v: &str) -> SignatureStatus {
        SignatureStatus::Valid(v.to_string())
    }

    /// 用真实扫描到的命令行给一条启动项定级。
    fn assert_not_dangerous(name: &str, cmd: &str, source: StartupSource) {
        let a = assess(name, cmd, source, &full("Test Vendor"));
        assert_ne!(
            a.risk,
            RiskLevel::Dangerous,
            "「{name}」被误判为不建议禁用，理由：{}",
            a.reason
        );
    }

    /// 理由里**不能出现**这些词 —— 出现即说明关键词被别的东西截胡了。
    fn assert_reason_excludes(name: &str, cmd: &str, source: StartupSource, banned: &[&str]) {
        let a = assess(name, cmd, source, &full("Test Vendor"));
        for b in banned {
            assert!(
                !a.reason.contains(b),
                "「{name}」的理由出现「{b}」：{}",
                a.reason
            );
        }
    }

    // ---- 用户直接报的那条：360 压缩 / 360 画报 ----

    /// 360画报（360huabao.exe）是壁纸小工具，360 自己的启动项管理都允许禁止。
    #[test]
    fn bug_report_360huabao_is_safe_to_disable() {
        assert_not_dangerous(
            "360画报",
            r"C:/Users/X/AppData/Local/360huabao/360huabao.exe",
            StartupSource::RunKey,
        );
        // 理由里不能再出现「安全软件」
        assert_reason_excludes(
            "360画报",
            r"C:/Users/X/AppData/Local/360huabao/360huabao.exe",
            StartupSource::RunKey,
            &["安全软件", "实时防护"],
        );
    }

    /// 360压缩的更新器（360zipUpdate.exe）同理。
    #[test]
    fn bug_report_360zip_update_is_safe_to_disable() {
        assert_not_dangerous(
            "360压缩更新",
            r"C:/Program Files (x86)/360/360zip/360zipUpdate.exe",
            StartupSource::RunKey,
        );
    }

    /// 反向保护：**真正的** 360 安全产品仍然必须拦住。
    #[test]
    fn real_security_products_still_blocked() {
        for (n, c) in [
            ("360安全卫士", r"C:/Program Files (x86)/360/360Safe/safemon/360Tray.exe"),
            ("360杀毒", r"C:/Program Files (x86)/360/360sd/360sd.exe"),
            ("zhudongfangyu", r"C:/Program Files (x86)/360/360Safe/deepscan/ZhuDongFangYu.exe"),
            ("QQPCTray", r"C:/Program Files/Tencent/QQPCMgr/QQPCTray.exe"),
            ("Windows Defender", r"C:/Program Files/Windows Defender/MsMpEng.exe"),
        ] {
            let a = assess(n, c, StartupSource::RunKey, &full("Test Vendor"));
            assert_eq!(
                a.risk,
                RiskLevel::Dangerous,
                "「{n}」是安全产品，必须不允许禁用，实际：{:?} / {}",
                a.risk,
                a.reason
            );
        }
    }

    // ---- 子串截胡：裸词命中品牌前缀 ----

    /// 搜狗浏览器 `SogouExplorer.exe` —— 里面含 `explorer`。
    /// 早先被判「资源管理器，禁用后桌面消失」。
    #[test]
    fn sogouexplorer_is_not_windows_explorer() {
        assert_reason_excludes(
            "搜狗高速浏览器",
            r"C:/Program Files (x86)/SogouExplorer/SogouExplorer.exe",
            StartupSource::RunKey,
            &["资源管理器"],
        );
        // 确认左词边界真的挡住了
        assert!(!hits("sogouexplorer.exe", "explorer"));
        // 而真正的资源管理器照样认得出
        assert!(hits("windows explorer", "explorer"));
        assert!(hits("explorer.exe", "explorer"));
    }

    /// QuickTime 里含 `ime`，`Multimedia Class Scheduler` 里也含 `ime`。
    /// 早先都被判「输入法，禁用后无法打中文」。
    #[test]
    fn ime_keyword_does_not_hijack_quicktime() {
        assert!(!hits("quicktime.exe", "ime"));
        assert!(!hits("multimedia class scheduler", "ime"));
        // 真输入法必须还能命中：靠的是厂商自己的完整标识，而不是裸 `ime`
        assert!(hits("sogouime.exe", "sogouime"));
        assert!(hits("iflyime.exe", "ifly"));
        assert!(hits("sogouimebroker.exe", "sogouime"));
        assert!(hits("搜狗输入法", "输入法"));
    }

    /// Bonjour 的 `mDNSResponder.exe` 含 `dns`，早先被判「DNS 客户端」。
    #[test]
    fn mdnsresponder_is_not_dns_client() {
        assert!(!hits("mdnsresponder.exe", "dns"));
        assert_reason_excludes(
            "Bonjour 服务",
            r"C:/Program Files/Bonjour/mDNSResponder.exe",
            StartupSource::Service,
            &["DNS 客户端"],
        );
        assert!(hits("dns client", "dns"));
        assert!(hits("dnscache", "dns"));
    }

    /// 品牌**前缀**是合法的，不能因为加词边界把整类保护弄丢。
    #[test]
    fn brand_prefix_still_matches() {
        assert!(hits("360totalsecurity.exe", "360total"));
        assert!(hits("360safetray.exe", "360safe"));
    }

    // ---- 系统目录判定 ----

    /// 第三方把 DLL 塞进 `System32\360DrvMgr\` 子目录，**不该**算系统组件。
    /// 早先只看「含 `\windows\system32\`」，于是 360驱动大师、360温度监控
    /// 全被判「位于系统目录的自动启动服务，禁用风险高」。
    #[test]
    fn vendor_subdir_under_system32_is_not_system_component() {
        assert!(!is_system_component(
            r"c:/windows/system32/360drvmgr/drvmgrservice.dll"
        ));
        assert!(!is_system_component(
            r"c:/windows/system32/360tptmon/tmservice.dll"
        ));
        // 但直接躺在 system32 下的，就是系统文件
        assert!(is_system_component(r"c:/windows/system32/kernel32.dll"));
        assert!(is_system_component(r"c:/windows/system32/svchost.exe"));
        assert!(is_system_component(r"c:/windows/explorer.exe"));
        // 相对形式（驱动 ImagePath）也要认
        assert!(is_system_component(r"system32\drivers\peauth.sys"));
        assert!(is_system_component(r"\SystemRoot\system32\tcpipreg.sys"));
        assert!(is_system_component(r"\??\C:/Windows/System32/drivers/ndis.sys"));
        // 大小写与正斜杠不敏感
        assert!(is_system_component("C:/Windows/System32/Drivers/ndis.sys"));
    }

    /// 驱动不能再落到「无法判定」—— 而「无法判定」是**可以勾选**的，
    /// 用户一勾就把系统驱动关了。
    #[test]
    fn system_drivers_are_never_unknown() {
        for d in [
            r"System32\drivers\peauth.sys",
            r"system32\drivers\tcpipreg.sys",
            r"\SystemRoot\system32\drivers\ndis.sys",
        ] {
            let a = assess(d, d, StartupSource::Service, &SignatureStatus::Unknown);
            assert_ne!(a.risk, RiskLevel::Unknown, "{d} 不该是「无法判定」");
            assert!(!a.can_disable(), "{d} 是系统驱动，不该允许禁用");
        }
    }

    // ---- 知识库与风险档不许互相打架 ----

    /// 知识库说「可以禁用」，规则链却判「不建议禁用」—— 用户在界面上会同时
    /// 看到这两种说法（黄/红标 + 说明面板里的建议），一眼就知道程序在胡说。
    ///
    /// 这条断言曾经一次性抓出 12 处不一致，全部是裸关键词造成的误判。
    #[test]
    fn knowledge_advice_never_contradicts_risk_level() {
        use crate::core::knowledge;
        let mut bad = Vec::new();
        for e in knowledge::builtin() {
            if e.advice != "可以禁用" {
                continue;
            }
            for k in &e.keys {
                // 造一个「用户自己装的普通程序」路径，避开系统目录与签名分支
                let cmd = format!(r"C:/Users/X/AppData/Roaming/{k}/{k}.exe");
                if let Some(a) = assess_cheap(k, &cmd, StartupSource::RunKey) {
                    if a.risk == RiskLevel::Dangerous {
                        bad.push(format!("{k}（KB：{}）→ {}", e.title, a.reason));
                    }
                }
            }
        }
        assert!(
            bad.is_empty(),
            "知识库说「可以禁用」但风险链判「不建议禁用」，共 {} 条：\n  {}",
            bad.len(),
            bad.join("\n  ")
        );
    }

    /// 反向：知识库说「建议保留」，风险链却说「可安全禁用」——
    /// 会诱导用户把该留的东西关掉。
    #[test]
    fn knowledge_keep_entries_are_not_marked_safe() {
        use crate::core::knowledge;
        let mut bad = Vec::new();
        for e in knowledge::builtin() {
            if e.advice != "建议保留" {
                continue;
            }
            for k in &e.keys {
                let cmd = format!(r"C:/Users/X/AppData/Roaming/{k}/{k}.exe");
                if let Some(a) = assess_cheap(k, &cmd, StartupSource::RunKey) {
                    if a.risk == RiskLevel::Safe {
                        bad.push(format!("{k}（KB：{}）→ {}", e.title, a.reason));
                    }
                }
            }
        }
        assert!(
            bad.is_empty(),
            "知识库说「建议保留」但风险链判「可安全禁用」，共 {} 条：\n  {}",
            bad.len(),
            bad.join("\n  ")
        );
    }

    /// 知识库**不许放行硬底线**：任何写「可以禁用」的条目，
    /// 只要它的 key 会命中安全软件或内核驱动，就是致命的错 ——
    /// 用户勾一下重启，机器就裸奔了。
    ///
    /// 这条断言是 `knowledge::reconcile_risk` 那个「知识库压过关键词规则」设计的
    /// 安全绳：既然允许人工说明去改写风险档，就必须钉死哪些东西**永远不许被改写**。
    #[test]
    fn knowledge_never_overrides_hard_floor() {
        use crate::core::knowledge;
        let mut bad = Vec::new();
        for e in knowledge::builtin() {
            if e.advice != "可以禁用" {
                continue;
            }
            for k in &e.keys {
                let cmd = format!(r"C:\Users\X\AppData\Roaming\{k}\{k}.exe");
                if let Some(f) = hard_floor(k, &cmd, StartupSource::RunKey) {
                    bad.push(format!("{k}（{}）→ {}", e.title, f.reason));
                }
            }
        }
        assert!(
            bad.is_empty(),
            "知识库把「可以禁用」写给了硬底线条目，共 {} 条：\n  {}",
            bad.len(),
            bad.join("\n  ")
        );
    }

    /// 同一个 key 不许被**两条**知识库条目同时认领 ——
    /// 那样查表结果就取决于条目顺序，同一个进程名可能忽而「可以禁用」
    /// 忽而「建议保留」，用户看到的说明会前后不一致。
    ///
    /// 注意：同一条目内列多个**大小写变体**（`profSvc` + `profsvc`）是刻意的，
    /// 用来记录注册表里可能出现的两种拼写，不算冲突。
    #[test]
    fn knowledge_keys_are_unique_across_entries() {
        use crate::core::knowledge;
        use std::collections::HashMap;
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut dup = Vec::new();
        for (idx, e) in knowledge::builtin().iter().enumerate() {
            for k in &e.keys {
                let key = k.to_lowercase();
                match seen.get(&key) {
                    Some(&prev) if prev != idx => {
                        dup.push(format!(
                            "{key}：「{}」与「{}」",
                            knowledge::builtin()[prev].title, e.title
                        ));
                    }
                    _ => {
                        seen.insert(key, idx);
                    }
                }
            }
        }
        assert!(dup.is_empty(), "知识库存在跨条目重复 key：\n  {}", dup.join("\n  "));
    }
}
