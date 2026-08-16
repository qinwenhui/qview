//! 主视图渲染：虚拟滚动 + 精确搜索高亮 + 文本选择 + word wrap + 主题色
//! + 空白字符 / 缩进参考线 / 批注标记 / 交替行 + 自绘双滚动条。
//!
//! 滚动模型：`scroll.y` = 可视行号（有效行）。非 wrap 时 wrap_factor=1，
//! 可视行 == 逻辑行；wrap 时 `wrap_factor ≈ 每逻辑行的平均可视行数`，
//! 首行锚定 + 前方 slack 保证视口覆盖。列位置用 UTF-16 单元索引。

use std::ffi::c_void;

use qview_core::search::Query;
use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::{DeleteObject, HFONT};

use crate::app::App;
use crate::layout;
use crate::paint;
use crate::theme::Rgb;

#[derive(Default)]
pub struct ViewMetrics {
    pub view_w: i32,
    pub view_h: i32,
    pub font_size_px: i32,
    pub row_h: i32,
    pub font: Option<HFONT>,
    pub font_size_cached: i32,
}

impl ViewMetrics {
    pub fn invalidate(&mut self) {
        self.font_size_cached = -1;
    }
}

#[derive(Default)]
pub struct ViewLayout {
    pub laid: Vec<layout::LaidLine>,
    pub content_x: i32,
    pub gutter_w: i32,
    pub text_right: i32,
    pub view_top: i32,
    pub view_h: i32,
    pub row_h: i32,
}

/// 批注强调色（琥珀）。
const ANNO_AMBER: Rgb = Rgb { r: 224, g: 172, b: 56 };

/// 匹配区间迭代器（字节偏移，与 egui viewer.rs 相同语义）。
fn query_ranges_in<'a>(
    q: &'a Query,
    hay: &'a [u8],
) -> Box<dyn Iterator<Item = (usize, usize)> + 'a> {
    match q {
        Query::Literal(p) => Box::new(memchr::memmem::find_iter(hay, p).map(move |m| (m, m + p.len()))),
        Query::Regex(re) => Box::new(re.find_iter(hay).map(move |m| (m.start(), m.end()))),
    }
}

/// 建立 byte→UTF-16 单元 映射（复用缓冲）。
fn build_byte_to_unit(text: &str, out: &mut Vec<usize>) {
    out.clear();
    out.resize(text.len() + 1, 0);
    let mut u = 0usize;
    let mut i = 0usize;
    for ch in text.chars() {
        let blen = ch.len_utf8();
        for k in 0..blen {
            out[i + k] = u;
        }
        u += ch.len_utf16();
        i += blen;
    }
    out[text.len()] = u;
}

/// 日志级别着色（返回主题色；匹配 [ERROR] / ERROR: / "ERROR" 等模式）。
/// 无分配：只对 ASCII 做一次大写化，字节窗口比较。
fn level_color(text: &str, theme: &crate::theme::ThemeColors) -> Option<Rgb> {
    // ASCII 日志占绝对多数；用 to_ascii_uppercase 避免 Unicode 大写分配
    let upper = text.to_ascii_uppercase();
    let u = upper.as_bytes();
    let levels: &[(&str, Rgb)] = &[
        ("ERROR", theme.level_error),
        ("FATAL", theme.level_error),
        ("CRIT", theme.level_error),
        ("WARN", theme.level_warn),
        ("WARNING", theme.level_warn),
        ("INFO", theme.level_info),
        ("NOTICE", theme.level_info),
        ("DEBUG", theme.level_debug),
        ("TRACE", theme.level_trace),
    ];
    for (word, color) in levels {
        if has_level(u, word.as_bytes()) {
            return Some(*color);
        }
    }
    None
}

