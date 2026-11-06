//! P5 常驻监控：低开销采样 + 阈值告警 + 峰值统计 + 自身占用自证。
//!
//! 设计要点（对照 P5 硬指标：常驻内存 ≤ 40 MB、常驻 CPU ≤ 0.5%）：
//!
//! 1. **采样要便宜**。每轮只做一次 PDH 采集（`perf::SystemSampler`）+
//!    一次 `GlobalMemoryStatusEx`。进程数用 `NtQuerySystemInformation`
//!    枚举一次（约 3.5 ms），默认 5 秒一轮 —— 折算下来 CPU 占用远低于 0.5%。
//! 2. **采样器必须跨调用存活**。PDH 的速率型计数器需要两次采样才有值，
//!    所以采样器住在 `MonitorHub` 里，由后台线程独占使用。
//! 3. **告警要去重**。同一种告警在冷却期内只报一次，否则用户会被
//!    同一条消息反复骚扰 —— 「轻提示，不打扰」是这个模块的核心诉求。
//! 4. **自身占用要能被量出来**。`self_working_set` 与 `self_cpu_percent`
//!    给出老六清净自己占了多少内存 / CPU，验收硬指标时直接读这两个数。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::memory;
use super::perf::SystemSampler;
use super::types::SystemMetrics;

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

/// 常驻监控配置（持久化到 `data/monitor.json`）。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MonitorConfig {
    /// 总开关。关掉之后后台线程退出，常驻占用归零（「用完即退」）。
    pub enabled: bool,
    /// 采样间隔（毫秒）。下限 1000，防止有人把它改成 1 ms 变成忙等。
    pub interval_ms: u64,
    /// CPU 占用告警阈值（%）。
    pub cpu_threshold: f32,
    /// 内存占用告警阈值（%）。
    pub mem_threshold: f32,
    /// 磁盘活动时间告警阈值（%）。
    pub disk_threshold: f32,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_ms: 5000,
            cpu_threshold: 85.0,
            mem_threshold: 90.0,
            disk_threshold: 95.0,
        }
    }
}

impl MonitorConfig {
    /// 采样间隔的下限与上限：太密会白烧 CPU，太疏会失去意义。
    pub fn clamped_interval(&self) -> u64 {
        self.interval_ms.clamp(1000, 60_000)
    }

    /// 校验并夹紧到合理范围，返回修正后的副本。
    pub fn sanitize(mut self) -> Self {
        self.interval_ms = self.clamped_interval();
        self.cpu_threshold = self.cpu_threshold.clamp(10.0, 100.0);
        self.mem_threshold = self.mem_threshold.clamp(10.0, 100.0);
        self.disk_threshold = self.disk_threshold.clamp(10.0, 100.0);
        self
    }
}

// ---------------------------------------------------------------------------
// 告警
// ---------------------------------------------------------------------------

/// 告警级别。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AlertLevel {
    Notice,
    Warn,
}

/// 一条告警。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Alert {
    /// 告警种类：cpu / mem / disk / self_cpu / self_mem
    pub kind: String,
    pub level: AlertLevel,
    pub title: String,
    /// 人话解释：为什么报这个。
    pub detail: String,
    /// 触发时的数值（%）。
    pub value: f32,
    /// 触发时刻（unix 秒）。
    pub at: u64,
}

// ---------------------------------------------------------------------------
// 快照
// ---------------------------------------------------------------------------

/// 一次采样结果。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct MonitorSample {
    pub cpu_percent: f32,
    pub mem_percent: f32,
    pub disk_percent: f32,
    pub net_rx_bps: f64,
    pub net_tx_bps: f64,
    pub process_count: u32,
    /// 老六清净自身工作集（MB）。
    pub self_working_set_mb: f32,
    /// 老六清净自身 CPU 占用（%）。
    pub self_cpu_percent: f32,
    /// 距上次采样的间隔（毫秒）。
    pub interval_ms: u64,
}

