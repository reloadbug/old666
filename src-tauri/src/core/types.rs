//! 全局共享数据类型 —— 前后端 IPC 契约的唯一定义处。
//!
//! 所有结构体都 `Serialize`，字段命名用 snake_case，前端按同名取值。

use serde::{Deserialize, Serialize};

/// 三档结论。这是产品对用户唯一需要解释的东西：能不能结束。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// 可安全结束 —— 绿色
    Safe,
    /// 建议保留 —— 黄色
    Keep,
    /// 禁止结束 —— 红色，硬拦截
    Forbidden,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Safe => "safe",
            Verdict::Keep => "keep",
            Verdict::Forbidden => "forbidden",
        }
    }
}

/// 数字签名状态。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SignatureStatus {
    /// 签名有效，附带签名主体
    Valid(String),
    /// 文件本身无签名
    Unsigned,
    /// 有签名但校验失败（被篡改 / 证书过期）
    Invalid,
    /// 无法判定（文件已消失 / 权限不足）
    Unknown,
}

/// 六维闲置信号打分，每维 0–100，越高越「可以结束」。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct IdleScore {
    /// 活跃度：CPU 占用越低分越高
    pub activity: u8,
    /// 前台关联：与当前前台窗口无关则分高
    pub foreground: u8,
    /// 使用频率：启动后长时间无 IO / 无窗口则分高
    pub frequency: u8,
    /// 签名来源：无签名或来源可疑则分高
    pub signature: u8,
    /// 系统关键性：非系统组件则分高
    pub criticality: u8,
    /// 用户白名单：在白名单里直接 0 分（永久豁免）
    pub whitelist: u8,
}

impl IdleScore {
    /// 加权总分。权重体现产品取向：**宁可少杀，不可错杀**。
    pub fn total(&self) -> u32 {
        // 关键性权重最高（安全兜底），白名单为硬否决
        let w = |v: u8, weight: u32| v as u32 * weight;
        (w(self.activity, 22)
            + w(self.foreground, 26)
            + w(self.frequency, 18)
            + w(self.signature, 10)
            + w(self.criticality, 24)
            + w(self.whitelist, 0))
            / 100
    }
}

/// 单个进程的完整快照。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub exe_path: Option<String>,
    /// 进程启动至今秒数
    pub uptime_secs: u64,

    // ---- 资源 ----
    /// CPU 占用百分比（0–100 × 核心数归一）
    pub cpu_percent: f32,
    /// 工作集（物理内存，字节）
    pub working_set: u64,
    /// 私有提交内存（字节）
    pub private_bytes: u64,
    pub thread_count: u32,
    pub handle_count: u32,
    /// 累计磁盘读取（字节）
    pub io_read_bytes: u64,
    /// 累计磁盘写入（字节）
    pub io_write_bytes: u64,

    // ---- 判定输入 ----
    pub session_id: u32,
    /// 是否持有**可见**顶层窗口
    pub has_window: bool,
    /// 是否持有**任意**顶层窗口（含隐藏）—— 托盘应用 / 输入法为 true
    pub has_tray_window: bool,
    /// 是否属于某个持有窗口的程序（子进程，如 chrome 渲染进程、IDE 下的 node）
    pub under_window_app: bool,
    /// 是否为当前前台进程
    pub is_foreground: bool,
    pub signature: SignatureStatus,
    /// 进程所属分组（用于列表分组头）
    pub group: String,

    // ---- 判定输出 ----
    pub verdict: Verdict,
    pub score: IdleScore,
    /// 人话解释：为什么给这个结论
    pub reasons: Vec<String>,
}

/// 系统级实时指标（顶部概览条用）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SystemMetrics {
    pub cpu_percent: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    /// 磁盘活动时间百分比
    pub disk_percent: f32,
    /// 磁盘读写速率（字节/秒）
    pub disk_read_bps: f64,
    pub disk_write_bps: f64,
    /// 网络收发速率（字节/秒）
    pub net_rx_bps: f64,
    pub net_tx_bps: f64,
    pub process_count: u32,
    pub thread_count: u32,
    pub uptime_secs: u64,
}

/// 一次净化操作中被结束的进程记录（用于回滚）。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct KilledRecord {
    pub pid: u32,
    pub name: String,
    pub exe_path: Option<String>,
    /// 结束时释放的工作集字节
    pub freed_bytes: u64,
    /// 结束方式：graceful / force / skipped
    pub method: String,
    /// 失败原因（成功为 None）
    pub error: Option<String>,
}