/// 字节窗口级别匹配：`[LEVEL]` / `LEVEL:` / `"LEVEL"` / `<LEVEL>` / ` LEVEL ` / 行首行尾。
fn has_level(u: &[u8], l: &[u8]) -> bool {
    let n = l.len();
    if n == 0 {
        return false;
    }
    // [LEVEL]
    if u.windows(n + 2).any(|w| w[0] == b'[' && w[n + 1] == b']' && &w[1..n + 1] == l) {
        return true;
    }
    // LEVEL:
    if u.windows(n + 1).any(|w| w[n] == b':' && &w[..n] == l) {
        return true;
    }
    // "LEVEL"
    if u.windows(n + 2).any(|w| w[0] == b'"' && w[n + 1] == b'"' && &w[1..n + 1] == l) {
        return true;
    }
    // <LEVEL>
    if u.windows(n + 2).any(|w| w[0] == b'<' && w[n + 1] == b'>' && &w[1..n + 1] == l) {
        return true;
    }
    // " LEVEL "
    if u.windows(n + 2).any(|w| w[0] == b' ' && w[n + 1] == b' ' && &w[1..n + 1] == l) {
        return true;
    }
    // 行首 "LEVEL "
    if u.len() > n && u.starts_with(l) && u[n] == b' ' {
        return true;
    }
    // 行尾 " LEVEL"
    if u.len() > n && u.ends_with(l) && u[u.len() - n - 1] == b' ' {
        return true;
    }
    false
}