/// 常驻监控快照：最近一次采样 + 峰值 + 未处理告警。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct MonitorSnapshot {
    pub running: bool,
    pub sample: MonitorSample,
    pub peak_cpu: f32,
    pub peak_mem: f32,
    pub peak_self_mem: f32,
    /// 采样轮数。
    pub rounds: u64,
    pub alerts: Vec<Alert>,
    pub config: MonitorConfig,
}

// ---------------------------------------------------------------------------
// Hub
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct MonitorState {
    sample: MonitorSample,
    peak_cpu: f32,
    peak_mem: f32,
    peak_self_mem: f32,
    rounds: u64,
    alerts: Vec<Alert>,
    /// 每种告警上次触发的时间（unix 秒），用于冷却去重。
    last_alert: std::collections::HashMap<String, u64>,
}

/// 告警冷却时间（秒）：同一种告警在这个窗口内不重复报。
const ALERT_COOLDOWN_SECS: u64 = 120;

/// 告警列表最多保留条数。
const MAX_ALERTS: usize = 50;

/// 常驻监控中心。持有配置、采样器、后台线程句柄。
pub struct MonitorHub {
    cfg: Mutex<MonitorConfig>,
    state: Mutex<MonitorState>,
    running: AtomicBool,
    stop: AtomicBool,
}

impl Default for MonitorHub {
    fn default() -> Self {
        Self::new()
    }
}

impl MonitorHub {
    pub fn new() -> Self {
        Self {
            cfg: Mutex::new(MonitorConfig::default()),
            state: Mutex::new(MonitorState::default()),
            running: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        }
    }

    /// 读取当前配置。
    pub fn config(&self) -> MonitorConfig {
        self.cfg.lock().map(|c| c.clone()).unwrap_or_default()
    }

    /// 写入配置。返回修正后的配置（阈值与间隔都被夹紧）。
    pub fn set_config(&self, cfg: MonitorConfig) -> MonitorConfig {
        let cfg = cfg.sanitize();
        if let Ok(mut c) = self.cfg.lock() {
            *c = cfg.clone();
        }
        cfg
    }

