//! CoreText 助手：字体（含 CJK 级联回退）、attributed line、度量、绘制。

use std::ptr;

use objc2_core_foundation::CFRetained;
use objc2_core_foundation::{
    CFArray, CFDictionary, CFMutableAttributedString, CFNumber, CFString, CFRange, CGPoint,
};
use objc2_core_graphics::{CGAffineTransformIdentity, CGContext, CGMutablePath};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use objc2_core_text::{
    kCTBackgroundColorAttributeName, kCTFontAttributeName,
    kCTForegroundColorAttributeName, kCTFontCascadeListAttribute, kCTFontNameAttribute,
    kCTLigatureAttributeName, CTFont, CTFontDescriptor, CTLine, CTTypesetter,
};

use crate::theme::Rgba;

/// 主字体族候选（按序回退）。
const FONT_CANDIDATES: &[&str] = &["Menlo", "SF Mono", "Monaco"];
/// CJK 级联回退字体。
const CJK_FALLBACKS: &[&str] = &[
    "PingFang SC",
    "Heiti SC",
    "Hiragino Sans GB",
    "Arial Unicode MS",
    "Apple Color Emoji",
];

/// 缓存好的字体与度量。
pub struct Font {
    ct_font: CFRetained<CTFont>,
    pub ascent: f64,
    pub descent: f64,
    pub leading: f64,
}

impl Font {
    /// 创建带 CJK 级联回退的字体。`family` 指定首选字体族（设置面板），
    /// `None` 则用默认候选族 `FONT_CANDIDATES`。
    pub fn with_family(family: Option<&str>, size: f64) -> Self {
        let base = match family.filter(|f| !f.is_empty()) {
            Some(f) => make_font(std::slice::from_ref(&f), size),
            None => make_font(FONT_CANDIDATES, size),
        };
        let font = add_cjk_cascade(base, size);
        let ascent = unsafe { font.ascent() };
        let descent = unsafe { font.descent() };
        let leading = unsafe { font.leading() };
        Self {
            ascent,
            descent,
            leading,
            ct_font: font,
        }
    }

    /// 行高（px）：真实字体度量（ascent+descent+leading），至少 14px。
    pub fn line_height(&self) -> f64 {
        (self.ascent + self.descent + self.leading).ceil().max(14.0)
    }

    pub fn as_ctfont(&self) -> &CTFont {
        &self.ct_font
    }

    /// 测量文本宽度（不换行 CTLine）。
    pub fn measure_width(&self, text: &str) -> f64 {
        line_width_plain(text, &self.ct_font)
    }

    /// 构建一个带高亮的 CTLine 并绘制到上下文 (x, y)。
    ///
    /// - `text_color`：整行前景色（级别色或默认）。
    /// - `matches`：行内匹配的 UTF-8 字节区间。
    /// - `current_matches`：当前命中所在的区间。
    /// - `highlight` / `current`：普通命中 / 当前命中的高亮色。
    /// - `bg_color`：整行背景（如交替行），`None` 表示不画。
    /// - `show_whitespace`：把空格/制表符替换为可视字符。
    #[allow(clippy::too_many_arguments)]
    pub fn draw_line(
        &self,
        ctx: &CGContext,
        x: f64,
        y: f64,
        text: &str,
        text_color: &Rgba,
        bg_color: Option<&Rgba>,
        matches: &[(usize, usize)],
        current_matches: &[(usize, usize)],
        highlight: &Rgba,
        current: &Rgba,
        show_whitespace: bool,
    ) {
        if text.is_empty() {
            return;
        }
        let display = if show_whitespace {
            replace_whitespace(text)
        } else {
            text.to_string()
        };
        let (attr, _total) = self.build_attributed(&display, text_color, bg_color);
        unsafe {
            let line = CTLine::with_attributed_string(&attr);
            let matches_u16 = crate::util::byte_ranges_to_utf16(&display, matches);
            let current_u16 = crate::util::byte_ranges_to_utf16(&display, current_matches);
            let hf = highlight.with_alpha(MATCH_FILL_ALPHA);
            let cf = current.with_alpha(CURRENT_FILL_ALPHA);
            draw_match_rects(ctx, &line, x, y, &matches_u16, &current_u16, &hf, &cf);
            draw_ctline_flipped(ctx, &line, x, y);
        }
    }