/// 主视图渲染（双缓冲已由 app.paint 管理）。
pub fn render_view(hdc: *mut c_void, rect: &RECT, app: &mut App) {
    unsafe {
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        if w <= 0 || h <= 0 {
            return;
        }

        let font_size = app.metrics.font_size_cached.max(8);
        app.metrics.row_h = (font_size + 5).max(14);
        let row_h = app.metrics.row_h;
        app.row_h = row_h;
        let font = ensure_font(app);
        let old_font = paint::select_font_safe(hdc, font);

        app.metrics.view_w = w;
        app.metrics.view_h = h;

        // ASCII 字符宽度缓存
        app.char_width_cache.ensure(hdc, font_size);
        let ascii = app.char_width_cache.ascii;
        let avg_char_w = ascii[48].max(6); // '0' 宽度

        // 无文件：画提示并返回
        if app.bridge.is_none() {
            let msg = crate::app::str_wide("按 Ctrl+O 打开日志文件");
            paint::draw_text_c(hdc, rect.left + 12, rect.top + 8, app.theme.text_secondary, &msg);
            app.view.laid.clear();
            app.first_visible_line = 0;
            app.last_visible_line = 0;
            paint::restore_font(hdc, old_font);
            return;
        }

        let word_wrap = app.config.gui.word_wrap;
        let show_line_nums = app.config.gui.show_line_numbers;

        // 行号 gutter 宽
        let gutter_w = if show_line_nums {
            let probe = crate::app::str_wide("000000 │");
            layout::measure_text(hdc, &probe).max(60)
        } else {
            0
        };

        let sb_w = 14;
        let text_right = rect.left + w - sb_w;
        let content_w = (text_right - rect.left - gutter_w - 6).max(40);

        // wrap 因子（egui 同款稳定近似）
        let wrap_factor = if word_wrap {
            ((100.0 * avg_char_w as f64 / content_w as f64).ceil()).max(2.0)
        } else {
            1.0
        };
        app.wrap_factor = wrap_factor;
        app.scroll.wrap_factor = wrap_factor;

        let total = app.total_lines().max(1);

        // 有效总行数 = 逻辑行 × wrap 因子；把 scroll.y 夹到合法范围，
        // 防止拖滚动条/翻页过头导致排版为空（文本消失）
        let view_h_px = (rect.bottom - rect.top).max(1) as f64;
        app.scroll.page_size_lines = (view_h_px / row_h as f64).max(1.0) as i64;
        let total_rows = total as f64 * wrap_factor;
        let max_scroll_y = ((total_rows * row_h as f64 - view_h_px) / row_h as f64).max(0.0) as i64;
        if app.scroll.y > max_scroll_y {
            app.scroll.y = max_scroll_y;
        }

        // 首行锚定
        let eff = app.scroll.y.max(0) as f64;
        let first_logical = (eff / wrap_factor).floor().max(0.0) as u64;
        let partial = (eff - first_logical as f64 * wrap_factor) as i32;

        let mut y_cursor = rect.top - partial * row_h;
        let mut laid: Vec<layout::LaidLine> = Vec::with_capacity(64);
        let mut longest_px: i32 = 0;
        let mut line_no = first_logical;

        // ── Phase A: 排版可见行 ──
        let bridge = app.bridge.as_ref().unwrap();
        while y_cursor < rect.bottom + row_h * 2 && line_no < total {
            let raw = bridge.read_raw(line_no);
            let text = &raw.text;
            app.utf16_scratch.clear();
            app.utf16_scratch.extend(text.encode_utf16());
            layout::line_char_widths(hdc, &app.utf16_scratch, &ascii, &mut app.width_scratch);

            let rows = &mut app.row_scratch;
            if word_wrap && !app.width_scratch.is_empty() {
                layout::wrap_line_into(&app.width_scratch, content_w, rows);
            } else {
                rows.clear();
                rows.push(layout::VisualRow {
                    start: 0,
                    end: app.utf16_scratch.len(),
                    x_off: 0,
                    width: app.width_scratch.iter().sum(),
                });
            }
            let line_h = rows.len() as i32 * row_h;
            if !word_wrap && !app.width_scratch.is_empty() {
                longest_px = longest_px.max(rows[0].width);
            }
            laid.push(layout::LaidLine {
                line: line_no,
                y_top: y_cursor,
                rows: app.row_scratch.clone(),
                line_h,
                char_w: app.width_scratch.clone(),
                units: app.utf16_scratch.clone(),
                text: raw.text.clone(),
                start_byte: raw.start_byte,
            });
            y_cursor += line_h;
            line_no += 1;
        }

        // 发布排版结果
        let content_x = rect.left + gutter_w + 4;
        app.view.laid = laid;
        app.view.content_x = content_x;
        app.view.gutter_w = gutter_w;
        app.view.text_right = text_right;
        app.view.view_top = rect.top;
        app.view.view_h = h;
        app.view.row_h = row_h;

        let first_vis = app.view.laid.iter().find(|l| l.y_top + l.line_h > rect.top).map(|l| l.line).unwrap_or(first_logical);
        app.first_visible_line = first_vis;
        app.last_visible_line = app.view.laid.last().map(|l| l.line).unwrap_or(first_logical);

        // 文本区底色
        paint::fill_rect(hdc, &RECT { left: rect.left, top: rect.top, right: text_right, bottom: rect.top + h }, app.theme.bg_primary);
        if gutter_w > 0 {
            paint::fill_rect(hdc, &RECT { left: rect.left, top: rect.top, right: rect.left + gutter_w, bottom: rect.top + h }, app.theme.line_number_bg);
            paint::fill_rect(hdc, &RECT { left: rect.left + gutter_w, top: rect.top, right: rect.left + gutter_w + 1, bottom: rect.top + h }, app.theme.bg_hover);
        }

        // 缩进参考线：每 4 列画竖线（按各行的前导缩进）
        let show_indent = app.config.gui.show_indent_guides;
        let show_ws = app.config.gui.show_whitespace;
        let do_color = app.config.gui.level_coloring;
        let search_active = !app.search.query.is_empty();

        // 当前命中字节（本帧更新）
        app.current_hit_byte = app.bridge.as_ref().and_then(|b| b.search_current()).map(|m| m.byte);

        for l in &app.view.laid {
            let text = &l.text;
            let line_start = l.start_byte;
            let line_end = line_start + text.len() as u64;
            let line_units = &l.units;
            let _ = line_end;
            // 行前导缩进列数（空格=1，Tab=4）
            let mut leading_cols = 0usize;
            if show_indent {
                for ch in text.chars() {
                    match ch {
                        ' ' => leading_cols += 1,
                        '\t' => leading_cols += 4,
                        _ => break,
                    }
                }
            }
            let text_color = if do_color {
                level_color(text, &app.theme).unwrap_or(app.theme.text_primary)
            } else {
                app.theme.text_primary
            };
            let is_annotated = app.annotations.marked.contains(&l.line);

            // 本行搜索命中区间（UTF-16 单元 + 是否当前命中），每行只扫一次
            let mut line_matches: Vec<(usize, usize, bool)> = Vec::new();
            if search_active {
                if let Some(q) = &app.parsed_q {
                    build_byte_to_unit(text, &mut app.byte_unit_scratch);
                    let b2u = &app.byte_unit_scratch;
                    for (a, b) in query_ranges_in(q, text.as_bytes()) {
                        let ua = b2u[a];
                        let ub = if b <= text.len() { b2u[b] } else { ua };
                        let is_current = app.current_hit_byte.map_or(false, |hb| {
                            let abs_a = line_start + a as u64;
                            let abs_b = line_start + b as u64;
                            hb >= abs_a && hb < abs_b
                        });
                        line_matches.push((ua, ub, is_current));
                    }
                }
            }

            // 逐可视子行绘制
            for (ri, row) in l.rows.iter().enumerate() {
                let row_y = l.y_top + ri as i32 * row_h;
                if row_y + row_h <= rect.top || row_y >= rect.top + h {
                    continue;
                }
                let row_rect = RECT { left: rect.left, top: row_y, right: text_right, bottom: row_y + row_h };

                // 交替行背景
                if l.line % 2 == 1 {
                    paint::fill_rect(hdc, &row_rect, app.theme.bg_tertiary);
                }

                // 批注行：整行淡淡琥珀底色，一眼可见
                if is_annotated {
                    paint::fill_rect(hdc, &row_rect, app.theme.bg_primary.blend(ANNO_AMBER, 16));
                }

                // 缩进参考线
                if show_indent {
                    let mut g = 4usize;
                    while g <= leading_cols {
                        let gx = content_x + (g as i32) * avg_char_w;
                        if gx < text_right {
                            paint::fill_rect(hdc, &RECT { left: gx, top: row_y, right: gx + 1, bottom: row_y + row_h }, app.theme.indent_guide);
                        }
                        g += 4;
                    }
                }

                // 批注标记（加宽琥珀竖条，整行可见高度）
                if is_annotated {
                    paint::fill_rect(hdc, &RECT { left: content_x - 5, top: row_y, right: content_x - 1, bottom: row_y + row_h }, ANNO_AMBER);
                }

                // 搜索高亮（背景矩形，按已收集区间）
                for &(ua, ub, is_current) in &line_matches {
                    if ub <= row.start || ua >= row.end {
                        continue;
                    }
                    let ia = ua.max(row.start);
                    let ib = ub.min(row.end);
                    let x0 = content_x + layout::prefix_width(&l.char_w, 0, ia);
                    let x1 = content_x + layout::prefix_width(&l.char_w, 0, ib);
                    let col = if is_current { app.theme.search_current } else { app.theme.search_highlight };
                    paint::fill_rect(hdc, &RECT { left: x0, top: row_y, right: x1.max(x0 + 1), bottom: row_y + row_h }, col);
                }

                // 文本选择高亮
                if let Some(sel) = app.selection {
                    let (sl, sc, el, ec) = sel.normalized();
                    if l.line >= sl && l.line <= el {
                        let c_a = if l.line == sl { sc } else { 0 };
                        let c_b = if l.line == el { ec } else { l.char_w.len() };
                        let c_a = c_a.min(l.char_w.len());
                        let c_b = c_b.max(c_a).min(l.char_w.len());
                        if c_b > row.start && c_a < row.end {
                            let ia = c_a.max(row.start);
                            let ib = c_b.min(row.end);
                            let x0 = content_x + layout::prefix_width(&l.char_w, 0, ia);
                            let x1 = content_x + layout::prefix_width(&l.char_w, 0, ib);
                            paint::fill_rect(hdc, &RECT { left: x0, top: row_y, right: x1.max(x0 + 1), bottom: row_y + row_h }, app.theme.selection_bg);
                        }
                    }
                }

                // 空白字符标记
                if show_ws && row.end > row.start {
                    let mut k = row.start;
                    while k < row.end {
                        let c = line_units[k];
                        let cw = l.char_w[k].max(1);
                        let cx = content_x + layout::prefix_width(&l.char_w, 0, k);
                        if c == ' ' as u16 {
                            paint::fill_rect(hdc, &RECT { left: cx + cw / 2 - 1, top: row_y + row_h / 2 - 1, right: cx + cw / 2 + 1, bottom: row_y + row_h / 2 + 1 }, app.theme.whitespace_marker);
                        } else if c == '\t' as u16 {
                            let arrow = crate::app::str_wide("→");
                            paint::draw_text_c(hdc, cx, row_y, app.theme.whitespace_marker, &arrow);
                        }
                        k += 1;
                    }
                }

                // 行号（仅逻辑行首个可视子行）
                if show_line_nums && ri == 0 {
                    let num = format!("{:>6} │", l.line + 1);
                    let nw = crate::app::str_wide(&num);
                    paint::draw_text_clipped_c(hdc, rect.left + 4, row_y, gutter_w - 8, app.theme.line_number, &nw);
                }

                // 文本（复用行 UTF-16，切片 + \0）
                if row.end > row.start {
                    app.utf16_scratch.clear();
                    app.utf16_scratch.extend_from_slice(&line_units[row.start..row.end]);
                    app.utf16_scratch.push(0);
                    paint::draw_text_clipped_c(hdc, content_x, row_y, text_right - content_x, text_color, &app.utf16_scratch);
                }
            }
        }

        // 无文件提示
        if app.bridge.is_none() {
            let msg = crate::app::str_wide("按 Ctrl+O 打开日志文件");
            paint::draw_text_c(hdc, rect.left + gutter_w + 12, rect.top + 8, app.theme.text_secondary, &msg);
        }

        // 横向滚动范围（非 wrap 时），并夹取 h_scroll
        if !word_wrap {
            let max_h = (longest_px - content_w).max(0) as i64;
            app.scroll.max_content_w = longest_px as f64;
            app.scroll.max_h_scroll_px = max_h as f64;
            if app.scroll.h_scroll > max_h {
                app.scroll.h_scroll = max_h;
            }
        }

        // 滚动条
        let has_h_scroll = !word_wrap && app.scroll.max_h_scroll_px > 0.0;
        let vsb = RECT { left: text_right, top: rect.top, right: rect.left + w, bottom: rect.top + h };
        paint_v_scrollbar(hdc, &vsb, app, total as f64 * wrap_factor);
        if has_h_scroll {
            let hsb = RECT { left: rect.left, top: rect.top + h - 14, right: text_right, bottom: rect.top + h };
            paint_h_scrollbar(hdc, &hsb, app);
        } else {
            app.hsb_thumb = std::mem::zeroed();
        }

        paint::restore_font(hdc, old_font);
    }
}