/// 净化结果。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PurgeResult {
    /// 快照 ID，用于回滚（演练模式下为空串）
    pub snapshot_id: String,
    pub timestamp: u64,
    /// 被结束的进程
    pub killed: Vec<KilledRecord>,
    /// 被工作集裁剪的进程
    pub trimmed: Vec<KilledRecord>,
    /// 因安全原因被拦下的进程（用户看得到的「我帮你挡住了」）
    pub blocked: Vec<KilledRecord>,
    /// 真实释放的物理内存字节
    pub freed_bytes: u64,
    /// **预计**可释放的物理内存字节（演练模式下即为全部收益）
    pub planned_bytes: u64,
    /// 整轮耗时毫秒
    pub elapsed_ms: u64,
    /// 是否为演练（`true` 时未对系统做任何实际改动）
    pub dry_run: bool,
}

/// 采集一次全量进程的耗时统计（P0 性能基线用）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct CollectStats {
    pub process_count: u32,
    pub enumerate_ms: f64,
    pub detail_ms: f64,
    pub total_ms: f64,
}

// ===========================================================================
// P2 · 开机加速
// ===========================================================================

/// 开机时间线的一个分段。
///
/// 数据源是 Windows 官方的 `Microsoft-Windows-Diagnostics-Performance/Operational`
/// 事件日志，而不是自己写 ETW 消费者 —— 后者要提权、要常驻、还容易漏事件，
/// 而系统已经把同一份数据算好并落了盘。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct BootPhase {
    /// 机器可读键：`firmware` / `kernel` / `drivers` / `devices` / `smss` / `pnp` / `profile` / `logon` / `postboot`
    pub key: String,
    /// 中文标签，UI 直接显示
    pub label: String,
    pub ms: u64,
    /// 分段归类：`main`（主路径，桌面可用前） / `post`（桌面可用后）
    pub kind: String,
    /// 该段是否属于「可优化」范畴（服务/启动项/驱动 → 用户能动手）
    pub actionable: bool,
}

/// 开机降级归因：某个拖慢了开机的具体对象。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct BootCulprit {
    /// 应用名 / 服务名 / 驱动名
    pub name: String,
    /// 友好名（系统给出的人话名称，可能为空）
    pub friendly_name: String,
    /// `app` / `service` / `driver` / `device`
    pub kind: String,
    /// 该对象总耗时
    pub total_ms: u64,
    /// 其中被判定为「拖慢」的部分
    pub degradation_ms: u64,
    /// 可执行文件路径（系统给出，可能为空）
    pub path: String,
    /// 厂商名（取自文件版本信息，可能为空）
    pub company: String,
}

/// 一次开机记录。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct BootRecord {
    /// 开机总耗时（毫秒）
    pub boot_ms: u64,
    /// 主路径耗时 —— 用户感知的「按下电源到能用」
    pub main_path_ms: u64,
    /// 桌面出现后的后台收尾耗时
    pub post_boot_ms: u64,
    /// 本次开机的起始时间（ISO 8601，来自 `BootStartTime`）
    pub start_time: String,
    /// 本次开机的结束时间
    pub end_time: String,
    /// 系统是否自评本次开机「降级」（有东西明显拖慢了）
    pub degraded: bool,
    /// 本次开机自启动的应用个数（系统统计）
    pub startup_apps: u32,
    /// 分段耗时，已按时间顺序排列
    pub phases: Vec<BootPhase>,
    /// 降级归因清单，按拖慢程度降序
    pub culprits: Vec<BootCulprit>,
}

/// 开机时间线的聚合视图（UI 一次拿全）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct BootTimeline {
    /// 最近若干次开机，最新在前
    pub records: Vec<BootRecord>,
    /// 历史均值（毫秒）
    pub avg_ms: u64,
    /// 历史最快（毫秒）
    pub best_ms: u64,
    /// 历史最慢（毫秒）
    pub worst_ms: u64,
    /// 数据源是否可用（日志被关闭 / 无记录时为 false）
    pub available: bool,
    /// 不可用时的原因（人话）
    pub note: String,
}

/// 启动项来源。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum StartupSource {
    /// 注册表 Run / RunOnce 键
    RunKey,
    /// 启动文件夹（快捷方式）
    StartupFolder,
    /// 自动启动的 Windows 服务
    Service,
    /// 开机 / 登录触发的计划任务
    ScheduledTask,
}

