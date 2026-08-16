//! 进程级内存诊断（零依赖）。

use std::ffi::c_void;

#[repr(C)]
#[derive(Default)]
struct PROCESS_MEMORY_COUNTERS {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
}

#[link(name = "psapi")]
extern "system" {
    #[link_name = "GetProcessMemoryInfo"]
    fn get_process_memory_info(
        process: *mut c_void,
        counters: *mut PROCESS_MEMORY_COUNTERS,
        cb: u32,
    ) -> i32;

    #[link_name = "EnumProcessModules"]
    fn enum_process_modules(
        process: *mut c_void,
        lph_module: *mut *mut c_void,
        cb: u32,
        lpcb_needed: *mut u32,
    ) -> i32;

    #[link_name = "GetModuleInformation"]
    fn get_module_information(
        process: *mut c_void,
        module: *mut c_void,
        info: *mut MODULEINFO,
        cb: u32,
    ) -> i32;

    #[link_name = "GetModuleBaseNameW"]
    fn get_module_base_name_w(
        process: *mut c_void,
        module: *mut c_void,
        base_name: *mut u16,
        size: u32,
    ) -> u32;
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct MODULEINFO {
    lp_base_of_dll: *mut c_void,
    size_of_image: u32,
    entry_point: *mut c_void,
}

pub fn rss_bytes() -> u64 {
    unsafe {
        let mut info = std::mem::zeroed::<PROCESS_MEMORY_COUNTERS>();
        info.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = get_process_memory_info(GetCurrentProcess(), &mut info, info.cb);
        if ok != 0 {
            info.working_set_size as u64
        } else {
            0
        }
    }
}

fn human(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB"];
    let mut n = n as f64;
    let mut i = 0;
    while n >= 1024.0 && i < UNITS.len() - 1 {
        n /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{:>7} {}", n as u64, UNITS[i])
    } else {
        format!("{:>7.2} {}", n, UNITS[i])
    }
}

pub fn log_initial() {
    eprintln!("[mem] main() 入口 RSS:       {}", human(rss_bytes()));
}

pub fn log_post_window() {
    eprintln!("[mem] 创建窗口后 RSS:        {}", human(rss_bytes()));
}

pub fn log_exit() {
    eprintln!("[mem] 退出时 RSS:            {}", human(rss_bytes()));
}

pub fn spawn_periodic_reporter() {
    std::thread::spawn(|| {
        use std::time::Duration;
        std::thread::sleep(Duration::from_secs(2));
        eprintln!("[mem] 启动后 2s RSS:         {}", human(rss_bytes()));
        std::thread::sleep(Duration::from_secs(3));
        eprintln!("[mem] 启动后 5s RSS:         {}", human(rss_bytes()));
        print_dlls();
    });
}

fn wide_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

fn list_dlls() -> Vec<(String, u64)> {
    unsafe {
        let process = GetCurrentProcess();
        let mut needed: u32 = 0;
        let _ = enum_process_modules(process, std::ptr::null_mut(), 0, &mut needed);
        if needed == 0 {
            return Vec::new();
        }
        let count = needed as usize / std::mem::size_of::<*mut c_void>();
        let mut handles = vec![std::ptr::null_mut::<c_void>(); count];
        let ok = enum_process_modules(
            process,
            handles.as_mut_ptr(),
            needed,
            &mut needed,
        );
        if ok == 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        for &h in &handles {
            if h.is_null() {
                continue;
            }
            let mut info = MODULEINFO::default();
            if get_module_information(process, h, &mut info, std::mem::size_of::<MODULEINFO>() as u32) != 0 {
                let mut name_buf = [0u16; 256];
                let len = get_module_base_name_w(
                    process,
                    h,
                    name_buf.as_mut_ptr(),
                    name_buf.len() as u32,
                );
                let name = if len > 0 {
                    wide_to_string(&name_buf)
                } else {
                    String::from("?")
                };
                out.push((name, info.size_of_image as u64));
            }
        }
        out
    }
}

fn print_dlls() {
    let mut mods = list_dlls();
    mods.sort_by(|a, b| b.1.cmp(&a.1));
    let total: u64 = mods.iter().map(|(_, s)| *s).sum();
    eprintln!("\n-- 已加载 DLL 镜像 Top 15, 总和 {} --", human(total));
    for (name, sz) in mods.iter().take(15) {
        eprintln!("    {:<32} {}", name, human(*sz));
    }
}