    /// 某行文本在 `width` 下会折成几段（可视行数）。空行/极窄宽度按 1 行。
    pub fn visual_rows(&self, text: &str, width: f64) -> usize {
        if text.is_empty() || width <= 1.0 {
            return 1;
        }
        unsafe {
            let attr = make_attr_string(text);
            set_font_ligature_attr(&attr, &self.ct_font, text);
            let total = text.encode_utf16().count() as isize;
            let segments = wrap_attr_into_segments(&attr, total, width);
            segments.len().max(1)
        }
    }

    /// 把一行按 `width` 折成 CTLine 分段（供命中测试/选择复用）。
    ///
    /// 返回 (attributed string, 分段列表)。`attr` 只含字体+连字属性，与绘制
    /// 用的 build_attributed 布局一致（等宽字体，连字关闭）。
    pub fn wrapped_segments(
        &self,
        text: &str,
        width: f64,
    ) -> Option<(CFRetained<CFMutableAttributedString>, Vec<(CFRange, CFRetained<CTLine>)>)> {
        if text.is_empty() || width <= 1.0 {
            return None;
        }
        unsafe {
            let attr = make_attr_string(text);
            set_font_ligature_attr(&attr, &self.ct_font, text);
            let total = text.encode_utf16().count() as isize;
            let segments = wrap_attr_into_segments(&attr, total, width);
            if segments.is_empty() {
                return None;
            }
            Some((attr, segments))
        }
    }

    /// 自动换行绘制：用 CTTypesetter 把长行按 `width` 断行，逐段画在 (x, y) 往下。
    /// 段间垂直间距 = `spacing`（通常为 config.row_h，≥ line_height）。
    ///
    /// 返回实际画出的可视行数（供上层估高）。
    #[allow(clippy::too_many_arguments)]
    pub fn draw_line_wrapped(
        &self,
        ctx: &CGContext,
        x: f64,
        y: f64,
        text: &str,
        text_color: &Rgba,
        bg_color: Option<&Rgba>,
        matches: &[(usize, usize)],
        current_matches: &[(usize, usize)],
        highlight: &Rgba,
        current: &Rgba,
        show_whitespace: bool,
        width: f64,
        spacing: f64,
    ) -> usize {
        if text.is_empty() || width <= 1.0 {
            return 0;
        }
        let display = if show_whitespace {
            replace_whitespace(text)
        } else {
            text.to_string()
        };
        let (attr, total) = self.build_attributed(&display, text_color, bg_color);
        let matches_u16 = crate::util::byte_ranges_to_utf16(&display, matches);
        let current_u16 = crate::util::byte_ranges_to_utf16(&display, current_matches);
        let hf = highlight.with_alpha(MATCH_FILL_ALPHA);
        let cf = current.with_alpha(CURRENT_FILL_ALPHA);
        unsafe {
            let segments = wrap_attr_into_segments(&attr, total, width);
            let n = segments.len();
            for (i, (_, ctline)) in segments.iter().enumerate() {
                let seg_y = y + i as f64 * spacing;
                draw_match_rects(ctx, ctline, x, seg_y, &matches_u16, &current_u16, &hf, &cf);
                draw_ctline_flipped(ctx, ctline, x, seg_y);
            }
            n
        }
    }

    /// 构造带字体 / 前景色 / 可选整行背景的 attributed string（UTF-16 索引）。
    ///
    /// 匹配高亮不再以 `kCTBackgroundColorAttributeName` 塞进 attributed string
    /// （那是不透明的硬色块），改由绘制路径在文字下方画半透明圆角矩形。
    fn build_attributed(
        &self,
        display: &str,
        text_color: &Rgba,
        bg_color: Option<&Rgba>,
    ) -> (CFRetained<CFMutableAttributedString>, isize) {
        unsafe {
            let attr = make_attr_string(display);
            // CFRange 用 UTF-16 码元计数（CFString 的 length），不能用字节数。
            let total = display.encode_utf16().count() as isize;
            let set = CFMutableAttributedString::set_attribute;
            let font_color = text_color.to_cgcolor();
            set(
                Some(&attr),
                CFRange { location: 0, length: total },
                Some(kCTFontAttributeName),
                Some(&self.ct_font),
            );
            set(
                Some(&attr),
                CFRange { location: 0, length: total },
                Some(kCTForegroundColorAttributeName),
                Some(&font_color),
            );
            // 关闭连字：等宽日志对齐需要逐字 1:1 宽度
            let ligature = CFNumber::new_i32(0);
            set(
                Some(&attr),
                CFRange { location: 0, length: total },
                Some(kCTLigatureAttributeName),
                Some(&ligature),
            );
            if let Some(bg) = bg_color {
                let bgc = bg.to_cgcolor();
                set(
                    Some(&attr),
                    CFRange { location: 0, length: total },
                    Some(kCTBackgroundColorAttributeName),
                    Some(&bgc),
                );
            }
            (attr, total)
        }
    }

