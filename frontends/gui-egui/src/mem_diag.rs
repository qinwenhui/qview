//! 零依赖进程内存诊断。
//!
//! 通过 PSAPI (`kernel32.dll` + `psapi.dll`) 取 `WorkingSetSize` 与峰值；
//! 其它字段用 `size_of` / `Vec::capacity × size_of` 估算。所有输出走 `stderr`。
//!
//! 用途：分析"无任何文件打开"时空载 100 MB 的真实分布。

use std::collections::BTreeMap;
use std::mem::size_of;
use std::sync::Mutex;

use crate::app::QLogApp;

// ---------------------------------------------------------------------------
// PSAPI 直接 FFI — 零新依赖
// ---------------------------------------------------------------------------

#[cfg(windows)]
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

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {}

#[cfg(windows)]
#[link(name = "psapi")]
extern "system" {
    #[link_name = "GetProcessMemoryInfo"]
    fn psapi_get_process_memory_info(
        process: *mut std::ffi::c_void,
        counters: *mut PROCESS_MEMORY_COUNTERS,
        cb: u32,
    ) -> i32;
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> *mut std::ffi::c_void;
}

#[derive(Debug, Clone, Copy)]
pub struct ProcMem {
    pub working_set: u64,
    pub peak_working_set: u64,
    pub pagefile_usage: u64,
    pub peak_pagefile: u64,
    pub page_faults: u64,
}

#[cfg(windows)]
pub fn process_memory() -> ProcMem {
    unsafe {
        let mut info = std::mem::zeroed::<PROCESS_MEMORY_COUNTERS>();
        info.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = psapi_get_process_memory_info(
            GetCurrentProcess(),
            &mut info,
            info.cb,
        );
        if ok != 0 {
            ProcMem {
                working_set: info.working_set_size as u64,
                peak_working_set: info.peak_working_set_size as u64,
                pagefile_usage: info.pagefile_usage as u64,
                peak_pagefile: info.peak_pagefile_usage as u64,
                page_faults: info.page_fault_count as u64,
            }
        } else {
            ProcMem {
                working_set: 0, peak_working_set: 0, pagefile_usage: 0,
                peak_pagefile: 0, page_faults: 0,
            }
        }
    }
}

/// Non-Windows fallback: no PSAPI, so RSS is reported as 0 and only the heap
/// estimates in [`write_report`] are meaningful.
#[cfg(not(windows))]
pub fn process_memory() -> ProcMem {
    ProcMem {
        working_set: 0, peak_working_set: 0, pagefile_usage: 0,
        peak_pagefile: 0, page_faults: 0,
    }
}

// ---------------------------------------------------------------------------
// 工具函数
// ---------------------------------------------------------------------------

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

fn vec_cap_bytes<T>(v: &Vec<T>) -> u64 {
    (v.capacity() * size_of::<T>()) as u64
}

fn string_cap_bytes(s: &String) -> u64 {
    s.capacity() as u64
}

// ---------------------------------------------------------------------------
// 字体注册表 — fonts.rs 在装载每份字体时调用 register_font(name, bytes_read)
// ---------------------------------------------------------------------------

static FONT_REGISTRY: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());

pub fn register_font(name: &str, bytes_on_heap: u64) {
    FONT_REGISTRY.lock().unwrap().insert(name.to_string(), bytes_on_heap);
}

pub fn clear_font_registry() {
    FONT_REGISTRY.lock().unwrap().clear();
}

fn copy_font_registry() -> Vec<(String, u64)> {
    FONT_REGISTRY
        .lock()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect()
}

fn font_total() -> u64 {
    FONT_REGISTRY.lock().unwrap().values().sum()
}

// ---------------------------------------------------------------------------
// QLogApp 各字段 size 估算
// ---------------------------------------------------------------------------

#[derive(Default, Debug)]
pub struct AppEstimate {
    pub path: u64,
    pub engine_head: u64,
    pub index_offsets: u64,
    pub cache_raw_est: u64,
    pub cache_display_est: u64,
    pub search_hits: u64,
    pub search_lines: u64,
    pub themes: u64,
    pub available_fonts: u64,
    pub search_input: u64,
    pub search_status: u64,
    pub status_msg: u64,
    pub scroll_name: u64,
    pub theme_meta: u64,
    pub config: u64,
    pub total_estimated_objects: u64,
}

