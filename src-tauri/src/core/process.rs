//! 进程枚举与基础信息采集。
//!
//! 主路径用 `NtQuerySystemInformation(SystemProcessInformation)` —— **一次系统调用**
//! 拿到全部进程的 PID/PPID/名称/内存/线程/句柄/IO/CPU 时间，这是达成
//! 「1000 进程 ≤ 200ms」的关键。路径与签名等昂贵信息按需二次查询。

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;

use windows::Win32::Foundation::{BOOL, CloseHandle, FILETIME, HANDLE, HWND, LPARAM, MAX_PATH};
use windows::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowThreadProcessId, IsWindowVisible,
};

use super::blacklist;
use super::types::CollectStats;

// ---------------------------------------------------------------------------
// ntdll 直调：NtQuerySystemInformation
// ---------------------------------------------------------------------------

#[link(name = "ntdll")]
extern "system" {
    fn NtQuerySystemInformation(
        system_information_class: u32,
        system_information: *mut c_void,
        system_information_length: u32,
        return_length: *mut u32,
    ) -> i32;
}

const SYSTEM_PROCESS_INFORMATION_CLASS: u32 = 5;
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004u32 as i32;
const STATUS_BUFFER_TOO_SMALL: i32 = 0xC000_0023u32 as i32;

#[repr(C)]
#[derive(Clone, Copy)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

/// 与 Win32 `SYSTEM_PROCESS_INFORMATION` 二进制兼容。
#[repr(C)]
struct SystemProcessInformation {
    next_entry_offset: u32,
    number_of_threads: u32,
    working_set_private_size: i64,
    hard_fault_count: u32,
    number_of_threads_high_watermark: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    image_name: UnicodeString,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
    handle_count: u32,
    session_id: u32,
    unique_process_key: usize,
    peak_virtual_size: usize,
    virtual_size: usize,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_page_count: usize,
    read_operation_count: i64,
    write_operation_count: i64,
    other_operation_count: i64,
    read_transfer_count: i64,
    write_transfer_count: i64,
    other_transfer_count: i64,
}

// ---------------------------------------------------------------------------
// 原始进程记录
// ---------------------------------------------------------------------------

/// 一次枚举拿到的原始进程数据（未做任何判定）。
///
/// `cpu_percent` 与 `exe_path` 不在 `NtQuerySystemInformation` 的返回里，
/// 由 `Sampler` 二次填充：CPU 靠两次采样的时间差算，路径按需查询。
#[derive(Clone, Debug, Default)]
pub struct RawProc {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    /// 可执行文件完整路径（权限不足时为 None）
    pub exe_path: Option<String>,
    /// CPU 占用百分比（已按逻辑核心数归一，0–100）
    pub cpu_percent: f32,
    pub working_set: u64,
    pub private_bytes: u64,
    pub thread_count: u32,
    pub handle_count: u32,
    /// FILETIME（100ns since 1601）
    pub create_time: u64,
    /// 累计内核态时间（100ns）
    pub kernel_time: u64,
    /// 累计用户态时间（100ns）
    pub user_time: u64,
    pub io_read: u64,
    pub io_write: u64,
    pub session_id: u32,
}

impl RawProc {
    /// 进程已运行秒数。
    pub fn uptime_secs(&self) -> u64 {
        let created = filetime_to_unix(self.create_time);
        if created == 0 {
            return 0;
        }
        now_unix().saturating_sub(created)
    }

    /// 可执行文件路径（引用形式，便于 `Option<&str>` 传递）。
    pub fn exe_path(&self) -> Option<String> {
        self.exe_path.clone()
    }

    /// 累计 CPU 时间（100ns）。
    pub fn total_cpu_time(&self) -> u64 {
        self.kernel_time.saturating_add(self.user_time)
    }
}