    /// 绘制一行文本的选区背景（半透明），**在文字之前**调用。
    ///
    /// `sel_u16` 是 UTF-16 命中区间（相对 `text` 的整段字符串），用与渲染
    /// 一致的折行（等宽、禁连字）分段定位，保证换行模式下选区不跨视觉行。
    /// `width` 用 `content_avail_width`（换行）或极大值（不换行）。
    pub fn draw_selection_rects(
        &self,
        ctx: &CGContext,
        x: f64,
        y: f64,
        text: &str,
        sel_u16: &[(usize, usize)],
        color: &Rgba,
        width: f64,
        spacing: f64,
    ) {
        if sel_u16.is_empty() || text.is_empty() || width <= 1.0 {
            return;
        }
        unsafe {
            let attr = make_attr_string(text);
            set_font_ligature_attr(&attr, &self.ct_font, text);
            let total = text.encode_utf16().count() as isize;
            let segments = wrap_attr_into_segments(&attr, total, width);
            for (i, (_, ctline)) in segments.iter().enumerate() {
                let seg_y = y + i as f64 * spacing;
                draw_match_rects(ctx, ctline, x, seg_y, sel_u16, &[], color, color);
            }
        }
    }
}

/// 命中高亮的圆角半径（px）。
const MATCH_RADIUS: f64 = 3.0;
/// 普通命中填充 alpha（≈0.38）。
const MATCH_FILL_ALPHA: u8 = 97;
/// 当前命中填充 alpha（≈0.50）。
const CURRENT_FILL_ALPHA: u8 = 128;

/// 在一行（可能是折行后的一段）上绘制高亮矩形，**在文字之前**调用。
///
/// - `line`：该视觉行对应的 CTLine；`x/y` 是它基线的绘制原点（flipped 坐标）。
/// - `matches_u16` / `current_u16`：整段字符串的 UTF-16 命中区间（普通 / 当前）。
/// - `highlight_fill` / `current_fill`：已带 alpha 的填充色（调用方预调好透明度）；
///   当前命中额外画 1px 描边。
///   矩形用 `CTLine::offset_for_string_index` 定位，确保与字形精确对齐、
///   不跨视觉行（换行模式下每段单独处理）。
fn draw_match_rects(
    ctx: &CGContext,
    line: &CTLine,
    x: f64,
    y: f64,
    matches_u16: &[(usize, usize)],
    current_u16: &[(usize, usize)],
    highlight_fill: &Rgba,
    current_fill: &Rgba,
) {
    if matches_u16.is_empty() && current_u16.is_empty() {
        return;
    }
    let range = unsafe { line.string_range() };
    let loc = range.location.max(0) as usize;
    let len = range.length.max(0) as usize;
    if len == 0 {
        return;
    }
    // 行高：取该行的 typographic bounds（与 draw_ctline_flipped 一致）
    let (mut ascent, mut descent) = (0.0f64, 0.0f64);
    unsafe {
        line.typographic_bounds(&mut ascent, &mut descent, ptr::null_mut());
    }
    let h = (ascent + descent).max(8.0);

    let mut normal: Vec<NSRect> = Vec::new();
    let mut cur: Vec<NSRect> = Vec::new();
    for &(s, e) in matches_u16 {
        collect_match_rect(line, s, e, loc, len, x, y, h, &mut normal);
    }
    for &(s, e) in current_u16 {
        collect_match_rect(line, s, e, loc, len, x, y, h, &mut cur);
    }

    if !normal.is_empty() {
        fill_rounded_rects(ctx, &normal, highlight_fill, 0.0);
    }
    if !cur.is_empty() {
        // 当前命中：先描边（略扩大）再填充，形成 1px 外框
        fill_rounded_rects(ctx, &cur, current_fill, 1.0);
    }
}