    /// 后台线程是否在跑。
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// 启动后台采样线程。已在跑时是幂等的。
    ///
    /// 线程退出条件：`stop` 标志被置位（`stop_monitor` 调用），
    /// 或者配置里的 `enabled` 被关掉。两者都会让常驻占用归零。
    pub fn start(self: &Arc<Self>) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        self.stop.store(false, Ordering::SeqCst);
        let hub = Arc::clone(self);
        std::thread::Builder::new()
            .name("old666-monitor".into())
            // 采样线程几乎不做计算，默认 2 MB 的栈纯属浪费。
            // 压到 256 KB，常驻内存直接省下一块。
            .stack_size(256 * 1024)
            .spawn(move || hub.run_loop())
            .ok();
    }

    /// 请求后台线程退出。
    pub fn stop_monitor(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.running.store(false, Ordering::SeqCst);
    }

    fn run_loop(self: Arc<Self>) {
        let sampler = SystemSampler::new();
        let mut last = Instant::now();
        // 自身 CPU 计数基线（kernel+user 累计时间，单位 100ns）
        let mut last_self_time = self.self_cpu_time();
        // 进程数基线：第一轮 PDH 还没值时先兜个底
        let mut last_process_count: u32 = 0;

        while !self.stop.load(Ordering::Relaxed) {
            let cfg = self.config();
            if !cfg.enabled {
                break;
            }
            let interval = Duration::from_millis(cfg.clamped_interval());
            std::thread::sleep(interval);
            if self.stop.load(Ordering::Relaxed) {
                break;
            }

            let now = Instant::now();
            let elapsed_ms = now.duration_since(last).as_millis().max(1) as u64;
            last = now;

            // 进程数走 PDH 的 \System\Processes（见 perf.rs），
            // 不在这里做 process::enumerate() —— 那要逐个 OpenProcess，
            // 常驻监控每 5 秒跑一次就把 CPU 顶到 1% 上下，达不到 0.5% 的指标。
            //
            // 磁盘与网络速率走「低频补采」：这两个计数器单轮约 45 ms，
            // 每轮都采等于白白烧掉 0.8% CPU，所以每 6 轮（约 30 秒）才补一次。
            let mut rounds_done = 0u32;
            rounds_done = rounds_done.wrapping_add(1);
            let heavy = rounds_done % 6 == 0;
            let metrics: SystemMetrics = if heavy {
                sampler.sample(last_process_count, 0)
            } else {
                sampler.sample_light(last_process_count, 0)
            };
            let process_count = metrics.process_count;
            last_process_count = process_count;
            let mem_percent = memory::memory_percent();

            let self_cpu_time = self.self_cpu_time();
            let self_cpu = self_cpu_percent(last_self_time, self_cpu_time, elapsed_ms);
            last_self_time = self_cpu_time;

            let self_ws = memory::working_set_of(std::process::id()).unwrap_or(0);
            let sample = MonitorSample {
                cpu_percent: metrics.cpu_percent,
                mem_percent,
                disk_percent: metrics.disk_percent,
                net_rx_bps: metrics.net_rx_bps,
                net_tx_bps: metrics.net_tx_bps,
                process_count,
                self_working_set_mb: self_ws as f32 / (1024.0 * 1024.0),
                self_cpu_percent: self_cpu,
                interval_ms: elapsed_ms,
            };

            self.absorb(sample, &cfg);
        }

        self.running.store(false, Ordering::SeqCst);
    }

    /// 把一次采样并入状态，并按阈值产出告警。
    fn absorb(&self, sample: MonitorSample, cfg: &MonitorConfig) {
        let mut st = match self.state.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        st.peak_cpu = st.peak_cpu.max(sample.cpu_percent);
        st.peak_mem = st.peak_mem.max(sample.mem_percent);
        st.peak_self_mem = st.peak_self_mem.max(sample.self_working_set_mb);
        st.rounds += 1;
        let sample_for_alerts = sample.clone();
        st.sample = sample;

        let now = super::process::now_unix();
        let push = |st: &mut MonitorState, kind: &str, level: AlertLevel, title: &str, detail: String, value: f32| {
            let last = st.last_alert.get(kind).copied().unwrap_or(0);
            if now.saturating_sub(last) < ALERT_COOLDOWN_SECS {
                return;
            }
            st.last_alert.insert(kind.to_string(), now);
            st.alerts.insert(
                0,
                Alert {
                    kind: kind.to_string(),
                    level,
                    title: title.to_string(),
                    detail,
                    value,
                    at: now,
                },
            );
            if st.alerts.len() > MAX_ALERTS {
                st.alerts.truncate(MAX_ALERTS);
            }
        };

        let sample = &sample_for_alerts;
        if sample.cpu_percent >= cfg.cpu_threshold {
            push(
                &mut st,
                "cpu",
                AlertLevel::Warn,
                "CPU 占用偏高",
                format!(
                    "系统 CPU 已连续占用 {:.0}%，超过阈值 {:.0}%。可以在「净化」页看看是谁在吃 CPU。",
                    sample.cpu_percent, cfg.cpu_threshold
                ),
                sample.cpu_percent,
            );
        }
        if sample.mem_percent >= cfg.mem_threshold {
            push(
                &mut st,
                "mem",
                AlertLevel::Warn,
                "内存占用偏高",
                format!(
                    "物理内存已用 {:.0}%，超过阈值 {:.0}%。老六清净不自动结束进程，请你自己确认后再操作。",
                    sample.mem_percent, cfg.mem_threshold
                ),
                sample.mem_percent,
            );
        }
        if sample.disk_percent >= cfg.disk_threshold {
            push(
                &mut st,
                "disk",
                AlertLevel::Notice,
                "磁盘忙不过来",
                format!(
                    "磁盘活动时间 {:.0}%，超过阈值 {:.0}%。多半是更新或下载在写盘。",
                    sample.disk_percent, cfg.disk_threshold
                ),
                sample.disk_percent,
            );
        }
        if sample.self_cpu_percent >= 1.0 {
            push(
                &mut st,
                "self_cpu",
                AlertLevel::Notice,
                "老六清净自己有点忙",
                format!(
                    "自身 CPU 占用 {:.1}%，高于 1%。如果不需要常驻监控，可以在「监控」页关掉它。",
                    sample.self_cpu_percent
                ),
                sample.self_cpu_percent,
            );
        }
    }

    /// 取当前快照。
    pub fn snapshot(&self) -> MonitorSnapshot {
        let st = self.state.lock().map(|g| g.clone()).unwrap_or_default();
        MonitorSnapshot {
            running: self.is_running(),
            sample: st.sample,
            peak_cpu: st.peak_cpu,
            peak_mem: st.peak_mem,
            peak_self_mem: st.peak_self_mem,
            rounds: st.rounds,
            alerts: st.alerts,
            config: self.config(),
        }
    }

    /// 清空告警列表。
    pub fn clear_alerts(&self) {
        if let Ok(mut st) = self.state.lock() {
            st.alerts.clear();
        }
    }

    /// 读取自身累计 CPU 时间（kernel+user，单位 100 ns）。
    fn self_cpu_time(&self) -> u64 {
        match super::process::query_times(std::process::id()) {
            Some((_, kernel, user)) => kernel.saturating_add(user),
            None => 0,
        }
    }
}