/// 枚举全部进程。返回 `(进程列表, 耗时毫秒)`。
pub fn enumerate() -> (Vec<RawProc>, f64) {
    let t0 = std::time::Instant::now();

    // 先探一次拿所需缓冲区大小，再按 1.5 倍余量重试（进程随时在变）
    let mut len: u32 = 0;
    let _probe = unsafe {
        NtQuerySystemInformation(
            SYSTEM_PROCESS_INFORMATION_CLASS,
            std::ptr::null_mut(),
            0,
            &mut len,
        )
    };

    let mut buf_len = if len > 0 { (len as usize) * 3 / 2 } else { 512 * 1024 };
    let mut buf: Vec<u8>;
    let mut attempts = 0;

    loop {
        buf = vec![0u8; buf_len];
        let mut ret_len: u32 = 0;
        let status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_PROCESS_INFORMATION_CLASS,
                buf.as_mut_ptr() as *mut c_void,
                buf_len as u32,
                &mut ret_len,
            )
        };

        if status == 0 {
            break;
        }
        if status == STATUS_INFO_LENGTH_MISMATCH || status == STATUS_BUFFER_TOO_SMALL {
            buf_len = if ret_len > 0 {
                (ret_len as usize) * 3 / 2
            } else {
                buf_len * 2
            };
            attempts += 1;
            if attempts > 8 {
                return (Vec::new(), t0.elapsed().as_secs_f64() * 1000.0);
            }
            continue;
        }
        // 其它错误码：放弃，返回空表（上层降级到 Toolhelp 路径）
        let _ = status;
        return (Vec::new(), t0.elapsed().as_secs_f64() * 1000.0);
    }

    let mut out = Vec::with_capacity(512);
    let base = buf.as_ptr() as usize;
    let mut offset = 0usize;

    loop {
        if offset + std::mem::size_of::<SystemProcessInformation>() > buf_len {
            break;
        }
        let entry = unsafe { &*(base.wrapping_add(offset) as *const SystemProcessInformation) };

        let name = if !entry.image_name.buffer.is_null() && entry.image_name.length > 0 {
            let n = (entry.image_name.length / 2) as usize;
            let slice = unsafe { std::slice::from_raw_parts(entry.image_name.buffer, n) };
            String::from_utf16_lossy(slice)
        } else {
            String::new()
        };

        out.push(RawProc {
            pid: entry.unique_process_id as u32,
            ppid: entry.inherited_from_unique_process_id as u32,
            name,
            exe_path: None,
            cpu_percent: 0.0,
            working_set: entry.working_set_size as u64,
            private_bytes: entry.private_page_count as u64,
            thread_count: entry.number_of_threads,
            handle_count: entry.handle_count,
            create_time: entry.create_time as u64,
            kernel_time: entry.kernel_time as u64,
            user_time: entry.user_time as u64,
            io_read: entry.read_transfer_count as u64,
            io_write: entry.write_transfer_count as u64,
            session_id: entry.session_id,
        });

        if entry.next_entry_offset == 0 {
            break;
        }
        offset += entry.next_entry_offset as usize;
    }

    (out, t0.elapsed().as_secs_f64() * 1000.0)
}

// ---------------------------------------------------------------------------
// 路径查询
// ---------------------------------------------------------------------------

/// 查询进程完整可执行文件路径。失败返回 `None`（多为权限不足的系统进程）。
pub fn query_exe_path(pid: u32) -> Option<String> {
    if blacklist::is_protected_pid(pid) {
        return None;
    }
    unsafe {
        let handle: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; MAX_PATH as usize * 2];
        let mut size = buf.len() as u32;
        let res = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(handle);
        res.ok()?;
        Some(String::from_utf16_lossy(&buf[..size as usize]))
    }
}

/// 查询进程创建时间（FILETIME）与累计 CPU 时间（100ns）。
pub fn query_times(pid: u32) -> Option<(u64, u64, u64)> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut create = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let res = GetProcessTimes(handle, &mut create, &mut exit, &mut kernel, &mut user);
        let _ = CloseHandle(handle);
        res.ok()?;
        let ft = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
        Some((ft(create), ft(kernel), ft(user)))
    }
}

// ---------------------------------------------------------------------------
// 窗口 / 前台归属
// ---------------------------------------------------------------------------

/// 收集所有持有**可见顶层窗口**的进程 PID。
pub fn pids_with_visible_window() -> HashSet<u32> {
    let mut set: HashSet<u32> = HashSet::new();
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let set = &mut *(lparam.0 as *mut HashSet<u32>);
        if IsWindowVisible(hwnd).as_bool() {
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid != 0 {
                set.insert(pid);
            }
        }
        true.into()
    }
    unsafe {
        let _ = EnumWindows(
            Some(cb),
            LPARAM(&mut set as *mut _ as isize),
        );
    }
    set
}

