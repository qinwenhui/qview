//! Cross-platform process metrics: peak RSS and process CPU time.
//!
//! Windows: `GetProcessMemoryInfo` / `GetProcessTimes` (windows-sys).
//! Linux:   `/proc/self/status` (VmHWM) and `/proc/self/stat` (utime+stime).
//! macOS:   `proc_pidinfo(PROC_PIDTASKINFO)` (RSS) + `task_info(MACH_TASK_BASIC_INFO)` (CPU).

/// Current resident set size of this process in KiB (0 = unavailable).
pub fn current_rss_kb() -> u64 {
    #[cfg(windows)]
    {
        return windows_current_rss_kb();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return linux_current_rss_kb();
    }
    #[cfg(target_os = "macos")]
    {
        return macos_current_rss_kb();
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")), target_os = "macos")))]
    {
        0
    }
}

/// Total CPU time consumed by this process (user + kernel) in ns.
pub fn process_cpu_ns() -> u64 {
    #[cfg(windows)]
    {
        return windows_cpu_ns();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return linux_cpu_ns();
    }
    #[cfg(target_os = "macos")]
    {
        return macos_cpu_ns();
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")), target_os = "macos")))]
    {
        0
    }
}

#[cfg(windows)]
fn windows_current_rss_kb() -> u64 {
    windows_pmc().map(|pmc| (pmc.WorkingSetSize as u64) / 1024).unwrap_or(0)
}

#[cfg(windows)]
fn windows_pmc() -> Option<windows_sys::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS> {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let mut pmc: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        if GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut pmc,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ) != 0
        {
            Some(pmc)
        } else {
            None
        }
    }
}

#[cfg(windows)]
fn windows_cpu_ns() -> u64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    unsafe {
        let mut ct: FILETIME = std::mem::zeroed();
        let mut et: FILETIME = std::mem::zeroed();
        let mut kt: FILETIME = std::mem::zeroed();
        let mut ut: FILETIME = std::mem::zeroed();
        if GetProcessTimes(GetCurrentProcess(), &mut ct, &mut et, &mut kt, &mut ut) != 0 {
            let kern = ((kt.dwHighDateTime as u64) << 32) | kt.dwLowDateTime as u64;
            let user = ((ut.dwHighDateTime as u64) << 32) | ut.dwLowDateTime as u64;
            (kern + user) * 100 // 100 ns ticks → ns
        } else {
            0
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn linux_current_rss_kb() -> u64 {
    // VmRSS: current resident set size in kB.
    parse_proc_status("VmRSS")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn linux_cpu_ns() -> u64 {
    let s = match std::fs::read_to_string("/proc/self/stat") {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let close = match s.rfind(')') {
        Some(c) => c,
        None => return 0,
    };
    // After ")": field 3 (state), 4 (ppid), …; field 14 = utime, 15 = stime.
    let rest = &s[close + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ticks: u64 = f.get(11).and_then(|v| v.parse().ok()).unwrap_or(0)
        + f.get(12).and_then(|v| v.parse().ok()).unwrap_or(0);
    ticks * 10_000_000 // 100 Hz clock tick = 10 ms
}

#[cfg(all(unix, not(target_os = "macos")))]
fn parse_proc_status(key: &str) -> u64 {
    let s = match std::fs::read_to_string("/proc/self/status") {
        Ok(s) => s,
        Err(_) => return 0,
    };
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(rest) = rest.strip_prefix(':') {
                return rest
                    .trim()
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
            }
        }
    }
    0
}

// ---------------------------------------------------------------------------
// macOS / Darwin:
//   RSS  : proc_pidinfo(PROC_PIDTASKINFO) → proc_taskinfo.pti_resident_size
//   CPU  : task_info(MACH_TASK_BASIC_INFO) → user_time + system_time
//          (proc_pidinfo's pti_total_* misses most threads on Apple Silicon;
//          task_info aggregates across all task threads correctly.)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn macos_current_rss_kb() -> u64 {
    let mut ti: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let ret = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTASKINFO,
            0,
            &mut ti as *mut _ as *mut libc::c_void,
            std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int,
        )
    };
    if ret <= 0 {
        return 0;
    }
    // pti_resident_size is in bytes; convert to KiB.
    (ti.pti_resident_size / 1024) as u64
}

#[cfg(target_os = "macos")]
fn macos_cpu_ns() -> u64 {
    #[allow(deprecated)] // libc::mach_task_self is deprecated but still functional
    let task = unsafe { libc::mach_task_self() };
    let mut tbi: libc::mach_task_basic_info = unsafe { std::mem::zeroed() };
    let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
    let kr = unsafe {
        libc::task_info(
            task,
            libc::MACH_TASK_BASIC_INFO,
            (&mut tbi as *mut libc::mach_task_basic_info) as libc::task_info_t,
            &mut count,
        )
    };
    if kr != libc::KERN_SUCCESS {
        return 0;
    }
    // time_value_t = { seconds: i32, microseconds: i32 } → ns.
    let user_ns = (tbi.user_time.seconds as i64) * 1_000_000_000
        + (tbi.user_time.microseconds as i64) * 1_000;
    let sys_ns = (tbi.system_time.seconds as i64) * 1_000_000_000
        + (tbi.system_time.microseconds as i64) * 1_000;
    (user_ns + sys_ns).max(0) as u64
}