fn paint_v_scrollbar(hdc: *mut c_void, rect: &RECT, app: &mut App, total_rows: f64) {
    unsafe {
        app.vsb_track = *rect;
        paint::fill_rect(hdc, rect, app.theme.scrollbar_track);
        if total_rows <= 1.0 {
            app.vsb_thumb = std::mem::zeroed();
            return;
        }
        let h = (rect.bottom - rect.top) as f64;
        let total_h = total_rows * (app.row_h as f64);
        if total_h <= h {
            app.vsb_thumb = std::mem::zeroed();
            return;
        }
        let thumb_h = ((h / total_h) * h).max(24.0) as i32;
        let max_scroll = (total_h - h).max(1.0);
        let frac = (app.scroll.y as f64 * app.row_h as f64) / max_scroll;
        let thumb_top = rect.top + (frac * (h - thumb_h as f64)) as i32;
        let thumb = RECT {
            left: rect.left + 2,
            top: thumb_top.max(rect.top + 1),
            right: rect.right - 2,
            bottom: (thumb_top + thumb_h).min(rect.bottom - 1),
        };
        app.vsb_thumb = thumb;
        let col = if app.scroll.thumb_dragging { app.theme.scrollbar_hover } else { app.theme.scrollbar_thumb };
        paint::fill_rect(hdc, &thumb, col);
    }
}