impl StartupSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            StartupSource::RunKey => "run_key",
            StartupSource::StartupFolder => "startup_folder",
            StartupSource::Service => "service",
            StartupSource::ScheduledTask => "scheduled_task",
        }
    }
    /// 中文名，UI 分组头用
    pub fn label(&self) -> &'static str {
        match self {
            StartupSource::RunKey => "注册表启动项",
            StartupSource::StartupFolder => "启动文件夹",
            StartupSource::Service => "自动启动服务",
            StartupSource::ScheduledTask => "计划任务",
        }
    }
}

/// 启动项风险分级。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    /// 安全禁用 —— 绿色，禁用后不影响系统正常使用
    Safe,
    /// 谨慎 —— 黄色，可能影响某个功能，需要用户确认
    Caution,
    /// 不建议 —— 红色，禁用会破坏系统功能，UI 应默认不允许勾选
    Dangerous,
    /// 无法判定 —— 灰色，信息不足
    Unknown,
}

impl RiskLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            RiskLevel::Safe => "safe",
            RiskLevel::Caution => "caution",
            RiskLevel::Dangerous => "dangerous",
            RiskLevel::Unknown => "unknown",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            RiskLevel::Safe => "可安全禁用",
            RiskLevel::Caution => "谨慎处理",
            RiskLevel::Dangerous => "不建议禁用",
            RiskLevel::Unknown => "无法判定",
        }
    }
}

/// 知识库条目 —— 「这个启动项到底是干什么的」。
///
/// 内置库随程序打包（`core/knowledge.rs` 的 `include_str!`），用户可以在界面上
/// 逐条改写；改写结果落到 `data/knowledge/startup.json`，**只覆盖同名条目，不改内置库**。
/// 这样升级程序时内置库能跟着更新，而用户自己的补充不会丢。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Knowledge {
    /// 这是什么的简短标题，如「OneDrive 云同步客户端」
    pub title: String,
    /// 厂商 / 归属软件
    pub vendor: String,
    /// 综合描述：它做什么、属于谁、禁用后会怎样
    pub desc: String,
    /// 给用户的建议（可禁用 / 谨慎 / 建议保留）
    pub advice: String,
    /// 出处：`builtin`（内置库）或 `user`（用户自己写的）
    pub origin: String,
    /// 命中的那条匹配串。用户条目一律只有一条 key，所以前端可以直接拿它
    /// 调 `knowledge_remove` 把「恢复内置说明」做成一次点击，
    /// 不必让用户去设置页里对着列表找是哪一条。
    pub key: String,
    /// 是不是「只看名字猜出来的」兜底条目（`updater` / `helper` / `push`）。
    ///
    /// 前端拿它来区分「这条说明有实据」还是「只是名字像」——
    /// 决定要不要用它去改写风险档：兜底条目连自己都不敢下结论
    /// （建议被强制收敛成「谨慎处理」），当然也不该去改写别人已经算出来的结论。
    #[serde(default)]
    pub generic: bool,
}

/// 一个启动项。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StartupItem {
    /// 稳定 ID（来源 + 定位键的哈希），跨扫描不变，用于禁用与还原
    pub id: String,
    pub name: String,
    /// 启动命令行 / 服务路径 / 任务动作
    pub command: String,
    pub source: StartupSource,
    /// 精确定位：注册表路径 / 文件夹路径 / 服务名 / 任务 XML 路径
    pub location: String,
    /// 发布者（取自签名主体，无签名时为空）
    pub publisher: Option<String>,
    pub signature: SignatureStatus,
    /// 当前是否处于启用状态
    pub enabled: bool,
    /// 是否由**本程序**禁用的（从变更留痕里重建出来的条目）。
    ///
    /// 这类条目已经不在系统里了，只是被我们记录下来，让用户能在列表里
    /// 重新看到它并一键启用 —— 否则「禁用」等于「从列表消失」，用户找不到还原入口。
    pub disabled_by_us: bool,
    /// 由老六禁用时所属的变更批次 ID（用于单条还原）。
    pub batch_id: Option<String>,
    /// 是否为「延迟自动启动」。**仅自动启动服务有意义**，其他来源恒为 false。
    ///
    /// 延迟启动 = 「想留但想更快」：服务本身保留，注册表 `DelayedAutostart=1`
    /// 让 SCM 在开机关键路径走完之后（约 2 分钟）再拉起它。
    /// 与「禁用」是两种用户意图，UI 必须分开提供。
    pub delayed: bool,
    pub risk: RiskLevel,
    /// 风险判定的中文理由
    pub risk_reason: String,
    /// 是否允许被禁用（`Dangerous` 为 false，UI 灰掉勾选框）
    pub can_disable: bool,
    /// 若能在开机归因里匹配到同名对象，则给出它拖慢的毫秒数
    pub impact_ms: Option<u64>,
    /// 签名是否尚未校验（快速扫描下为 true，UI 应显示「确认中…」）
    ///
    /// 存在这个字段的原因：`WinVerifyTrust` 必须哈希**整个文件**，
    /// 实测启动项里 16 个待验文件合计 601 MB（两个 Electron 应用各占 ~200 MB），
    /// 单这一项就要 0.6–2 s。首屏不该为它等待。
    pub signature_pending: bool,
    /// 知识库说明（内置库 + 用户补充合并后的结果）。没命中就是 `None`。
    pub knowledge: Option<Knowledge>,
    /// 用户在「白名单」里标记为要保留
    pub in_whitelist: bool,
    /// 用户在「黑名单」里标记为要禁用（这类条目免二次确认直接处理）
    pub in_blacklist: bool,
    /// 风险理由的来源前缀（如「开机触发；」「延迟启动…；」）。
    ///
    /// 单独存而不直接拼进 `risk_reason`：异步补签之后要重算理由，
    /// 届时需要拿得到这个前缀，否则会把「登录触发」这类来源信息丢掉。
    /// 跳过序列化 —— 它只服务于后端重算，不该出现在前端契约里。
    #[serde(skip)]
    pub reason_prefix: String,
}