/// 收集所有持有**任意顶层窗口**（含隐藏）的进程 PID。
///
/// 为什么这个比「可见窗口」重要：托盘应用、输入法、常驻助手、IDE
/// 都可能没有可见窗口，但它们一定会创建一个（隐藏的）顶层窗口来收消息。
/// 只看可见窗口，会把这类**用户正在依赖的程序**误判成「闲置后台」。
pub fn pids_with_any_window() -> HashSet<u32> {
    let mut set: HashSet<u32> = HashSet::new();
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let set = &mut *(lparam.0 as *mut HashSet<u32>);
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != 0 {
            set.insert(pid);
        }
        true.into()
    }
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut set as *mut _ as isize));
    }
    set
}

/// 计算「祖先链上存在窗口进程」的 PID 集合。
///
/// 这是**通用性最强**的一条保护规则：一个进程自己可能没有窗口，
/// 但它属于某个交互式应用的一部分 —— 典型如
/// `chrome.exe`(主) → `chrome.exe`(渲染)、`Code.exe` → `node.exe`、
/// `WorkBuddy.exe` → `bash.exe` / `sandbox-cli.exe`。
/// 只要祖先里有窗口进程，就说明它服务于一个用户正在用的程序，
/// 一律不得判为「可安全结束」。
///
/// `window_pids` 应传入 [`pids_with_any_window`] 的结果。
pub fn pids_with_window_ancestor(list: &[RawProc], window_pids: &HashSet<u32>) -> HashSet<u32> {
    /// 祖先链最大回溯层数 —— 防止环形 ppid 造成死循环。
    const MAX_DEPTH: usize = 16;

    let parent: HashMap<u32, u32> = list.iter().map(|p| (p.pid, p.ppid)).collect();
    let mut out: HashSet<u32> = HashSet::new();

    for p in list {
        let mut cur = p.ppid;
        let mut depth = 0;
        while cur != 0 && cur != p.pid && depth < MAX_DEPTH {
            if window_pids.contains(&cur) {
                out.insert(p.pid);
                break;
            }
            match parent.get(&cur) {
                Some(&next) if next != cur => cur = next,
                _ => break,
            }
            depth += 1;
        }
    }
    out
}

/// 当前前台进程 PID。
pub fn foreground_pid() -> u32 {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return 0;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid
    }
}

// ---------------------------------------------------------------------------
// 辅助换算
// ---------------------------------------------------------------------------

/// FILETIME（1601 起 100ns）→ Unix 秒。
pub fn filetime_to_unix(ft: u64) -> u64 {
    const EPOCH_DIFF: u64 = 116_444_736_000_000_000; // 1601→1970，单位 100ns
    if ft < EPOCH_DIFF {
        0
    } else {
        (ft - EPOCH_DIFF) / 10_000_000
    }
}

/// 当前 Unix 秒。
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 逻辑处理器数量（CPU 百分比归一用）。
pub fn cpu_count() -> u32 {
    unsafe {
        let mut info = SYSTEM_INFO::default();
        GetSystemInfo(&mut info);
        if info.dwNumberOfProcessors == 0 {
            1
        } else {
            info.dwNumberOfProcessors
        }
    }
}

/// 进程分组名（列表分组头用）。纯展示，不参与安全判定。
pub fn group_of(name: &str, exe: Option<&str>) -> String {
    let n = blacklist::normalize(name);
    if blacklist::is_denied_name(&n) {
        return "系统组件".into();
    }
    if let Some(p) = exe {
        let lp = p.to_ascii_lowercase();
        if lp.contains("\\windows\\") || lp.contains("\\program files") {
            return "后台服务".into();
        }
    }
    match n.as_str() {
        "chrome" | "msedge" | "firefox" | "iexplore" | "brave" | "opera" => "浏览器".into(),
        "wechat" | "qq" | "dingtalk" | "telegram" | "feishu" | "wecom" => "即时通讯".into(),
        "steam" | "epicgameslauncher" | "wegame" | "battle.net" | "origin" => "游戏平台".into(),
        _ => "其他应用".into(),
    }
}

/// 按 PID 建索引，便于 O(1) 查父进程。
pub fn index_by_pid(list: &[RawProc]) -> HashMap<u32, usize> {
    list.iter().enumerate().map(|(i, p)| (p.pid, i)).collect()
}

// ---------------------------------------------------------------------------
// 采样器：把「累计值」变成「瞬时值」
// ---------------------------------------------------------------------------