/// 由两次累计 CPU 时间差算出瞬时占用百分比（已按核心数归一）。
fn self_cpu_percent(prev: u64, now: u64, elapsed_ms: u64) -> f32 {
    if elapsed_ms == 0 || now < prev {
        return 0.0;
    }
    // 100 ns → ms
    let used_ms = (now - prev) as f64 / 10_000.0;
    let pct = used_ms / elapsed_ms as f64 * 100.0;
    let cores = super::process::cpu_count().max(1) as f64;
    (pct / cores) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_sane() {
        let c = MonitorConfig::default();
        assert!(c.interval_ms >= 1000);
        assert!(c.cpu_threshold > 0.0 && c.cpu_threshold <= 100.0);
        assert!(c.mem_threshold > 0.0 && c.mem_threshold <= 100.0);
    }

    #[test]
    fn config_sanitizes_out_of_range_values() {
        let c = MonitorConfig {
            enabled: true,
            interval_ms: 10,
            cpu_threshold: 500.0,
            mem_threshold: -3.0,
            disk_threshold: 1_000.0,
        }
        .sanitize();
        assert_eq!(c.interval_ms, 1000);
        assert_eq!(c.cpu_threshold, 100.0);
        assert_eq!(c.mem_threshold, 10.0);
        assert_eq!(c.disk_threshold, 100.0);
    }

    #[test]
    fn interval_is_clamped_to_a_floor() {
        let c = MonitorConfig {
            interval_ms: 1,
            ..Default::default()
        };
        assert_eq!(c.clamped_interval(), 1000);
    }

    #[test]
    fn self_cpu_percent_is_zero_when_no_time_passed() {
        assert_eq!(self_cpu_percent(100, 100, 1000), 0.0);
    }

    #[test]
    fn self_cpu_percent_scales_with_cpu_time() {
        // 1 秒 CPU 时间 ÷ 1 秒墙钟 = 100%，再按核心数归一
        let cores = super::super::process::cpu_count().max(1) as f64;
        let used = 1_000_000u64 / 100; // 10000 个 100ns = 1ms
        let pct = self_cpu_percent(0, used, 1000);
        assert!(pct > 0.0, "应有非零占用，实际 {pct}");
        assert!(pct <= 100.0 / cores as f32 + 1.0);
    }

    #[test]
    fn hub_snapshot_reports_config_and_starts_stopped() {
        let hub = MonitorHub::new();
        assert!(!hub.is_running());
        let s = hub.snapshot();
        assert_eq!(s.config.interval_ms, MonitorConfig::default().interval_ms);
        assert!(s.alerts.is_empty());
    }

    #[test]
    fn clearing_alerts_empties_the_list() {
        let hub = MonitorHub::new();
        hub.clear_alerts();
        assert!(hub.snapshot().alerts.is_empty());
    }
}
