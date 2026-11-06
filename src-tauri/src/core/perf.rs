//! 系统级实时指标采集（PDH 性能计数器）。
//!
//! P0 验证点：**PDH 能否稳定拿到 CPU / 内存 / 磁盘 / 网络四类指标**。
//!
//! 实现要点：
//! - 计数器一律用 `PdhAddEnglishCounterW`（英文名，与系统语言无关），
//!   否则中文系统上 `\Processor Information(_Total)\% Processor Time` 会添加失败。
//! - 速率型计数器需要**两次采样**才有值，因此 `new()` 时先 `collect()` 一次。
//! - 网络是多实例计数器（每个网卡一个实例），用 `PdhGetFormattedCounterArrayW` 求和。
//! - 任何一步失败都**降级为 0**，绝不让采集失败拖垮整个 UI。
//!
//! windows-rs 0.58 里 PDH 句柄就是裸 `isize`（没有 `PDH_HQUERY` 类型别名），
//! 无效句柄用 0 表示。

use windows::core::PCWSTR;
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhGetFormattedCounterValue, PdhOpenQueryW, PDH_FMT_COUNTERVALUE,
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

use super::types::SystemMetrics;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 无效 PDH 句柄。
const INVALID: isize = 0;

/// 系统指标采样器。持有 PDH 查询句柄，需长期复用（每次采样后计数器才连续）。
pub struct SystemSampler {
    query: isize,
    cpu: isize,
    disk_read: isize,
    disk_write: isize,
    disk_time: isize,
    net_rx: isize,
    net_tx: isize,
    processes: isize,
    ok: bool,
}

// PDH 句柄是无状态的 OS 句柄；调用侧统一用 Mutex 串行化，这里显式声明契约。
unsafe impl Send for SystemSampler {}

impl SystemSampler {
    pub fn new() -> Self {
        unsafe {
            let mut query: isize = 0;
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 || query == INVALID {
                return Self::dead();
            }

            let add = |q: isize, path: &str| -> isize {
                let mut c: isize = 0;
                let w = wide(path);
                let rc = PdhAddEnglishCounterW(q, PCWSTR(w.as_ptr()), 0, &mut c);
                if rc != 0 {
                    INVALID
                } else {
                    c
                }
            };

            let cpu = add(query, r"\Processor Information(_Total)\% Processor Time");
            let disk_read = add(query, r"\PhysicalDisk(_Total)\Disk Read Bytes/sec");
            let disk_write = add(query, r"\PhysicalDisk(_Total)\Disk Write Bytes/sec");
            let disk_time = add(query, r"\PhysicalDisk(_Total)\% Disk Time");
            let net_rx = add(query, r"\Network Interface(*)\Bytes Received/sec");
            let net_tx = add(query, r"\Network Interface(*)\Bytes Sent/sec");
            // P5 常驻监控：进程数走 PDH 的 \System\Processes，
            // 不用逐个 OpenProcess —— 那一步会把常驻 CPU 顶到 1% 上下。
            let processes = add(query, r"\System\Processes");

            let s = Self {
                query,
                cpu,
                disk_read,
                disk_write,
                disk_time,
                net_rx,
                net_tx,
                processes,
                ok: true,
            };
            // 速率计数器需要基线采样
            let _ = PdhCollectQueryData(s.query);
            s
        }
    }

    fn dead() -> Self {
        Self {
            query: INVALID,
            cpu: INVALID,
            disk_read: INVALID,
            disk_write: INVALID,
            disk_time: INVALID,
            net_rx: INVALID,
            net_tx: INVALID,
            processes: INVALID,
            ok: false,
        }
    }

    /// PDH 是否可用（P0 报告要如实体现）。
    pub fn is_healthy(&self) -> bool {
        self.ok
    }