/// 有状态的进程采样器。
///
/// `NtQuerySystemInformation` 给的是**累计** CPU 时间与 IO 字节，
/// 要变成任务管理器那样的瞬时百分比，必须保存上一次采样做差。
/// 采样间隔由调用方控制（P1 要求 1–5s 自适应）。
pub struct Sampler {
    /// pid → (累计 CPU 时间 100ns, 采样时刻)
    prev_cpu: HashMap<u32, (u64, std::time::Instant)>,
    /// pid → (创建时间, 可执行路径) —— 路径查询昂贵，按 PID+创建时间缓存
    path_cache: HashMap<u32, (u64, Option<String>)>,
    cpu_count: u32,
    first_run: bool,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            prev_cpu: HashMap::with_capacity(512),
            path_cache: HashMap::with_capacity(512),
            cpu_count: cpu_count(),
            first_run: true,
        }
    }

    /// 采集一轮。
    ///
    /// * `with_paths` —— 是否查询可执行路径（首轮 / 需要签名判定时开）。
    ///   关掉可显著提速，用于高频刷新的中间帧。
    pub fn collect(&mut self, with_paths: bool) -> (Vec<RawProc>, CollectStats) {
        let t_start = std::time::Instant::now();

        let (mut list, enumerate_ms) = enumerate();
        let t_after_enum = std::time::Instant::now();

        // --- CPU 增量 ---
        let now = std::time::Instant::now();
        let mut seen: HashSet<u32> = HashSet::with_capacity(list.len());
        for p in list.iter_mut() {
            seen.insert(p.pid);
            let cur = p.total_cpu_time();
            if let Some((prev_cpu, prev_t)) = self.prev_cpu.get(&p.pid) {
                let wall_ns = now.duration_since(*prev_t).as_nanos() as f64;
                if wall_ns > 1.0 {
                    // delta 单位 100ns → 转成 ns 再比
                    let delta_ns = cur.saturating_sub(*prev_cpu) as f64 * 100.0;
                    let pct = delta_ns / wall_ns * 100.0 / self.cpu_count as f64;
                    p.cpu_percent = pct.clamp(0.0, 100.0 * self.cpu_count as f64) as f32;
                }
            }
            self.prev_cpu.insert(p.pid, (cur, now));
        }
        // 清理已退出进程，防止长跑内存增长
        self.prev_cpu.retain(|k, _| seen.contains(k));
        self.path_cache.retain(|k, _| seen.contains(k));

        // 首轮没有基线，全部按 0 处理（避免把累计值当成瞬时值放大）
        if self.first_run {
            for p in list.iter_mut() {
                p.cpu_percent = 0.0;
            }
            self.first_run = false;
        }

        // --- 路径（可选） ---
        if with_paths {
            for p in list.iter_mut() {
                if blacklist::is_protected_pid(p.pid) {
                    continue;
                }
                // 命中缓存且创建时间一致（防 PID 复用）
                if let Some((ct, path)) = self.path_cache.get(&p.pid) {
                    if *ct == p.create_time {
                        p.exe_path = path.clone();
                        continue;
                    }
                }
                let path = query_exe_path(p.pid);
                self.path_cache.insert(p.pid, (p.create_time, path.clone()));
                p.exe_path = path;
            }
        }

        let detail_ms = t_after_enum.elapsed().as_secs_f64() * 1000.0;
        let stats = CollectStats {
            process_count: list.len() as u32,
            enumerate_ms,
            detail_ms,
            total_ms: t_start.elapsed().as_secs_f64() * 1000.0,
        };
        (list, stats)
    }

    /// 本采样器的逻辑核心数（用于报告）。
    pub fn cpu_count(&self) -> u32 {
        self.cpu_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_epoch() {
        assert_eq!(filetime_to_unix(116_444_736_000_000_000), 0);
        assert_eq!(filetime_to_unix(0), 0);
        // 1601+1s
        assert_eq!(filetime_to_unix(116_444_736_000_000_000 + 10_000_000), 1);
    }

    #[test]
    fn enumerate_returns_self() {
        let (list, ms) = enumerate();
        assert!(!list.is_empty(), "至少应枚举出当前进程");
        let me = std::process::id();
        assert!(list.iter().any(|p| p.pid == me), "应包含自身 PID {me}");
        assert!(ms >= 0.0);
    }

    #[test]
    fn cpu_count_positive() {
        assert!(cpu_count() >= 1);
    }

    #[test]
    fn group_maps_system() {
        assert_eq!(group_of("svchost.exe", None), "系统组件");
        assert_eq!(group_of("chrome.exe", None), "浏览器");
    }
}