fn paint_h_scrollbar(hdc: *mut c_void, rect: &RECT, app: &mut App) {
    unsafe {
        app.hsb_track = *rect;
        paint::fill_rect(hdc, rect, app.theme.scrollbar_track);
        let w = (rect.right - rect.left) as f64;
        let total_w = app.scroll.max_content_w.max(w);
        if total_w <= w {
            app.hsb_thumb = std::mem::zeroed();
            return;
        }
        let thumb_w = ((w / total_w) * w).max(28.0) as i32;
        let max_scroll = (total_w - w).max(1.0);
        let frac = (app.scroll.h_scroll as f64) / max_scroll;
        let thumb_x = rect.left + (frac * (w - thumb_w as f64)) as i32;
        let thumb = RECT {
            left: thumb_x.max(rect.left + 2),
            top: rect.top + 2,
            right: (thumb_x + thumb_w).min(rect.right - 2),
            bottom: rect.bottom - 2,
        };
        app.hsb_thumb = thumb;
        paint::fill_rect(hdc, &thumb, app.theme.scrollbar_thumb);
    }
}

/// 字体缓存管理。
pub fn ensure_font(app: &mut App) -> HFONT {
    let target = if app.metrics.font_size_px <= 0 { 13 } else { app.metrics.font_size_px };
    if app.metrics.font_size_cached != target || app.metrics.font.is_none() {
        if let Some(h) = app.metrics.font {
            unsafe { DeleteObject(h as *mut c_void); }
        }
        let h = paint::create_font(target, "Consolas");
        app.metrics.font = Some(h);
        app.metrics.font_size_cached = target;
    }
    app.metrics.font.unwrap()
}