pub fn estimate_app(app: &QLogApp) -> AppEstimate {
    let mut e = AppEstimate::default();

    e.path = app.path.as_ref().map(|p| (p.as_os_str().len() * size_of::<u16>()) as u64).unwrap_or(0);

    if let Some(arc) = app.engine.as_ref() {
        let eng = arc.lock();
        e.engine_head =
            (size_of::<qview_core::engine::Engine>()
                + size_of::<qview_core::file::MmapBackend>()
                + size_of::<qview_core::file::LineIndex>()
                + size_of::<qview_core::cache::LineCache>()
                + size_of::<qview_core::search::SearchResults>()
                + size_of::<qview_core::edit::EditBuffer>()) as u64;
        // index offsets Vec<u64>
        let offsets = eng.index.snapshot_offsets();
        e.index_offsets = (offsets.capacity() * size_of::<u64>()) as u64;

        e.cache_raw_est = (eng.cache.raw_len() * 256) as u64;
        e.cache_display_est = (eng.cache.display_len() * 256) as u64;
    }

    e.search_hits = vec_cap_bytes(&app.search_hits);
    e.search_lines = vec_cap_bytes(&app.search_lines);
    e.search_input = string_cap_bytes(&app.search_input);
    e.search_status = string_cap_bytes(&app.search_status);
    e.status_msg = string_cap_bytes(&app.status_msg);
    e.scroll_name = std::mem::size_of::<f64>() as u64 * 4;

    // themes 是 Vec<Theme { String + bool + ThemeColors{ 21 × Color32 } }>
    let per_theme = size_of::<crate::style::Theme>()
        + 64                          // 估算 name 平均 64 byte capacity
        + 21 * size_of::<egui::Color32>();
    e.themes = (app.themes.capacity() * per_theme) as u64;

    e.available_fonts = {
        let mut b = 0u64;
        for s in &app.available_fonts {
            b += s.capacity() as u64;
        }
        b
    };

    // config 在 app 里是栈引用，但其 vec 字段按需；最近文件/搜索历史已迁到
    // store，这里统计的是内存缓存（来源 store 表）。
    e.config = size_of::<crate::config::AppConfig>() as u64
        + vec_cap_bytes(&*app.recent_files.lock())
        + vec_cap_bytes(&*app.search_history.lock());

    e.theme_meta = e.themes;

    e.total_estimated_objects = e.engine_head + e.index_offsets
        + e.cache_raw_est + e.cache_display_est
        + e.search_hits + e.search_lines
        + e.themes + e.available_fonts
        + e.search_input + e.search_status + e.status_msg
        + e.path + e.config + e.scroll_name;

    e
}

// ---------------------------------------------------------------------------
// 报告输出
// ---------------------------------------------------------------------------

pub fn write_report(label: &str, app: &QLogApp) {
    let proc = process_memory();
    let e = estimate_app(app);
    let fonts = copy_font_registry();
    let font_heap_total = font_total();
    let unresolved = proc.working_set as i64
        - e.total_estimated_objects as i64
        - font_heap_total as i64;

    let mut out = String::new();
    out.push_str(&format!("\n========== 内存快照: {} ==========\n", label));
    out.push_str("-- 进程级 (Windows PSAPI) --\n");
    out.push_str(&format!("  RSS (Working Set):     {}\n", human(proc.working_set)));
    out.push_str(&format!("  Peak RSS:              {}\n", human(proc.peak_working_set)));
    out.push_str(&format!("  Commit (Pagefile):     {}\n", human(proc.pagefile_usage)));
    out.push_str(&format!("  Peak Commit:           {}\n", human(proc.peak_pagefile)));
    out.push_str(&format!("  Page Faults:           {}\n", proc.page_faults));

    out.push_str("\n-- QLogApp 字段估算 (heap) --\n");
    out.push_str(&format!("  总计 (估算):           {}\n", human(e.total_estimated_objects)));
    out.push_str(&format!("  engine head (struct):  {}\n", human(e.engine_head)));
    out.push_str(&format!("  index offsets:         {}\n", human(e.index_offsets)));
    out.push_str(&format!("  cache raw (估 256B/n): {}\n", human(e.cache_raw_est)));
    out.push_str(&format!("  cache display:         {}\n", human(e.cache_display_est)));
    out.push_str(&format!("  search_hits:           {}\n", human(e.search_hits)));
    out.push_str(&format!("  search_lines:          {}\n", human(e.search_lines)));
    out.push_str(&format!("  themes[cap]:           {}\n", human(e.themes)));
    out.push_str(&format!("  available_fonts:       {}\n", human(e.available_fonts)));
    out.push_str(&format!("  search/status 字符串:  {}\n",
        human(e.search_input + e.search_status + e.status_msg)));
    out.push_str(&format!("  path/recents/history:  {}\n", human(e.path + e.config)));
    out.push_str(&format!("  scroll_y/h/etc (栈内): {}\n", human(e.scroll_name)));

    if !fonts.is_empty() {
        out.push_str("\n-- FontData 字节 (fonts.rs 装载到堆) --\n");
        for (name, sz) in &fonts {
            out.push_str(&format!("  {:<24} {}\n", name, human(*sz)));
        }
        out.push_str(&format!("  小计 font heap:        {}\n", human(font_heap_total)));
    } else {
        out.push_str("\n-- FontData 字节 --\n  (空 — Q_LOG_NO_FONTS=1 或未扫描到 assets)\n");
    }

    out.push_str("\n-- 未识别 / 系统 DLL / egui 内部 --\n");
    out.push_str(&format!("  RSS - 上面估算值:      {}\n",
        human(unresolved.max(0) as u64)));
    out.push_str("  说明: 包括 egui Context/InputState/Memory + epaint 字体 atlas\n");
    out.push_str("        (rasterize 后大几十 MB)、glow/OpenGL 资源、PSAPI/GLFW/dwmapi\n");
    out.push_str("        等系统 DLL、regex-automata 静态 lazy、encoding_rs 静态表。\n");
    out.push_str("==========================================\n");

    #[cfg(windows)]
    append_dll_breakdown(&mut out);

    eprint!("{}", out);
}