/// 构造一个「只关心名称与命令行」的启动项，供各模块的单元测试使用。
///
/// 启动项有 17 个字段，每个测试模块各写一遍构造器必然漂移 ——
/// 新增字段时只改这一处，所有测试都跟着更新。
#[cfg(test)]
impl StartupItem {
    pub fn blank_for_test(name: &str, command: &str) -> Self {
        Self {
            id: format!("test::{name}"),
            name: name.to_string(),
            command: command.to_string(),
            source: StartupSource::RunKey,
            location: r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run".into(),
            publisher: None,
            signature: SignatureStatus::Unsigned,
            enabled: true,
            disabled_by_us: false,
            batch_id: None,
            delayed: false,
            risk: RiskLevel::Safe,
            risk_reason: "测试".into(),
            can_disable: true,
            impact_ms: None,
            signature_pending: false,
            knowledge: None,
            in_whitelist: false,
            in_blacklist: false,
            reason_prefix: String::new(),
        }
    }
}

/// 一次启动项变更的记录（用于还原与留痕）。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StartupChange {
    pub item_id: String,
    pub name: String,
    pub source: StartupSource,
    pub action: String,
    /// 原状态的可还原载荷（注册表原值 / 原文件路径 / 原 StartType / 原 Enabled）
    pub undo: String,
    pub ok: bool,
    pub error: Option<String>,
    /// 变更目标的精确定位串。
    ///
    /// 加这个字段是为了「禁用后仍能在列表里看到」：批次留痕要能独立重建出
    /// 一个完整的 `StartupItem`，否则列表里只剩一个名字，点「启用」都不知道该动哪儿。
    /// `serde(default)` 让旧批次文件仍然能读。
    #[serde(default)]
    pub location: String,
    /// 变更目标的启动命令行（同上，用于重建条目）。旧批次为空串。
    #[serde(default)]
    pub command: String,
}

/// 启动项操作结果。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct StartupResult {
    /// 变更批次 ID，用于整体还原
    pub batch_id: String,
    pub timestamp: u64,
    pub changes: Vec<StartupChange>,
    /// 是否为演练（未对系统做任何改动）
    pub dry_run: bool,
    pub elapsed_ms: u64,
}

/// 启动项扫描结果。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct StartupScan {
    pub items: Vec<StartupItem>,
    /// 各类来源的条目数：run_key / startup_folder / service / scheduled_task
    pub counts: Vec<(String, usize)>,
    /// 高风险但用户可能想禁用的条目数（UI 概览用）
    pub disable_candidates: usize,
    pub elapsed_ms: u64,
    /// 分阶段耗时 `(阶段名, 毫秒)`，用于性能诊断
    pub stage_ms: Vec<(String, u64)>,
    /// 是否需要管理员权限才能完整枚举/控制
    pub needs_admin: bool,
    /// 仍待校验签名的条目数（> 0 时 UI 应继续调一次 `startup_verify`）
    pub pending_signatures: usize,
}

