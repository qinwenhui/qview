//! 底部状态栏：三区（左路径 / 中状态 / 右统计）+ 顶部进度条行。
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────┐
//! │ ▓▓▓▓▓▓░░░ 42% 索引中...                    [取消]             │  ← 进度行(16px)
//! │ …app.log         已打开 · 12 行 · 4KiB    GBK │ 12行 │ 4KiB │ 编码 │ 批注(0) │  ← 状态行
//! └──────────────────────────────────────────────────────────────┘
//! ```
//! 编码 / 批注标签的命中矩形在渲染时记录到 `StatusRects`，供鼠标点击定位。

use std::ffi::c_void;

use windows_sys::Win32::Foundation::RECT;

use crate::app::{str_wide, App};
use crate::paint;

/// 可点击标签的命中矩形（渲染时填充）
#[derive(Clone, Copy)]
pub struct StatusRects {
    pub enc: RECT,
    pub ann: RECT,
}

impl Default for StatusRects {
    fn default() -> Self {
        Self {
            enc: RECT { left: 0, top: 0, right: 0, bottom: 0 },
            ann: RECT { left: 0, top: 0, right: 0, bottom: 0 },
        }
    }
}

pub const PROGRESS_H: i32 = 16;

/// 是否有后台任务需要进度条（索引 / 搜索）
pub fn has_progress(app: &App) -> bool {
    if let Some(ref b) = app.bridge {
        if b.indexing_active() {
            return true;
        }
    }
    app.search.searching
}

/// 解析进度文本里的 "NN%" → 0.0..1.0（与 egui progress_frac 相同）
pub fn progress_frac(text: &str) -> Option<f32> {
    let pct = text.rsplit('%').next()?.rsplit(' ').next()?;
    let n: f32 = pct.parse().ok()?;
    Some((n / 100.0).clamp(0.0, 1.0))
}

/// 当前进度：(frac, label, is_indexing)
pub fn current_progress(app: &App) -> Option<(f32, String, bool)> {
    let b = app.bridge.as_ref()?;
    let engine = b.engine.lock().unwrap();
    if let Some(ref p) = engine.index_progress {
        let frac = progress_frac(p).unwrap_or(0.0);
        Some((frac, p.clone(), true))
    } else if let Some(ref p) = engine.search_progress {
        let frac = progress_frac(p).unwrap_or(0.0);
        Some((frac, p.clone(), false))
    } else {
        None
    }
}