    /// 轻量采样：**只**取 CPU、内存、进程数。
    ///
    /// P5 常驻监控用它，而不是 `sample()`。
    /// 原因：`% Disk Time` 与 `Network Interface(*)` 这两个计数器
    /// 单轮要 40 ms 上下，五秒一轮就是 0.8% CPU —— 达不到 0.5% 的指标。
    /// 监控页要的是「系统忙不忙」，磁盘与网络速率偶尔看一眼就够，
    /// 所以那条路径改用低频采样。
    pub fn sample_light(&self, process_count: u32, thread_count: u32) -> SystemMetrics {
        let mut m = SystemMetrics {
            process_count,
            thread_count,
            ..Default::default()
        };

        unsafe {
            let mut ms = MEMORYSTATUSEX {
                dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
                ..Default::default()
            };
            if GlobalMemoryStatusEx(&mut ms).is_ok() {
                m.mem_total = ms.ullTotalPhys;
                m.mem_used = ms.ullTotalPhys.saturating_sub(ms.ullAvailPhys);
            }
        }

        if !self.ok {
            return m;
        }

        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return m;
            }
            m.cpu_percent = read_single(self.cpu).unwrap_or(0.0).clamp(0.0, 100.0) as f32;
            if let Some(v) = read_single(self.processes) {
                if v.is_finite() && v >= 0.0 {
                    m.process_count = v as u32;
                }
            }
        }

        m
    }

    /// 采集一次系统指标。
    pub fn sample(&self, process_count: u32, thread_count: u32) -> SystemMetrics {
        let mut m = SystemMetrics {
            process_count,
            thread_count,
            ..Default::default()
        };

        // 内存：用 GlobalMemoryStatusEx，比 PDH 精确且免基线
        unsafe {
            let mut ms = MEMORYSTATUSEX {
                dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
                ..Default::default()
            };
            if GlobalMemoryStatusEx(&mut ms).is_ok() {
                m.mem_total = ms.ullTotalPhys;
                m.mem_used = ms.ullTotalPhys.saturating_sub(ms.ullAvailPhys);
            }
        }

        if !self.ok {
            return m;
        }

        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return m;
            }
            m.cpu_percent = read_single(self.cpu).unwrap_or(0.0).clamp(0.0, 100.0) as f32;
            m.disk_percent = read_single(self.disk_time).unwrap_or(0.0).clamp(0.0, 100.0) as f32;
            m.disk_read_bps = read_single(self.disk_read).unwrap_or(0.0);
            m.disk_write_bps = read_single(self.disk_write).unwrap_or(0.0);
            m.net_rx_bps = read_array_sum(self.net_rx).unwrap_or(0.0);
            m.net_tx_bps = read_array_sum(self.net_tx).unwrap_or(0.0);
            // 进程数：优先用 PDH 计数器（几乎零开销）；
            // 拿不到时保留调用方传进来的值。
            if let Some(v) = read_single(self.processes) {
                if v.is_finite() && v >= 0.0 {
                    m.process_count = v as u32;
                }
            }
        }

        m
    }
}

impl Default for SystemSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SystemSampler {
    fn drop(&mut self) {
        if self.ok {
            unsafe {
                let _ = PdhCloseQuery(self.query);
            }
        }
    }
}

/// 读取单实例计数器当前值。
unsafe fn read_single(counter: isize) -> Option<f64> {
    if counter == INVALID {
        return None;
    }
    let mut val = PDH_FMT_COUNTERVALUE::default();
    if PdhGetFormattedCounterValue(counter, PDH_FMT_DOUBLE, None, &mut val) != 0 {
        return None;
    }
    let v = val.Anonymous.doubleValue;
    if v.is_finite() {
        Some(v)
    } else {
        None
    }
}

/// 读取多实例计数器并求和（网络：多网卡累加）。
unsafe fn read_array_sum(counter: isize) -> Option<f64> {
    if counter == INVALID {
        return None;
    }
    let mut size: u32 = 0;
    let mut count: u32 = 0;

    // 第一次调用取所需缓冲区大小
    let rc = PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, None);
    if rc != 0 || size == 0 || count == 0 {
        return Some(0.0);
    }

    // 按 u64 对齐分配，保证 PDH_FMT_COUNTERVALUE_ITEM_W 布局正确
    let words = (size as usize).div_ceil(8);
    let mut buf: Vec<u64> = vec![0; words];
    let mut size2 = size;
    let mut count2 = count;
    let rc = PdhGetFormattedCounterArrayW(
        counter,
        PDH_FMT_DOUBLE,
        &mut size2,
        &mut count2,
        Some(buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W),
    );
    if rc != 0 {
        return Some(0.0);
    }

    let items = std::slice::from_raw_parts(
        buf.as_ptr() as *const PDH_FMT_COUNTERVALUE_ITEM_W,
        count2 as usize,
    );
    let mut sum = 0.0f64;
    for it in items {
        let v = it.FmtValue.Anonymous.doubleValue;
        // 无效值（网卡刚下线）以非有限值返回，跳过
        if v.is_finite() && v > 0.0 {
            sum += v;
        }
    }
    Some(sum)
}

/// 格式化字节数为人类可读。
pub fn human_bytes(b: u64) -> String {
    const U: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", b, U[0])
    } else {
        format!("{:.1} {}", v, U[i])
    }
}

/// 格式化速率。
pub fn human_rate(bps: f64) -> String {
    format!("{}/s", human_bytes(bps.max(0.0) as u64))
}

/// 格式化毫秒数为人类可读（开机分段用，>1s 时切换成「秒」）。
///
/// 开机分段动辄几千毫秒，直接印 `9537 ms` 用户没有体感，
/// 统一成 `9.5 s` 更直观；小于 1 秒的仍按毫秒显示，避免出现 `0.3 s` 这种噪声。
pub fn human_ms(ms: u64) -> String {
    if ms >= 1000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        format!("{ms} ms")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(1024 * 1024 * 3), "3.0 MB");
    }

    #[test]
    fn human_ms_switches_unit_at_one_second() {
        assert_eq!(human_ms(0), "0 ms");
        assert_eq!(human_ms(999), "999 ms");
        assert_eq!(human_ms(1000), "1.0 s");
        assert_eq!(human_ms(9537), "9.5 s");
        assert_eq!(human_ms(15478), "15.5 s");
    }

    #[test]
    fn sampler_constructs() {
        let s = SystemSampler::new();
        let m = s.sample(100, 1000);
        // 内存信息来自 GlobalMemoryStatusEx，任何 Windows 上都应有值
        assert!(m.mem_total > 0, "物理内存总量应 > 0");
    }
}