/// 计算一个 UTF-16 命中区间在该 CTLine 上的重叠矩形，追加到 `out`。
/// `loc/len` 是该行在整段字符串中的起止（UTF-16 码元），`offset_for_string_index`
/// 期望的是相对行首的索引，因此要减去 `loc`。
fn collect_match_rect(
    line: &CTLine,
    s: usize,
    e: usize,
    loc: usize,
    len: usize,
    x: f64,
    y: f64,
    h: f64,
    out: &mut Vec<NSRect>,
) {
    let ov_s = s.max(loc);
    let ov_e = e.min(loc + len);
    if ov_e <= ov_s {
        return;
    }
    let ls = ov_s - loc;
    let le = ov_e - loc;
    let (x0, x1) = unsafe {
        (
            line.offset_for_string_index(ls as isize, ptr::null_mut()),
            line.offset_for_string_index(le as isize, ptr::null_mut()),
        )
    };
    let w = (x1 - x0).max(1.0);
    out.push(NSRect {
        origin: NSPoint::new(x + x0, y - h),
        size: NSSize::new(w, h),
    });
}

/// 用 `color` 填充一组圆角矩形；`inset`>0 时先画一圈描边（当前命中外框）。
fn fill_rounded_rects(ctx: &CGContext, rects: &[NSRect], color: &Rgba, inset: f64) {
    if rects.is_empty() {
        return;
    }
    let cg = color.to_cgcolor();
    if inset > 0.0 {
        let stroke = color.with_alpha(210).to_cgcolor();
        unsafe {
            let path = CGMutablePath::new();
            for &r in rects {
                let rr = NSRect {
                    origin: NSPoint::new(r.origin.x - inset, r.origin.y - inset),
                    size: NSSize::new(r.size.width + inset * 2.0, r.size.height + inset * 2.0),
                };
                CGMutablePath::add_rounded_rect(
                    Some(&path),
                    ptr::null_mut(),
                    rr,
                    MATCH_RADIUS + inset,
                    MATCH_RADIUS + inset,
                );
            }
            CGContext::set_stroke_color_with_color(Some(ctx), Some(&stroke));
            CGContext::set_line_width(Some(ctx), 1.0);
            CGContext::add_path(Some(ctx), Some(&path));
            CGContext::stroke_path(Some(ctx));
        }
    }
    unsafe {
        let path = CGMutablePath::new();
        for &r in rects {
            CGMutablePath::add_rounded_rect(
                Some(&path),
                ptr::null_mut(),
                r,
                MATCH_RADIUS,
                MATCH_RADIUS,
            );
        }
        CGContext::set_fill_color_with_color(Some(ctx), Some(&cg));
        CGContext::add_path(Some(ctx), Some(&path));
        CGContext::fill_path(Some(ctx));
    }
}

/// 命中测试：把水平位置 `x`（相对该 CTLine 原点的距离）换算成整段字符串的
/// UTF-16 码元索引。越界 clamp 到该行的 `[loc, loc+len]`。
pub unsafe fn point_to_offset(line: &CTLine, x: f64) -> usize {
    let range = line.string_range();
    let loc = range.location.max(0) as usize;
    let len = range.length.max(0) as usize;
    let idx = line.string_index_for_position(CGPoint::new(x, 0.0));
    if idx < 0 {
        return loc;
    }
    (idx as usize).clamp(loc, loc + len)
}

/// UTF-16 码元索引 → UTF-8 字节偏移（CoreText 命中测试用，clamp 到文本末尾）。
pub fn utf16_to_byte(text: &str, u16_idx: usize) -> usize {
    let mut byte = 0;
    let mut u16 = 0;
    for c in text.chars() {
        if u16 >= u16_idx {
            break;
        }
        byte += c.len_utf8();
        u16 += c.len_utf16();
    }
    byte
}