/// 渲染进度行 + 状态行。坐标：`progress_top..status_top` 进度行，`status_top..h` 状态行。
pub unsafe fn render(
    hdc: *mut c_void,
    w: i32,
    progress_top: i32,
    status_top: i32,
    h: i32,
    app: &mut App,
    rects: &mut StatusRects,
) {
    // ── 进度行 ──
    if progress_top < status_top {
        let bar = RECT { left: 0, top: progress_top, right: w, bottom: status_top };
        paint::fill_rect(hdc, &bar, app.theme.bg_secondary);
        if let Some((frac, label, is_indexing)) = current_progress(app) {
            let fill_w = ((w as f32) * frac) as i32;
            let fill_color = if is_indexing { app.theme.info } else { app.theme.success };
            if fill_w > 0 {
                paint::fill_rect(hdc, &RECT { left: 0, top: progress_top, right: fill_w, bottom: status_top }, fill_color);
            }
            let font = ensure_status_font(app);
            let old = paint::select_font_safe(hdc, font);
            let tw: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
            paint::draw_text_c(hdc, 6, progress_top + 2, app.theme.text_primary, &tw);
            // 取消按钮（右侧）
            let cancel_w = 56;
            let cancel = RECT { left: w - cancel_w - 4, top: progress_top + 2, right: w - 4, bottom: status_top - 2 };
            paint::fill_rect(hdc, &cancel, app.theme.btn_danger);
            let cw: Vec<u16> = "取消".encode_utf16().chain(std::iter::once(0)).collect();
            paint::draw_text_c(hdc, cancel.left + 2, cancel.top + 1, app.theme.text_primary, &cw);
            app.progress_cancel_rect = cancel;
            paint::restore_font(hdc, old);
        } else {
            app.progress_cancel_rect = std::mem::zeroed();
        }
    }

    // ── 状态行 ──
    paint::fill_rect(hdc, &RECT { left: 0, top: status_top, right: w, bottom: h }, app.theme.statusbar_bg);
    let font = ensure_status_font(app);
    let old = paint::select_font_safe(hdc, font);
    let text_h = h - status_top;

    // 左区：路径（截断 50 字）
    let left_w = ((w as f32) * 0.38).min(420.0) as i32;
    if let Some(ref p) = app.path {
        let full = p.display().to_string();
        let s = if full.chars().count() > 50 {
            let tail: String = full.chars().skip(full.chars().count() - 49).collect();
            format!("…{}", tail)
        } else {
            full
        };
        paint::draw_text_clipped_c(hdc, 6, status_top, left_w - 12, app.theme.statusbar_text, &str_wide(&s));
    } else {
        paint::draw_text_c(hdc, 6, status_top, app.theme.text_disabled, &str_wide("未打开文件"));
    }

    // 中区：搜索状态优先（有查询时显示 N/M 匹配），否则状态消息
    // 直接读 search.status（jump_hit 每步都更新），而非陈旧的 search_status
    let center_x = left_w + 8;
    let center_w = w - center_x - 320;
    if center_w > 40 {
        if !app.search.query.is_empty() && !app.search.status.is_empty() {
            paint::draw_text_clipped_c(hdc, center_x, status_top, center_w, app.theme.warning, &str_wide(&app.search.status));
        } else if !app.status_text.is_empty() {
            paint::draw_text_clipped_c(hdc, center_x, status_top, center_w, app.theme.success, &str_wide(&app.status_text));
        }
    }

    // 右区：大小 | 行数 | 编码 | 批注(N)
    let mut right_x = w - 6;
    // 批注
    let ann_label = format!("批注({})", app.annotation_count);
    let ann_w = paint::text_width(hdc, &str_wide(&ann_label));
    right_x -= ann_w;
    paint::draw_text_c(hdc, right_x, status_top, app.theme.warning, &str_wide(&ann_label));
    rects.ann = RECT { left: right_x - 2, top: status_top, right: right_x + ann_w + 2, bottom: status_top + text_h };
    right_x -= 14;
    // 编码
    let enc_label = app.config.engine.encoding.clone();
    let enc_w = paint::text_width(hdc, &str_wide(&enc_label));
    right_x -= enc_w;
    paint::draw_text_c(hdc, right_x, status_top, app.theme.info, &str_wide(&enc_label));
    rects.enc = RECT { left: right_x - 2, top: status_top, right: right_x + enc_w + 2, bottom: status_top + text_h };
    // 分隔 │
    right_x -= 16;
    paint::draw_text_c(hdc, right_x, status_top, app.theme.text_disabled, &str_wide("│"));
    // 行数
    if let Some(ref b) = app.bridge {
        let line_label = format!("{} 行", b.line_count);
        right_x -= paint::text_width(hdc, &str_wide(&line_label)) + 10;
        paint::draw_text_c(hdc, right_x, status_top, app.theme.statusbar_text, &str_wide(&line_label));
        // 大小
        paint::draw_text_c(hdc, right_x, status_top, app.theme.text_disabled, &str_wide("│"));
        let size_label = paint::human_bytes(b.size);
        let size_w = paint::text_width(hdc, &str_wide(&size_label));
        right_x -= size_w + 12;
        paint::draw_text_c(hdc, right_x, status_top, app.theme.statusbar_text, &str_wide(&size_label));
    }

    paint::restore_font(hdc, old);
}

fn ensure_status_font(app: &mut App) -> windows_sys::Win32::Graphics::Gdi::HFONT {
    if app.status_font.is_none() {
        app.status_font = Some(paint::create_font(12, "Segoe UI"));
    }
    app.status_font.unwrap()
}
