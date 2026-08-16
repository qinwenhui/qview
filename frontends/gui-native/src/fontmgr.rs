//! 字体管理：默认 Consolas / 用户选定字体。

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Graphics::Gdi::HFONT;

use crate::paint;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FontSetting {
    pub name: String,
    pub pixel: i32,
}

impl Default for FontSetting {
    fn default() -> Self {
        Self {
            name: "Consolas".into(),
            pixel: 14,
        }
    }
}

impl FontSetting {
    pub fn make(&self) -> HFONT {
        paint::create_font(self.pixel, &self.name)
    }
}

/// 系统等宽字体列表（设置-字体下拉用）。
pub fn all_system_fonts() -> Vec<String> {
    enum_mono_fonts()
}

/// 用 EnumFontFamiliesExW 枚举系统等宽字体，回退常用列表。
pub fn enum_mono_fonts() -> Vec<String> {
    
    use windows_sys::Win32::Graphics::Gdi::{
        CreateDCW, DeleteDC, EnumFontFamiliesExW, FIXED_PITCH, LOGFONTW, OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, FF_MODERN, FW_NORMAL,
    };

    unsafe {
        let names = std::sync::Mutex::new(Vec::<String>::new());
        let screen = CreateDCW(std::ptr::null(), "DISPLAY".encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>().as_ptr(), std::ptr::null(), std::ptr::null());
        if screen.is_null() {
            return fallback_fonts();
        }
        let mut lf: LOGFONTW = std::mem::zeroed();
        lf.lfCharSet = DEFAULT_CHARSET as u8;
        lf.lfPitchAndFamily = (FIXED_PITCH as u8) | (FF_MODERN as u8);
        lf.lfWeight = FW_NORMAL as i32;
        lf.lfOutPrecision = OUT_DEFAULT_PRECIS as u8;
        lf.lfClipPrecision = CLIP_DEFAULT_PRECIS as u8;

        extern "system" fn cb(
            lpelfe: *const windows_sys::Win32::Graphics::Gdi::LOGFONTW,
            _lptm: *const windows_sys::Win32::Graphics::Gdi::TEXTMETRICW,
            _fonttype: u32,
            lparam: isize,
        ) -> i32 {
            unsafe {
                let lf = &*lpelfe;
                let len = lf.lfFaceName.iter().position(|&c| c == 0).unwrap_or(32);
                let name = String::from_utf16_lossy(&lf.lfFaceName[..len]);
                let list = &*(lparam as *const std::sync::Mutex<Vec<String>>);
                if !name.is_empty() && !list.lock().unwrap().contains(&name) {
                    list.lock().unwrap().push(name);
                }
            }
            1
        }

        let _ = EnumFontFamiliesExW(screen, &lf, Some(cb), &names as *const _ as isize, 0);
        DeleteDC(screen);
        let list = names.into_inner().unwrap_or_default();
        if list.is_empty() {
            fallback_fonts()
        } else {
            list
        }
    }
}

fn fallback_fonts() -> Vec<String> {
    vec![
        "Consolas".into(),
        "Courier New".into(),
        "Lucida Console".into(),
        "Cascadia Mono".into(),
        "MS Gothic".into(),
        "SimSun".into(),
    ]
}