// ---------------------------------------------------------------------------
// 已加载 DLL 列举 (PSAPI EnumProcessModules + GetModuleInformation)
// ---------------------------------------------------------------------------
//
// 直读每个加载模块的 SizeOfImage，让我们看到"系统 DLL 总和"
// 与我们 Rust 二进制段 + RUNTIME_DLL 的边界。

#[cfg(windows)]
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct MODULEINFO {
    lp_base_of_dll: *mut std::ffi::c_void,
    size_of_image: u32,
    entry_point: *mut std::ffi::c_void,
}

#[cfg(windows)]
#[link(name = "psapi")]
extern "system" {
    #[link_name = "EnumProcessModules"]
    fn enum_process_modules(
        process: *mut std::ffi::c_void,
        lph_module: *mut *mut std::ffi::c_void,
        cb: u32,
        lpcb_needed: *mut u32,
    ) -> i32;

    #[link_name = "GetModuleInformation"]
    fn get_module_information(
        process: *mut std::ffi::c_void,
        module: *mut std::ffi::c_void,
        info: *mut MODULEINFO,
        cb: u32,
    ) -> i32;
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    #[link_name = "GetModuleBaseNameW"]
    fn get_module_base_name_w(
        process: *mut std::ffi::c_void,
        module: *mut std::ffi::c_void,
        base_name: *mut u16,
        size: u32,
    ) -> u32;
}

#[cfg(windows)]
#[derive(Debug, Clone)]
pub struct ModuleInfo {
    pub name: String,
    pub size_of_image: u64,
}

#[cfg(windows)]
fn wide_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

#[cfg(windows)]
pub fn list_loaded_modules() -> Vec<ModuleInfo> {
    unsafe {
        let process = GetCurrentProcess();
        let mut needed: u32 = 0;
        // 第一遍调用获取大小
        let _ = enum_process_modules(
            process,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
        if needed == 0 {
            return Vec::new();
        }
        let count = (needed as usize) / std::mem::size_of::<*mut std::ffi::c_void>();
        let mut handles = vec![std::ptr::null_mut::<std::ffi::c_void>(); count];
        let ok = enum_process_modules(
            process,
            handles.as_mut_ptr(),
            needed,
            &mut needed,
        );
        if ok == 0 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(count);
        for &h in &handles {
            if h.is_null() {
                continue;
            }
            let mut info = MODULEINFO::default();
            if get_module_information(process, h, &mut info, size_of::<MODULEINFO>() as u32) != 0 {
                let mut name_buf = [0u16; 512];
                let len = get_module_base_name_w(process, h, name_buf.as_mut_ptr(), name_buf.len() as u32);
                let name = if len > 0 {
                    wide_to_string(&name_buf)
                } else {
                    String::from("?")
                };
                out.push(ModuleInfo {
                    name,
                    size_of_image: info.size_of_image as u64,
                });
            }
        }
        out
    }
}

#[cfg(windows)]
fn append_dll_breakdown(out: &mut String) {
    let mods = list_loaded_modules();
    if mods.is_empty() {
        return;
    }
    // 按 size 倒序
    let mut mods = mods;
    mods.sort_by(|a, b| b.size_of_image.cmp(&a.size_of_image));
    let total: u64 = mods.iter().map(|m| m.size_of_image).sum();

    out.push_str("\n-- 已加载 DLL 镜像大小 (PE SizeOfImage 总和) --\n");
    out.push_str(&format!("  (DLL 镜像总和:       {} )\n", human(total)));
    out.push_str("  Top 20 (按 SizeOfImage):\n");
    for m in mods.iter().take(20) {
        out.push_str(&format!("    {:<32} {}\n", m.name, human(m.size_of_image)));
    }
}
