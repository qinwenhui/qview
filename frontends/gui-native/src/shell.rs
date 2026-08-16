//! 调用 comdlg32!GetOpenFileNameW 弹文件选择窗口。
//! 零依赖，免 rfd crate。

use std::path::PathBuf;

const MAX_PATH: usize = 4096;

#[repr(C)]
#[allow(non_snake_case)] // Win32 结构字段名
struct OPENFILENAMEW {
    lStructSize: u32,
    hwndOwner: *mut std::ffi::c_void,
    hInstance: *mut std::ffi::c_void,
    lpstrFilter: *const u16,
    lpstrCustomFilter: *const u16,
    nMaxCustFilter: u32,
    nFilterIndex: u32,
    lpstrFile: *mut u16,
    nMaxFile: u32,
    lpstrFileTitle: *mut u16,
    nMaxFileTitle: u32,
    lpstrInitialDir: *const u16,
    lpstrTitle: *const u16,
    Flags: u32,
    nFileOffset: u16,
    nFileExtension: u16,
    lpstrDefExt: *const u16,
    lCustData: usize,
    lpfnHook: *const std::ffi::c_void,
    lpTemplateName: *const u16,
    pvReserved: *mut std::ffi::c_void,
    dwReserved: u32,
    FlagsEx: u32,
}

const OFN_EXPLORER: u32 = 0x00080000;
const OFN_FILEMUSTEXIST: u32 = 0x00001000;
const OFN_HIDEREADONLY: u32 = 0x00000100;

#[link(name = "comdlg32")]
extern "system" {
    fn GetOpenFileNameW(ofn: *mut OPENFILENAMEW) -> i32;
}

pub fn pick_file() -> Option<PathBuf> {
    unsafe {
        let mut buffer = [0u16; MAX_PATH];
        let initial_dir: Vec<u16> = "C:\\"
            .encode_utf16().chain(std::iter::once(0)).collect();
        let title = "选择日志文件";
        let title_w: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();

        let filter = "日志文件\0*.log;*.txt;*.out;*.csv;*.json;*.xml\0所有文件\0*.*\0\0";
        let filter_w: Vec<u16> = filter.encode_utf16().chain(std::iter::once(0)).collect();

        let mut ofn: OPENFILENAMEW = std::mem::zeroed();
        ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        ofn.lpstrFilter = filter_w.as_ptr();
        ofn.nFilterIndex = 1;
        ofn.lpstrFile = buffer.as_mut_ptr();
        ofn.nMaxFile = buffer.len() as u32;
        ofn.lpstrInitialDir = initial_dir.as_ptr();
        ofn.lpstrTitle = title_w.as_ptr();
        ofn.Flags = OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_HIDEREADONLY;

        let ok = GetOpenFileNameW(&mut ofn);
        if ok != 0 {
            let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
            let path_str = String::from_utf16_lossy(&buffer[..len]);
            Some(PathBuf::from(path_str))
        } else {
            None
        }
    }
}