#[cfg(test)]
mod tests {
    use super::utf16_to_byte;

    #[test]
    fn ascii_maps_one_to_one() {
        assert_eq!(utf16_to_byte("hello", 0), 0);
        assert_eq!(utf16_to_byte("hello", 2), 2);
        assert_eq!(utf16_to_byte("hello", 5), 5);
        assert_eq!(utf16_to_byte("hello", 99), 5); // 越界 clamp
    }

    #[test]
    fn bmp_multibyte_counts_two_bytes_per_char() {
        // "你" = U+4F60，UTF-8 3 字节、UTF-16 1 码元
        assert_eq!(utf16_to_byte("你好", 1), 3);
        assert_eq!(utf16_to_byte("你好", 2), 6);
    }

    #[test]
    fn astral_char_counts_two_u16_units() {
        // 😀 = U+1F600，UTF-8 4 字节、UTF-16 2 码元（代理对）
        let s = "a😀b";
        assert_eq!(utf16_to_byte(s, 0), 0);
        assert_eq!(utf16_to_byte(s, 1), 1);
        assert_eq!(utf16_to_byte(s, 3), 5); // 两个码元都在表情内 → 跳到最后
        assert_eq!(utf16_to_byte(s, 4), 6);
    }
}

/// 绘制简单文本（无高亮），用于行号栏、状态栏等。
pub fn draw_string(
    ctx: &CGContext,
    font: &Font,
    text: &str,
    x: f64,
    y: f64,
    color: &Rgba,
) {
    if text.is_empty() {
        return;
    }
    unsafe {
        let attr = make_attr_string(text);
        let total = text.encode_utf16().count() as isize;
        let fg = color.to_cgcolor();
        CFMutableAttributedString::set_attribute(
            Some(&attr),
            CFRange { location: 0, length: total },
            Some(kCTFontAttributeName),
            Some(font.as_ctfont()),
        );
        CFMutableAttributedString::set_attribute(
            Some(&attr),
            CFRange { location: 0, length: total },
            Some(kCTForegroundColorAttributeName),
            Some(&fg),
        );
        let line = CTLine::with_attributed_string(&attr);
        draw_ctline_flipped(ctx, &line, x, y);
    }
}

/// 在 flipped 视图的上下文中绘制 CTLine：基线位于 (x, y)。
///
/// AppKit 给 flipped 视图的 CGContext 做了上下翻转（原点在左上、y 向下），
/// 而 CoreText 按"原点在左下、y 向上"设计，直接画会倒置。标准解法：
/// 保存状态 → translate 到目标点 → scale(1,-1) → 画 → 恢复。
fn draw_ctline_flipped(ctx: &CGContext, line: &CTLine, x: f64, y: f64) {
    unsafe {
        CGContext::save_g_state(Some(ctx));
        CGContext::set_text_matrix(Some(ctx), CGAffineTransformIdentity);
        CGContext::translate_ctm(Some(ctx), x, y);
        CGContext::scale_ctm(Some(ctx), 1.0, -1.0);
        line.draw(ctx);
        CGContext::restore_g_state(Some(ctx));
    }
}