pub fn destroy_backbuf(app: &mut App) {
    unsafe { app.destroy_full_backbuf(); }
}

/// 像素 → (逻辑行, UTF-16 列)，供鼠标命中测试（选择 / 右键）。
pub fn pixel_to_line_col(x: i32, y: i32, app: &App) -> (u64, usize) {
    let v = &app.view;
    let row_h = v.row_h.max(1);
    let content_x = v.content_x;
    for l in &v.laid {
        if y >= l.y_top && y < l.y_top + l.line_h {
            let ri = ((y - l.y_top) / row_h).max(0) as usize;
            let ri = ri.min(l.rows.len().saturating_sub(1));
            let row = l.rows[ri];
            let xr = (x - content_x).max(0) as i32;
            // 在该行宽度内定位字符
            let mut acc = 0i32;
            let mut col = row.start;
            for k in row.start..row.end {
                let cw = l.char_w.get(k).copied().unwrap_or(8).max(1);
                if acc + cw / 2 > xr {
                    break;
                }
                acc += cw;
                col = k + 1;
            }
            col = col.min(row.end).max(row.start);
            return (l.line, col);
        }
    }
    // 视口外：按首/末行夹取
    if app.view.laid.is_empty() {
        (0, 0)
    } else if y < app.view.laid[0].y_top {
        (app.view.laid[0].line, 0)
    } else {
        let l = app.view.laid.last().unwrap();
        (l.line, l.char_w.len())
    }
}