/// 不换行 CTLine 的宽度。
fn line_width_plain(text: &str, font: &CTFont) -> f64 {
    unsafe {
        let attr = make_attr_string(text);
        let total = text.encode_utf16().count() as isize;
        CFMutableAttributedString::set_attribute(
            Some(&attr),
            CFRange { location: 0, length: total },
            Some(kCTFontAttributeName),
            Some(font),
        );
        let line = CTLine::with_attributed_string(&attr);
        line.typographic_bounds(ptr::null_mut(), ptr::null_mut(), ptr::null_mut())
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// 创建内容为 `text` 的可变 attributed string。
///
/// 必须先填内容再设属性——在一个空串上调用 `set_attribute` 会因 range 越界
/// 而崩溃（CFAttributedStringSetAttribute 对越界 range 是未定义行为）。
unsafe fn make_attr_string(text: &str) -> CFRetained<CFMutableAttributedString> {
    let attr = CFMutableAttributedString::new(None, 0).unwrap();
    CFMutableAttributedString::replace_string(
        Some(&attr),
        CFRange { location: 0, length: 0 },
        Some(&CFString::from_str(text)),
    );
    attr
}

/// 构建一个仅含字体 + 禁连字属性的 attributed string（命中测试用，无颜色）。
pub fn plain_attr(text: &str, font: &CTFont) -> CFRetained<CFMutableAttributedString> {
    unsafe {
        let attr = make_attr_string(text);
        set_font_ligature_attr(&attr, font, text);
        attr
    }
}

/// 给整段设置字体 + 关闭连字（测量/命中测试用，无颜色）。
unsafe fn set_font_ligature_attr(attr: &CFMutableAttributedString, font: &CTFont, text: &str) {
    let total = text.encode_utf16().count() as isize;
    let set = CFMutableAttributedString::set_attribute;
    set(
        Some(attr),
        CFRange { location: 0, length: total },
        Some(kCTFontAttributeName),
        Some(font),
    );
    let ligature = CFNumber::new_i32(0);
    set(
        Some(attr),
        CFRange { location: 0, length: total },
        Some(kCTLigatureAttributeName),
        Some(&ligature),
    );
}

/// 用 CTTypesetter 把整段按 `width` 折成分段（CFRange + CTLine）。
/// 极端窄宽度下无断点 → 强制整段，避免死循环；上限 1000 段。
unsafe fn wrap_attr_into_segments(
    attr: &CFMutableAttributedString,
    total: isize,
    width: f64,
) -> Vec<(CFRange, CFRetained<CTLine>)> {
    let typesetter = CTTypesetter::with_attributed_string(attr);
    let mut segments: Vec<(CFRange, CFRetained<CTLine>)> = Vec::new();
    let mut start: isize = 0;
    while start < total {
        let mut brk = typesetter.suggest_line_break(start, width);
        if brk <= start {
            brk = total;
        }
        brk = brk.min(total);
        let range = CFRange { location: start, length: brk - start };
        let ctline = typesetter.line(range);
        segments.push((range, ctline));
        start = brk;
        if segments.len() > 1000 {
            break;
        }
    }
    segments
}

/// 尝试用候选字体名创建字体，全部失败则用 Menlo。
///
/// `CTFontCreateWithName` 永不返回 NULL（不存在的名字会回退到默认字体），
/// 因此用 postscript 名或族名核对是否真的取到了请求的字体（如 "Menlo" 的
/// postscript 名是 "Menlo-Regular"，但族名是 "Menlo"）。
fn make_font(candidates: &[&str], size: f64) -> CFRetained<CTFont> {
    for name in candidates {
        let cf = CFString::from_str(name);
        let f = unsafe { CTFont::with_name(&cf, size, ptr::null()) };
        let ps = unsafe { f.post_script_name().to_string() };
        let fam = unsafe { f.family_name().to_string() };
        if ps.eq_ignore_ascii_case(name) || fam.eq_ignore_ascii_case(name) {
            return f;
        }
    }
    unsafe { CTFont::with_name(&CFString::from_str("Menlo"), size, ptr::null()) }
}

/// 给字体追加 CJK 级联回退列表。
fn add_cjk_cascade(base: CFRetained<CTFont>, size: f64) -> CFRetained<CTFont> {
    unsafe {
        let base_desc = base.font_descriptor();
        // 构造级联描述符数组
        let descriptors: Vec<CFRetained<CTFontDescriptor>> = CJK_FALLBACKS
            .iter()
            .map(|n| {
                let name = CFString::from_str(n);
                let dict = CFDictionary::from_slices(&[&*kCTFontNameAttribute], &[&*name]);
                CTFontDescriptor::with_attributes(dict.as_ref())
            })
            .collect();
        let refs: Vec<&CTFontDescriptor> = descriptors.iter().map(|d| &**d).collect();
        let cascade_array = CFArray::from_objects(&refs);
        let attrs =
            CFDictionary::from_slices(&[&*kCTFontCascadeListAttribute], &[&*cascade_array]);
        let new_desc = base_desc.copy_with_attributes(attrs.as_ref());
        CTFont::with_font_descriptor(&new_desc, size, ptr::null())
    }
}

/// 空白替换：' '→'·'，'\t'→'→'。
fn replace_whitespace(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ' ' => '·',
            '\t' => '→',
            other => other,
        })
        .collect()
}
