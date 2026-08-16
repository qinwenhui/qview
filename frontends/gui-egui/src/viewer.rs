//! Main viewer — virtual-scrolling log display with custom scrollbars,
//! search highlighting, word-wrap support, and log-level colouring.
//!
//! Text is painted directly (no `Label` widgets) so there is no
//! viewport-bound text selection — what you see is tied to the actual
//! log line, not the pixel position.

/// 搜索匹配高亮上限：单行 / 可见窗口内最多高亮多少个命中区间。
/// 巨行 + 常见模式会产出上百万命中，若全部 append 进 TextLayoutJob 会生成
/// 数百 MB 的 galley（且被 egui 缓存）→ 内存暴涨（用户实测搜索后飙到 2G）。
const MAX_MATCH_RANGES: usize = 10_000;

// 超长行阈值 / 视觉行模型 / 字符度量等统一来自 `crate::layout`（排版度量模块）——
// 格子系统：字节 / 字符 / 视觉列 / 视觉行换算只走 layout，不在这里各自实现。

use std::sync::Arc;

use egui::{pos2, vec2, Color32, Context, FontId, Rect, RichText, Sense};
use egui::text::{LayoutJob, TextFormat};

use qview_core::search::Query;
use qview_application::protocol::view_intent::{FilterSpec, HighlightKind};

use crate::layout::{CharMetrics, HugeLayout, VisualRowModel, CHUNK_LINE_BYTES};

/// Match ranges `(start, end)` of the parsed search query within `hay`, so
/// highlighting agrees with what the engine actually matched (regex matches
/// have variable length; literals are just memmem occurrences).
fn query_ranges_in<'a>(
    q: &'a Query,
    hay: &'a [u8],
) -> Box<dyn Iterator<Item = (usize, usize)> + 'a> {
    match q {
        Query::Literal(p) => {
            Box::new(memchr::memmem::find_iter(hay, p).map(move |m| (m, m + p.len())))
        }
        Query::Regex(re) => Box::new(re.find_iter(hay).map(move |m| (m.start(), m.end()))),
    }
}

// (旧 `append_search_job` 已删——per-row layout 后每行自己扫搜索区间 append)

/// Agent 视图过滤器：行是否命中（架构 §9 ApplyFilter 的可见投影）。
/// 无分配（ASCII 大小写不敏感用字节窗口比较）。
fn filter_matches_line(f: &FilterSpec, text: &str) -> bool {
    match f {
        FilterSpec::Literal { pattern, case_sensitive } => {
            if pattern.is_empty() {
                return true;
            }
            if *case_sensitive {
                text.contains(pattern.as_str())
            } else {
                text.as_bytes()
                    .windows(pattern.len())
                    .any(|w| w.eq_ignore_ascii_case(pattern.as_bytes()))
            }
        }
        FilterSpec::Contains { needle } => {
            if needle.is_empty() {
                return true;
            }
            text.contains(needle.as_str())
        }
        FilterSpec::ErrorLevel { min, max } => {
            // 扫描 3 位数字（错误码），落在 [min, max] 即命中
            let bytes = text.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i].is_ascii_digit() {
                    let start = i;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    if let Ok(n) = text[start..i].parse::<u16>() {
                        if n >= *min && n <= *max {
                            return true;
                        }
                    }
                } else {
                    i += 1;
                }
            }
            false
        }
    }
}

use crate::log_debug;
use crate::app::QLogApp;
use crate::theme_data::ThemeColors;
use qview_core::engine::Engine;

/// One pre-laid-out visible line.  Stores the real galley so the paint pass
/// doesn't re-layout, and the actual Y position so hit-testing matches what's
/// rendered exactly (crucial when word-wrap makes line heights non-uniform).
///
/// 巨行分块：一条超长行会被切成多段（每段 ≤ CHUNK_BYTES），每段一个 LaidLine，
/// `chunk_char` = 该段在原始行里的**字符**起点（普通行 = 0）。点选/选区映射要
/// 加回 chunk_char 才能得到原始行的绝对字符位置。
struct LaidLine {
    line: u64,
    y_top: f32,
    height: f32,
    galley: Arc<egui::Galley>,
    /// 该段在原始行里的字符起点（普通行 = 0）。
    chunk_char: usize,
}

// ---------------------------------------------------------------------------
// 视觉行模型（物理行 ↔ 视觉行）、超长行阈值、字符度量、坐标总闸统一来自
// `crate::layout` —— 见 `layout/mod.rs` 的格子系统设计。

/// 一条超长行的缓存：文本 + 字符数 + char→byte 跳跃表 + 失效键。
///
/// 渲染 per-row layout 每帧都调 `engine.read_line` 解码整行 UTF-8——10MB 巨行
/// ~30ms/次，per-frame 调就成了滚动卡顿。`text` 把解码结果缓存下来，per-frame
/// 直接切片。`byte_index[i] = (char_pos, byte_pos)` 每 1024 字符一个采样点，
/// 用于把 chunk_char 转成 byte_pos 时跳过 `char_indices().nth()` 的 O(N) 扫描。
#[derive(Clone)]
pub struct HugeLineCache {
    pub line: u64,
    pub text: Arc<str>,
    pub char_count: usize,
    /// 该行在文件里的绝对字节偏移（用于搜索命中字节比对）。
    pub start_byte: u64,
    /// 每 BYTE_INDEX_STRIDE 字符一个采样点：[(char_pos, byte_pos)]，单调。
    pub byte_index: Arc<Vec<(usize, usize)>>,
    /// 每视觉行的元数据缓存（`HugeLayout`）：layout 时记录实际字符数；视口滚动
    /// 经过的行会缓存。用于从行开头精确累积 char_pos —— `chars_per_row` 是单字节
    /// 宽估算，含 CJK/多字节字符时每行实际字符数更少，估算起点偏大 → chunk_char /
    /// 光标位置偏大 1（用户反馈：编辑超长行插入跑到 b 后）。同时是
    /// `ViewMapping::char_to_row_col` / `row_col_to_char` 的换算基准。
    /// wrap_w / font_size 变化时（ensure 重建）整体清空。
    pub layout: HugeLayout,
    pub layout_font_size: f32,
    pub layout_wrap_w: f32,
}

/// byte_index 采样步长（每 N 字符采一个点）。1024 ≈ 「单行几百字符」范围足够。
const BYTE_INDEX_STRIDE: usize = 1024;

/// 把 char_pos 通过 byte_index + 短距离线性扫描转为 byte_pos。O(log N + stride)。
fn char_pos_to_byte_pos(byte_index: &[(usize, usize)], text: &str, char_pos: usize) -> usize {
    if char_pos == 0 { return 0; }
    // 二分找最大采样点 ≤ char_pos。注意 hi 是 inclusive 上界（len-1），配合
    // `mid = (lo+hi+1)/2` 才不越界；旧代码 hi=len 且 mid 偏上 → lo=len-1 时
    // mid=len → byte_index[len] 越界 panic（用户实测：搜索后闪退）。
    let mut lo = 0usize;
    let mut hi = byte_index.len().saturating_sub(1);
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        if byte_index[mid].0 <= char_pos { lo = mid; } else { hi = mid - 1; }
    }
    let (anchor_char, anchor_byte) = byte_index[lo];
    if anchor_char == char_pos {
        return anchor_byte;
    }
    // 从 anchor 线性扫描；char_indices() 第 N 次迭代给的是切片内第 N 个字符的偏移
    let mut count = anchor_char;
    for (rel_b, _) in text[anchor_byte..].char_indices() {
        if count == char_pos {
            return anchor_byte + rel_b;
        }
        count += 1;
    }
    text.len()
}

/// 构造 char→byte 索引：每隔 BYTE_INDEX_STRIDE 字符采一个点。
/// 采样点 `(char_count, byte_off)` 含义：「第 char_count 个字符（0-indexed）从 byte_off 开始」。
///
/// **关键**：`char_indices()` 每次迭代给的 `byte_off` 是**当前字符**的字节偏移。
/// 旧代码先 `char_count += 1` 再 `push((char_count, byte_off))`，导致采样点记录的
/// byte_off 是 chars[char_count-1] 的偏移（偏 1 字节）→ char_pos_to_byte_pos 返回
/// byte_pos-1 → row_text 从 chars[char_pos-1] 开始 → 渲染内容偏左 1 字符，col 偏大 1
/// → 高亮/选区/复制全差 1 字符（用户实测：选 version_id 复制出 ersion_id）。
fn build_byte_index(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut char_count = 0usize;
    for (byte_off, _) in text.char_indices() {
        if char_count % BYTE_INDEX_STRIDE == 0 {
            // chars[char_count] 的字节偏移 = 当前迭代的 byte_off
            out.push((char_count, byte_off));
        }
        char_count += 1;
    }
    out
}

/// 确保某超长行的缓存存在（首次建会缓存文本 + byte_index）；layout_key 失效重建。
fn ensure_huge_meta(
    cache: &mut Vec<HugeLineCache>,
    engine: &Engine,
    line: u64,
    font_size: f32,
    wrap_w: f32,
) -> Option<usize> {
    if let Some(i) = cache.iter().position(|c| c.line == line) {
        if i + 1 != cache.len() {
            let c = cache.remove(i);
            cache.push(c);
        }
        let entry = cache.last_mut().unwrap();
        if entry.layout_font_size != font_size || entry.layout_wrap_w != wrap_w {
            // 失效键变了，重读 + 重建 byte_index
            let raw = engine.read_line(line);
            if raw.text.len() <= CHUNK_LINE_BYTES {
                cache.pop();
                return None;
            }
            let text: Arc<str> = Arc::from(raw.text);
            let byte_index = Arc::new(build_byte_index(&text));
            entry.char_count = text.chars().count();
            entry.start_byte = raw.start_byte;
            entry.text = text;
            entry.byte_index = byte_index;
            entry.layout.clear();
            entry.layout_font_size = font_size;
            entry.layout_wrap_w = wrap_w;
        }
        return Some(cache.len() - 1);
    }
    let raw = engine.read_line(line);
    if raw.text.len() <= CHUNK_LINE_BYTES {
        return None;
    }
    let text: Arc<str> = Arc::from(raw.text);
    let byte_index = Arc::new(build_byte_index(&text));
    let char_count = text.chars().count();
    cache.push(HugeLineCache {
        line,
        text,
        char_count,
        start_byte: raw.start_byte,
        byte_index,
        layout: HugeLayout::new(),
        layout_font_size: font_size,
        layout_wrap_w: wrap_w,
    });
    // LRU 预算：缓存条目按字节总数（≈ 各行 text 字节）控制。
    while cache.len() > 1 {
        let sum: u64 = cache.iter().map(|c| c.text.len() as u64).sum();
        if sum <= 64 * 1024 * 1024 { break; }
        cache.remove(0);
    }
    cache.iter().position(|c| c.line == line)
}

/// 一次性 layout 单行（max_rows=1），返回该行实际放置的字符数。
/// 用于补齐 row_chars 缓存（视口前的行不需要渲染，但要知道每行实际字符数才能
/// 精确累积 char_pos —— 含 CJK 时每行字符数 < 单字节宽估算）。
fn layout_row_char_count(
    ui: &egui::Ui,
    wrap_w: f32,
    text: &str,
    content_fmt: &TextFormat,
) -> usize {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_w;
    job.wrap.break_anywhere = true;
    job.wrap.max_rows = 1;
    job.wrap.overflow_character = None;
    job.append(text, 0.0, content_fmt.clone());
    ui.fonts(|f| f.layout_job(job))
        .rows
        .first()
        .map(|r| r.glyphs.len())
        .unwrap_or(0)
}

/// Scroll speed when dragging a selection past a viewport edge.  Per-frame
/// delta = RATE × distance beyond the edge (points).  Kept as a fixed per-frame
/// amount so the speed tracks how far the pointer overshoots.
const AUTO_SCROLL_RATE: f64 = 0.2;

/// Measure the actual monospace character width from the current font.
/// Uses a string of 10 digits and divides by 10 for an accurate per-char width.
fn measure_char_width(ui: &egui::Ui, font: &FontId) -> f32 {
    ui.fonts(|f| {
        let galley = f.layout_no_wrap(
            "0000000000".to_string(),
            font.clone(),
            Color32::WHITE,
        );
        galley.rect.width() / 10.0
    })
}

// ---------------------------------------------------------------------------
// public helpers
// ---------------------------------------------------------------------------

pub fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut n = n as f64;
    let mut i = 0;
    while n >= 1024.0 && i < UNITS.len() - 1 {
        n /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", n as u64, UNITS[i])
    } else {
        format!("{:.2} {}", n, UNITS[i])
    }
}

/// Return a per-line colour if a recognised log level marker is found.
/// Matches bracketed (`[ERROR]`) and non-bracketed (`ERROR:`, ` ERROR `)
/// patterns so common log formats work out of the box.
pub fn level_color(text: &str, theme: &ThemeColors) -> Option<Color32> {
    let upper = text.to_uppercase();
    let levels: &[(&str, Color32)] = &[
        ("ERROR", theme.level_error),
        ("FATAL", theme.level_error),
        ("CRIT",  theme.level_error),
        ("WARN",  theme.level_warn),
        ("WARNING", theme.level_warn),
        ("INFO",  theme.level_info),
        ("NOTICE", theme.level_info),
        ("DEBUG", theme.level_debug),
        ("TRACE", theme.level_trace),
    ];
    for (word, color) in levels {
        if has_level(&upper, word) {
            return Some(*color);
        }
    }
    None
}

/// Check whether `level` appears as a log-level marker in `text`.
/// Matches: `[LEVEL]`, `LEVEL:`, `"LEVEL"`, ` LEVEL `, or line starts with it.
fn has_level(upper: &str, level: &str) -> bool {
    // [LEVEL]
    let bracketed = format!("[{}]", level);
    if upper.contains(&bracketed) {
        return true;
    }
    // LEVEL:
    let colon = format!("{}:", level);
    if upper.contains(&colon) {
        return true;
    }
    // "LEVEL"
    let quoted = format!("\"{}\"", level);
    if upper.contains(&quoted) {
        return true;
    }
    // <LEVEL> (XML-style)
    let xml = format!("<{}>", level);
    if upper.contains(&xml) {
        return true;
    }
    // LEVEL surrounded by whitespace or at BOL/EOL
    let word = format!(" {} ", level);
    if upper.contains(&word) {
        return true;
    }
    if upper.starts_with(&format!("{} ", level)) {
        return true;
    }
    if upper.ends_with(&format!(" {}", level)) {
        return true;
    }
    false
}

// (旧 `pixel_to_char` / `text_pixel_width` 已删——度量统一走 `layout::CharMetrics`
// 的 `x_to_char` / `char_to_x` / `text_w`，不再各自 layout 测宽)

// ---------------------------------------------------------------------------
// render
// ---------------------------------------------------------------------------

/// Render the central log-view area.
pub fn render_central_panel(ctx: &Context, app: &mut QLogApp) {
    egui::CentralPanel::default().show(ctx, |ui| {
        let engine = match &app.engine {
            Some(arc) => arc.lock(),
            None => {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.35);
                    ui.label(
                        RichText::new("QVIEW")
                            .size(36.0)
                            .strong(),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("点击 打开 或拖入文本文件开始浏览")
                            .size(16.0)
                            .color(Color32::from_gray(160)),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("支持 .log / .txt / .out / .csv / .json 等文本格式")
                            .size(13.0)
                            .color(Color32::from_gray(130)),
                    );
                });
                return;
            }
        };

        let total_lines = engine.effective_line_count();
        if total_lines == 0 {
            return;
        }
        let num_rows = total_lines as usize;
        let font_size = app.font_size;
        let mono = FontId::monospace(font_size);
        let char_w = measure_char_width(ui, &mono);
        let char_w_f64 = char_w as f64;

        // ---- 测准 galley 实际行高 / 实际字符宽 ----
        // 模型估算的 `row_h = font_size * 1.4` 与 egui galley 的实际行高
        // (~1.2×font_size) 有 ~14% 漂移；用模型做巨行分块 y_top 累加时，每块累积
        // 漂移 ~14%，32KB chunk ≈220 行 × 14% ≈30 行留白（用户实测反馈）。
        // fallback 路径用 `y_cursor += cheight`（实际 galley 高度）是对的，
        // cache 路径错在用 `c0*row_h`。这里把整个 viewer 切换到实测值（galley 行
        // 高 + 实测字符宽），cache / model / scroll 全部一致。
        let row_h_measured = ui.fonts(|f| {
            f.layout_no_wrap("Mg".to_string(), mono.clone(), Color32::WHITE)
                .rect
                .height() as f64
        });
        let row_h = if row_h_measured > 0.0 { row_h_measured } else { app.row_h };
        // 格子系统刻度：格宽 = 实测字符宽，格高 = 实测行高。全浏览器唯一来源。
        let metrics = CharMetrics::new(char_w, row_h as f32);

        // ---- sizing ----
        let sb_w = 12.0;
        let h_sb = 14.0_f64;
        let gap = 2.0;

        let viewport_w = ui.available_width().max(80.0) as f64;
        let log_w = viewport_w - sb_w - gap;
        // Reserve space for status bar + separator (~14px total) so bottom rows
        // and vertical scrollbar thumb aren't hidden behind the status bar.
        let log_h = (ui.available_height() as f64) - h_sb - gap - 2.0 - 14.0;

        // 弹窗打开时冻结主视图滚轮，避免滚轮穿过弹窗滚动底层文本。
        // 注意：器灵窗口**不在**这里——它是独立子窗口（多视口），主窗口必须
        // 始终可滚；若曾误把 show_agent_window 加进来会导致器灵开着时主视图
        // 永远滚不动（用户点主页也没用）。
        let dialog_open = app.show_about
            || app.show_donate
            || app.show_help
            || app.show_shortcuts
            || app.show_settings
            || app.show_file_properties
            || app.show_index_manager
            || app.show_encoding_confirm
            || app.show_annotation_dialog
            || app.show_annotation_list;

        let scroll_delta = if dialog_open {
            0.0
        } else {
            ctx.input(|i| i.smooth_scroll_delta.y) as f64
        };
        let h_delta_raw = if dialog_open || app.word_wrap {
            0.0
        } else {
            ctx.input(|i| {
                (i.smooth_scroll_delta.x
                    + if i.modifiers.shift {
                        i.smooth_scroll_delta.y
                    } else {
                        0.0
                    }) as f64
            })
        };

        let log_rect = Rect::from_min_size(
            ui.cursor().left_top(),
            vec2(log_w as f32, log_h as f32),
        );

        // ---- 超长行列表（懒构建，文件内容变化时失效） ----
        // 编辑器改过行 → 超长行缓存失效。否则 cache.text 是旧快照，渲染/点击的
        // col 与编辑器 read_line 的当前文本错位 → 插入偏移（用户反馈 ab1cd）。
        if app.huge_cache_dirty.get() {
            app.huge_chunk_cache.clear();
            app.huge_cache_dirty.set(false);
        }
        // 构建 key = (行数, mmap 字节数)，两者任一变化（open/reload/save/编辑）即重建。
        // 索引驱动：无超长行 O(1) 短路，**绝不整文件扫描**；后台索引中暂缓（None）。
        let build_key = (engine.effective_line_count(), engine.mmap.size());
        if app.huge_lines_built != Some(build_key) {
            match engine.huge_lines(CHUNK_LINE_BYTES as u64) {
                Some(huge) => {
                    app.huge_lines = huge;
                    app.huge_lines_built = Some(build_key);
                    app.huge_chunk_cache.clear();
                }
                None => {
                    // 索引尚未完成：先按无超长行渲染（超长行走普通路径仍可展开），
                    // 索引完成后 build_key 变化会带着完整列表重算。
                    app.huge_lines.clear();
                    app.huge_chunk_cache.clear();
                }
            }
        }

        // ---- 视觉行模型 ----
        // 普通行：word_wrap 用 wrap_factor（与旧 effective_row_h 一致，保证无超长行
        // 文件滚动行为完全不变），否则 1。超长行：按 bytes_per_row 展开成多视觉行。
        // Compute a fixed wrap-height multiplier from viewport width.
        // Typical log line ~100 chars; each char takes char_w pixels.
        // A line wraps to ceil(100*char_w / log_w) visual rows.
        // Use ≥ 2.0 so the scroll model always has some slack.
        let wrap_factor: f64 = if app.word_wrap {
            let est_rows = (100.0 * char_w_f64 / log_w).ceil();
            est_rows.max(2.0)
        } else {
            1.0
        };
        // Publish for app.rs shortcuts (jump_hit, goto_line, etc.).
        app.wrap_height_mult = wrap_factor;
        let row_mult = wrap_factor.ceil().max(1.0) as u64;
        // gutter 用实测宽度（而不是 `(9.0 * char_w)` 估算）—— 这样 model 的
        // `bytes_per_row` 和 galley 的实际 wrap_w 一致，cr 不会漂。
        let gutter_measured: f32 = if app.show_line_numbers {
            ui.fonts(|f| {
                f.layout_no_wrap("     0 │ ".to_string(), mono.clone(), Color32::WHITE)
                    .rect
                    .width()
            })
        } else {
            0.0
        };
        let wrap_w_model = (log_w as f32 - gutter_measured).max(100.0) as f64;
        let vmodel = VisualRowModel::build(char_w, wrap_w_model, row_h as f32, row_mult, &app.huge_lines);
        app.visual_model = Some(vmodel.clone());
        let row_h_f64 = row_h as f64;
        let content_rows = vmodel.content_rows(num_rows as u64);
        let content_h = content_rows as f64 * row_h_f64;
        let max_v_scroll = (content_h - log_h).max(0.0);
        app.scroll_y = (app.scroll_y - scroll_delta).clamp(0.0, max_v_scroll);

        // 视口顶部的视觉行。
        let vis_top = (app.scroll_y / row_h_f64).floor() as u64;
        // ── Snap scroll_y to whole visual rows to prevent floating-point drift ──
        {
            let ideal = vis_top as f64 * row_h_f64;
            let offset = app.scroll_y - ideal;
            if offset < 0.0 || offset >= row_h_f64 {
                app.scroll_y = ideal + offset.rem_euclid(row_h_f64);
            }
        }

        // 物理行范围：由视觉行映射（含超长行展开）得到，保证滚得进超长行内部。
        let visible_slack = if app.word_wrap { 12u64 } else { 2u64 };
        let visible_row_count = ((log_h / row_h_f64).ceil() as u64).max(1);
        let vis_bottom = vis_top + visible_row_count + visible_slack;
        let first = (vmodel.visual_to_line(vis_top) as usize).min(num_rows.saturating_sub(1));
        let last = (vmodel.visual_to_line(vis_bottom) as usize + 1)
            .min(num_rows)
            .max(first + 1)
            .min(num_rows);

        // Publish visible range so jump_hit can anchor searches to the viewport.
        app.first_visible_line = first as u64;
        app.last_visible_line = last.saturating_sub(1) as u64;

        let theme = app.current_theme_colors().clone();
        let show_line_nums = app.show_line_numbers;
        let do_coloring = app.level_coloring;
        let word_wrap = app.word_wrap;
        let search_query = &app.search_query;
        let qb = search_query.as_bytes();
        // Multi-line queries can't be matched inside a single line, so scan
        // the visible byte window ONCE per frame and clip matches to each
        // line. Only runs while the query contains a newline — single-line
        // queries keep the zero-cost per-line scan below.
        let multiline = !qb.is_empty() && qb.contains(&b'\n');

        // Rebuild the parsed search query whenever it changes, so the per-line
        // highlight matches what the engine actually searched. `parse_query`
        // handles regex, case-insensitivity and whole-word, so highlighting is
        // correct for all of them (previously the viewer re-scanned each line
        // with the raw query bytes, which broke regex / case-insensitive hits).
        let hl_changed = *search_query != app.last_hl_query;
        if hl_changed {
            app.last_hl_query = search_query.clone();
            let opts = qview_core::search::SearchOptions {
                use_regex: app.use_regex,
                case_sensitive: app.case_sensitive,
                whole_word: app.whole_word,
                crlf: engine.uses_crlf(),
            };
            app.parsed_search_q = if search_query.is_empty() {
                None
            } else {
                qview_core::search::parse_query(search_query, &opts).ok()
            };
        }

        let match_ranges: Vec<(u64, u64)> = if multiline {
            let q_len = qb.len() as u64;
            // 用免解码的 line_byte_range：首/末行可能是超长行，read_line 会整行解码。
            let first_byte = engine
                .line_byte_range(first as u64)
                .map(|(s, _)| s)
                .unwrap_or(0);
            let last_line = (last - 1) as u64;
            let last_end = engine
                .line_byte_range(last_line)
                .map(|(_, e)| e)
                .unwrap_or(0);
            let win_start = first_byte.saturating_sub(q_len.saturating_sub(1));
            let win_len = last_end.saturating_sub(win_start) as usize;
            if win_len == 0 {
                Vec::new()
            } else {
                let win = engine.mmap.slice(win_start, win_len);
                match app.parsed_search_q.as_ref() {
                    Some(q) => query_ranges_in(q, win)
                        .map(|(s, e)| (win_start + s as u64, win_start + e as u64))
                        // 巨行 + 常见模式会产出上百万命中区间：只取前 MAX_MATCH_RANGES 个
                        // 高亮（其余行照常渲染），避免收集出几百 MB 的 Vec。
                        .take(MAX_MATCH_RANGES)
                        .collect(),
                    None => Vec::new(),
                }
            }
        } else {
            Vec::new()
        };
        // Diagnostic: log once per multi-line search query the number of match
        // ranges found in the visible window (0 = highlight can't paint).
        if multiline && hl_changed {
            log_debug!("viewer", "多行高亮诊断: 命中区间数={} 首可见行={} 末可见行={} 查询字节数={}",
                match_ranges.len(), first, last, qb.len());
        }
        // ---- measure content width ----
        let mut longest_px: f64 = 0.0;

        // ---- render visible lines ----
        let clip = log_rect;
        ui.set_clip_rect(clip);

        // ---- gutter width (needed for selection column calculation) ----
        // 与上面的 `gutter_measured` 用同一字面量，保证 model 和 galley wrap_w 一致。
        let gutter_w: f32 = if show_line_nums {
            ui.fonts(|f| {
                f.layout_no_wrap("     0 │ ".to_string(), mono.clone(), Color32::WHITE)
                    .rect
                    .width()
            })
        } else {
            0.0
        };

        // ---- content geometry ----
        let content_x = log_rect.left() + gutter_w - app.h_scroll as f32;
        let content_max_w = if word_wrap {
            log_w as f32 - gutter_w
        } else {
            f32::INFINITY
        };
        // ── Phase A: layout visible lines, record ACTUAL positions ─────
        // Hit-testing must use real galley heights (varied per wrapped line),
        // not a uniform effective_row_h — that was the off-by-one source.
        let mut laid: Vec<LaidLine> = Vec::with_capacity(visible_row_count as usize + 8);
        let huge_list = app.huge_lines.clone(); // 少量条目，克隆避免借用冲突
        let wrap_w = (log_w as f32 - gutter_w).max(100.0);
        let mut phys_line = first as u64;
        let mut vrow = vmodel.line_to_visual(phys_line);
        let mut y_cursor = log_rect.top() + (vrow as f64 * row_h_f64 - app.scroll_y) as f32;

        // Resolve the current search-hit byte ONCE per frame.
        let current_hit_byte: Option<u64> = engine.search.current().map(|m| m.byte);

        // Agent 视图过滤器快照（不匹配的行淡化，不影响人类搜索状态）
        let agent_filter = app.agent_filter.clone();

        loop {
            if vrow >= vis_bottom || phys_line as usize >= num_rows {
                break;
            }
            let line_no = phys_line;

            // ── 超长行：缓存分块 + 只排版视口内块 ──
            // 整行只在首次需要时读一次进缓存；之后每帧只对与视口相交的块排版。
            // 滚动由视觉行模型驱动（能滚进超长行内部看到屏外的匹配高亮），不再
            // 每帧整行重读 / 全量排版 → 内存与卡顿都消除。
            if huge_list.binary_search_by_key(&line_no, |&(l, _)| l).is_ok() {
                // ── 编辑检测：行内字符编辑不改 build_key（effective_line_count /
                //    mmap.size 都不变），huge_chunk_cache.text 会变陈旧 → 编辑超长行
                //    输入看不到实时回显（用户反馈）。用 engine.line_byte_range 的长度
                //    对比缓存文本长度：不同 → 该行被编辑过 → 移除缓存，让
                //    ensure_huge_meta 重建（read_line 新文本 + 重建 byte_index）。
                //    重建后长度一致，后续帧不再重建。 ──
                if let Some(ci) = app.huge_chunk_cache.iter().position(|c| c.line == line_no) {
                    let cur_len = engine
                        .line_byte_range(line_no)
                        .map(|(s, e)| (e - s) as usize)
                        .unwrap_or(0);
                    if app.huge_chunk_cache[ci].text.len() != cur_len {
                        app.huge_chunk_cache.remove(ci);
                    }
                }
                // 用首块文本做行着色判定（日志级别标记都在行首，无需整行）
                let head_text: std::borrow::Cow<'_, str> =
                    if let Some(ci) = ensure_huge_meta(
                        &mut app.huge_chunk_cache,
                        &engine,
                        line_no,
                        font_size,
                        wrap_w,
                    ) {
                        // 直接用缓存的文本首 200 chars（避免再 read_line）
                        std::borrow::Cow::Borrowed(&app.huge_chunk_cache[ci].text)
                    } else {
                        let raw = engine.read_line(line_no);
                        std::borrow::Cow::Owned(raw.text)
                    };
                let head_text_str: String = head_text.chars().take(200).collect();
                let head_text = head_text_str.as_str();
                let mut text_color = if do_coloring {
                    level_color(&head_text, &theme).unwrap_or(theme.text_primary)
                } else {
                    theme.text_primary
                };
                if let Some(f) = &agent_filter {
                    if !filter_matches_line(f, &head_text) {
                        text_color = text_color.gamma_multiply(0.35);
                    }
                }

                // per-row layout：每行一个独立小 LayoutJob（max_rows=1）→ 一个小 galley。
                // 内存只跟视口内行数成正比（~50 × 5KB = 250KB），不再 30MB+ 单 galley。
                // 行间连续 wrap → 没有 chunk 边界 / 没有「下一块换新行」。
                // chunk_char = 行内绝对字符起点 → 光标 / 选区 / hit-test 都能精确定位。
                if let Some(ci) = app.huge_chunk_cache.iter().position(|c| c.line == line_no) {
                    let cache_entry = &app.huge_chunk_cache[ci];
                    let char_count = cache_entry.char_count;
                    let full_text: &str = &cache_entry.text;
                    let byte_index: &Vec<(usize, usize)> = &cache_entry.byte_index;
                    // 估算每行字符数（等宽字体下 wrap_w / char_w），用作跳行定位。
                    let chars_per_row = (wrap_w as f64 / char_w_f64).floor().max(1.0) as usize;
                    // 在本行内的可见行区间 [loc0, loc1)
                    let loc0 = vis_top.saturating_sub(vrow);
                    let loc1 = vis_bottom.saturating_sub(vrow);
                    let vis_rows = loc1.saturating_sub(loc0);
                    if vis_rows > 0 {
                        // 逐行 layout（每行 LayoutJob 带 max_rows=1，galley 只有 1 行）
                        let content_fmt = TextFormat {
                            font_id: mono.clone(),
                            color: text_color,
                            ..Default::default()
                        };
                        let match_fmt = TextFormat {
                            font_id: mono.clone(),
                            color: text_color,
                            background: theme.search_highlight,
                            ..Default::default()
                        };
                        let current_match_fmt = TextFormat {
                            font_id: mono.clone(),
                            color: text_color,
                            background: theme.search_current,
                            ..Default::default()
                        };
                        // line_start_byte 从缓存拿（避免再 read_line 一次，巨行 30ms / 次）。
                        let line_start_byte = cache_entry.start_byte;
                        let parsed_q = app.parsed_search_q.as_ref();
                        // 从行开头精确累积 char_pos：视口前每行用缓存的 row_chars
                        // （滚动经过的行会记录实际字符数）；没有缓存就 layout 一次补齐
                        // （一次性成本：跳到超长行深处时首次补齐视口前 ~几十 ms）。
                        // `loc0 * chars_per_row` 纯估算在含 CJK/多字节字符时会偏大（每行
                        // 实际字符数 < 单字节宽估算），导致 chunk_char / 光标位置偏大 1。
                        let mut char_pos = 0usize;
                        let mut row_idx = 0usize;
                        let loc0_u = loc0 as usize;
                        while row_idx < loc0_u {
                            let rc_known = cache_entry.layout.get(row_idx).map(|m| m.char_count);
                            let rc = match rc_known {
                                Some(rc) => rc,
                                None => {
                                    // 补齐该行：layout 取实际字符数，并记录进 layout 缓存
                                    let bp = char_pos_to_byte_pos(byte_index, full_text, char_pos);
                                    let chars_left = char_count - char_pos;
                                    let max_chars = (chars_per_row + 8).min(chars_left);
                                    let remaining = &full_text[bp..];
                                    let slice = if max_chars >= chars_left {
                                        remaining
                                    } else {
                                        let end = remaining
                                            .char_indices()
                                            .nth(max_chars)
                                            .map(|(b, _)| b)
                                            .unwrap_or(remaining.len());
                                        &remaining[..end]
                                    };
                                    // 行内 \n = 视觉换行：截断到 \n 前（否则 max_rows=1 少算）
                                    let (row_text, had_nl) = match slice.find('\n') {
                                        Some(nl) => (&slice[..nl], true),
                                        None => (slice, false),
                                    };
                                    let n = layout_row_char_count(ui, wrap_w, row_text, &content_fmt);
                                    cache_entry.layout.set_row(row_idx, char_pos, n);
                                    n + if had_nl { 1 } else { 0 } // 推进跳过 \n
                                }
                            };
                            char_pos += rc;
                            row_idx += 1;
                        }
                        char_pos = char_pos.min(char_count);
                        // 用 byte_pos 跟踪 layout 文本切片起点：通过 byte_index O(log N + stride) 跳转
                        let mut byte_pos: usize = char_pos_to_byte_pos(byte_index, full_text, char_pos);
                        let qb = search_query.as_bytes();
                        let multiline = !qb.is_empty() && qb.contains(&b'\n');
                        // 整行预扫一次匹配范围（相对行首字节），每个视觉行按本行
                        // 字节区间裁剪。之前每个视觉行只扫自己的 row_text 切片（含 8
                        // 字符余量），匹配词跨视觉行边界且超出余量时扫不到完整词 →
                        // 跨行匹配不高亮（用户反馈）。整行扫 + 裁剪能正确高亮跨行匹配
                        // （A 行高亮匹配词左半，B 行高亮右半，视觉连续）。
                        let mut line_matches: Vec<(usize, usize)> = Vec::new();
                        if !multiline {
                            if let Some(q) = parsed_q {
                                line_matches = query_ranges_in(q, full_text.as_bytes())
                                    .take(MAX_MATCH_RANGES)
                                    .collect();
                            }
                        }
                        for row_in_vis in 0..vis_rows {
                            if char_pos >= char_count { break; }
                            // 关键：不要传整段剩余 text 给 layout_job——它会扫整段才能
                            // 算出 wrap 点，单次 layout 复杂度 O(剩余长度) = O(N)。视口内
                            // N rows × 平均 N/2 剩余 = O(N²) per frame = 巨行卡顿根因。
                            // 限制 row_text 切片到 chars_per_row + 余量的字节数（按 char
                            // 边界对齐）。
                            let chars_left = char_count - char_pos;
                            let max_chars = (chars_per_row + 8).min(chars_left);
                            let (row_text, had_nl): (&str, bool) = {
                                let remaining = &full_text[byte_pos..];
                                let slice = if max_chars >= chars_left {
                                    remaining
                                } else {
                                    let end_byte = remaining
                                        .char_indices()
                                        .nth(max_chars)
                                        .map(|(b, _)| b)
                                        .unwrap_or(remaining.len());
                                    &remaining[..end_byte]
                                };
                                // 行内 \n（如格式化 JSON）= 视觉换行：截断到 \n 前，
                                // \n 结束当前视觉行（不 layout 进当前行，推进时跳过）。
                                // 否则 max_rows=1 在 \n 换行 → glyphs 只算到 \n 前 →
                                // 累积少算 → chunk_char 偏 → col 偏（用户实测全错乱）。
                                match slice.find('\n') {
                                    Some(nl) => (&slice[..nl], true),
                                    None => (slice, false),
                                }
                            };
                            // 用 max_rows=1 强制只 layout 一行，剩下的字符被丢弃（下次循环不进入）
                            let mut job = LayoutJob::default();
                            job.wrap.max_width = wrap_w;
                            job.wrap.break_anywhere = true;
                            job.wrap.max_rows = 1;
                            // 不要省略号：max_rows=1 截断时 egui 默认用 '…' 替换超出字符，
                            // 换行后每行末尾会显示省略号（用户反馈）。设为 None → 直接裁剪。
                            job.wrap.overflow_character = None;
                            // 搜索高亮：单行直接扫本行切片；多行查 match_ranges
                            let row_bytes = row_text.as_bytes();
                            let mut pos = 0usize;
                            if multiline {
                                let row_byte_start = line_start_byte + byte_pos as u64;
                                let row_byte_end = row_byte_start + row_bytes.len() as u64;
                                for &(gs, ge) in &match_ranges {
                                    if ge <= row_byte_start { continue; }
                                    if gs >= row_byte_end { break; }
                                    let a = (gs.max(row_byte_start) - row_byte_start) as usize;
                                    let b = (ge.min(row_byte_end) - row_byte_start) as usize;
                                    if a > pos {
                                        job.append(&row_text[pos..a], 0.0, content_fmt.clone());
                                    }
                                    let is_current = current_hit_byte.map_or(false, |hb: u64| {
                                        let seg_start = line_start_byte + byte_pos as u64 + pos as u64;
                                        let seg_end = line_start_byte + byte_pos as u64 + b as u64;
                                        hb >= seg_start && hb < seg_end
                                    });
                                    let fmt = if is_current {
                                        current_match_fmt.clone()
                                    } else {
                                        match_fmt.clone()
                                    };
                                    job.append(&row_text[a..b], 0.0, fmt);
                                    pos = b;
                                }
                            } else {
                                // 用整行预扫的 line_matches（full_text 字节偏移）裁剪到
                                // 本行字节区间 [byte_pos, byte_pos+row_text.len())。
                                let row_byte_off = byte_pos;
                                let row_byte_end = byte_pos + row_text.len();
                                for &(a, b) in &line_matches {
                                    if b <= row_byte_off { continue; }
                                    if a >= row_byte_end { break; }
                                    let a2 = a.max(row_byte_off);
                                    let b2 = b.min(row_byte_end);
                                    let la = a2 - row_byte_off;
                                    let lb = b2 - row_byte_off;
                                    if la > pos {
                                        job.append(&row_text[pos..la], 0.0, content_fmt.clone());
                                    }
                                    let abs_start = line_start_byte + a2 as u64;
                                    let abs_end = line_start_byte + b2 as u64;
                                    let is_current = current_hit_byte.map_or(false, |hb| {
                                        hb >= abs_start && hb < abs_end
                                    });
                                    let fmt = if is_current {
                                        current_match_fmt.clone()
                                    } else {
                                        match_fmt.clone()
                                    };
                                    job.append(&row_text[la..lb], 0.0, fmt);
                                    pos = lb;
                                }
                            }
                            if pos < row_bytes.len() {
                                job.append(&row_text[pos..], 0.0, content_fmt.clone());
                            }
                            let galley = ui.fonts(|f| f.layout_job(job));
                            // 此行的实际字符数（等宽下 ≈ chars_per_row，最后一行可能更少）
                            let row_chars = galley.rows.first().map(|r| r.glyphs.len()).unwrap_or(0);
                            let row_byte_count = row_chars
                                .checked_sub(if char_pos + row_chars > 0 && row_chars > 0 { 1 } else { 0 })
                                .unwrap_or(0);
                            let _ = row_byte_count;
                            let row_height = galley.rect.height().max(row_h as f32);
                            let row_px = galley.rect.width() as f64;
                            if row_px > longest_px { longest_px = row_px; }
                            // y_top = line_top + (loc0 + row_in_vis) * row_h
                            let row_y = y_cursor + (loc0 as f32 + row_in_vis as f32) * row_h as f32;
                            laid.push(LaidLine {
                                line: line_no,
                                y_top: row_y,
                                height: row_height,
                                galley,
                                chunk_char: char_pos, // 该行在原始行里的字符起点
                            });
                            // 记录本视觉行实际字符数（供后续帧视口前精确推进 char_pos，
                            // 以及 ViewMapping 的字符↔视觉行换算）
                            cache_entry.layout.set_row(row_idx, char_pos, row_chars);
                            row_idx += 1;
                            // 推进 char_pos / byte_pos 到下一行起点（用 row_text 切片算，不要走 full_text[byte_pos..]，
                            // 否则对巨行又是 O(N)）。row_chars 是 layout 后 row 0 实际放的字符数。
                            // had_nl：该视觉行被行内 \n 截断 → 推进时跳过 \n（\n 占 1 字符，视觉换行）。
                            if row_chars > 0 || had_nl {
                                let advance_byte = if row_chars >= row_text.chars().count() {
                                    // row_text 全部用上
                                    row_text.len()
                                } else {
                                    // layout 截断：找出 row_text 内第 row_chars 个字符后的字节偏移
                                    row_text
                                        .char_indices()
                                        .nth(row_chars)
                                        .map(|(b, _)| b)
                                        .unwrap_or(row_text.len())
                                };
                                let skip_nl = if had_nl { 1 } else { 0 };
                                byte_pos += advance_byte + skip_nl;
                                char_pos += row_chars + skip_nl;
                            } else {
                                break; // 没字符可 layout 了
                            }
                        }
                    }
                    // 推进 y_cursor / vrow：基于实际行数（=ceil(char_count/chars_per_row)）
                    let actual_rows = ((char_count + chars_per_row - 1) / chars_per_row) as u64;
                    y_cursor += actual_rows as f32 * row_h as f32;
                    vrow += actual_rows;
                    phys_line += 1;
                    continue;
                }
            }

            let raw = engine.read_line(line_no);
            let full_text = &raw.text;

            let mut text_color = if do_coloring {
                level_color(full_text, &theme).unwrap_or(theme.text_primary)
            } else {
                theme.text_primary
            };
            if let Some(f) = &agent_filter {
                if !filter_matches_line(f, full_text) {
                    // 淡化不匹配行（保留行结构，突显命中区）
                    text_color = text_color.gamma_multiply(0.35);
                }
            }

            let mut job = LayoutJob::default();
            let content_fmt = TextFormat {
                font_id: mono.clone(),
                color: text_color,
                ..Default::default()
            };
            let match_fmt = TextFormat {
                font_id: mono.clone(),
                color: text_color,
                background: theme.search_highlight,
                ..Default::default()
            };
            let current_match_fmt = TextFormat {
                font_id: mono.clone(),
                color: text_color,
                background: theme.search_current,
                ..Default::default()
            };

            // ── 兜底全量 layout（huge_list 漏判但文本确实超长，如编辑引入的新超长行或
//    huge_lines 扫描未完成；正常打开文件不会走到——超长行已在上方走缓存路径）──
// 跟 cache 路径同样按 per-row 切：每个视觉行一个 LaidLine，chunk_char=行内字符起点。
// 绝对不能像之前那样 layout 一整行成一个 galley（会让整个 LaidLine height = 整
// 行换行后总高，渲染时选区 / 光标都按「单行」处理，跨行选不中、caret 占满整高）。
if full_text.len() > CHUNK_LINE_BYTES {
    let char_count = full_text.chars().count();
    let chars_per_row = (wrap_w as f64 / char_w_f64).floor().max(1.0) as usize;
    // 按 row 逐行 layout（每行 LayoutJob 只 max_rows=1）。这里不再 layout 整行——
    // 一整行 10MB 的整 galley 会让 y_cursor/选区/caret 全部按单行处理。
    let mut cum_char: usize = 0;
    let mut row_in_vis_local: usize = 0;
    // 仅 layout 视口内需要的行（loc0..loc1），视口外用估算推进 running_y
    let loc0 = vis_top.saturating_sub(vrow);
    let loc1 = vis_bottom.saturating_sub(vrow);
    let vis_rows = loc1.saturating_sub(loc0);
    let vis_rows_u = vis_rows as usize;
    while cum_char < char_count {
        // 跳过视口外的 row（用估算字符数）
        if row_in_vis_local >= vis_rows_u && cum_char < char_count {
            // 还在视口外：估算
            cum_char = cum_char.saturating_add(chars_per_row).min(char_count);
            row_in_vis_local += 1;
            continue;
        }
        if row_in_vis_local >= vis_rows_u { break; }
        // 视口内的 row：layout 单行
        let chars_left = char_count - cum_char;
        let max_chars = (chars_per_row + 8).min(chars_left);
        let (row_text, had_nl): (&str, bool) = {
            let remaining = &full_text[full_text.char_indices().nth(cum_char).map(|(b, _)| b).unwrap_or(full_text.len())..];
            let slice = if max_chars >= chars_left {
                remaining
            } else {
                let end_byte = remaining
                    .char_indices()
                    .nth(max_chars)
                    .map(|(b, _)| b)
                    .unwrap_or(remaining.len());
                &remaining[..end_byte]
            };
            // 行内 \n = 视觉换行：截断到 \n 前（否则 max_rows=1 少算）
            match slice.find('\n') {
                Some(nl) => (&slice[..nl], true),
                None => (slice, false),
            }
        };
        let mut onejob = LayoutJob::default();
        onejob.wrap.max_width = wrap_w;
        onejob.wrap.break_anywhere = true;
        onejob.wrap.max_rows = 1;
        onejob.wrap.overflow_character = None; // 不显示省略号（同 cache 路径）
        onejob.append(row_text, 0.0, content_fmt.clone());
        let one_galley = ui.fonts(|f| f.layout_job(onejob));
        let row_chars = one_galley.rows.first().map(|r| r.glyphs.len()).unwrap_or(0);
        let row_height = one_galley.rect.height().max(row_h as f32);
        let row_px = one_galley.rect.width() as f64;
        if row_px > longest_px { longest_px = row_px; }
        let row_y = y_cursor + (loc0 as f32 + row_in_vis_local as f32) * row_h as f32;
        laid.push(LaidLine {
            line: line_no,
            y_top: row_y,
            height: row_height,
            galley: one_galley,
            chunk_char: cum_char,
        });
        let skip_nl = if had_nl { 1 } else { 0 };
        cum_char += row_chars + skip_nl;
        row_in_vis_local += 1;
        if row_chars == 0 && !had_nl { break; }
    }
    let actual_rows = ((char_count + chars_per_row - 1) / chars_per_row) as u64;
    y_cursor += actual_rows as f32 * row_h as f32;
    vrow += actual_rows;
    phys_line += 1;
    continue;
}

            if search_query.is_empty() || qb.is_empty() {
                job.append(full_text, 0.0, content_fmt);
            } else if multiline {
                // Multi-line query: clip the per-frame match ranges to this
                // line. Highlight covers match start line → spanned lines →
                // match end line.
                let hit_byte = current_hit_byte;
                let line_start = raw.start_byte;
                let line_end = line_start + full_text.len() as u64;
                let mut pos = 0usize;
                for &(gs, ge) in &match_ranges {
                    if ge <= line_start {
                        continue;
                    }
                    if gs >= line_end {
                        break;
                    }
                    let a = ((gs.max(line_start)) - line_start) as usize;
                    let b = ((ge.min(line_end)) - line_start) as usize;
                    if a > pos {
                        job.append(&full_text[pos..a], 0.0, content_fmt.clone());
                    }
                    // A line belongs to the CURRENT match if it overlaps the
                    // match's whole byte span — so a multi-line match lights
                    // up on every line it spans, not just its start line.
                    let is_current = hit_byte.map_or(false, |hb| {
                        line_start < hb + qb.len() as u64 && line_end > hb
                    });
                    let fmt = if is_current {
                        current_match_fmt.clone()
                    } else {
                        match_fmt.clone()
                    };
                    job.append(&full_text[a..b], 0.0, fmt);
                    pos = b;
                }
                if pos < full_text.len() {
                    job.append(&full_text[pos..], 0.0, content_fmt);
                }
            } else {
                let hit_byte = current_hit_byte;
                let line_start = raw.start_byte;
                let cb = full_text.as_bytes();
                let mut pos = 0usize;
                if let Some(q) = app.parsed_search_q.as_ref() {
                    // 巨行 + 常见模式：只高亮前 MAX_MATCH_RANGES 个命中，防止把
                    // 上百万段 append 进 job → 巨大 galley → 内存暴涨（用户实测
                    // 搜索后点下一条飙到 2G）。截断后剩余行尾按普通文本渲染。
                    for (a, b) in query_ranges_in(q, cb).take(MAX_MATCH_RANGES) {
                        if a > pos {
                            job.append(&full_text[pos..a], 0.0, content_fmt.clone());
                        }
                        let end = b.min(full_text.len());
                        let abs_start = line_start + a as u64;
                        let abs_end = line_start + end as u64;
                        let is_current = hit_byte.map_or(false, |hb| {
                            hb >= abs_start && hb < abs_end
                        });
                        let fmt = if is_current {
                            current_match_fmt.clone()
                        } else {
                            match_fmt.clone()
                        };
                        job.append(&full_text[a..end], 0.0, fmt);
                        pos = end;
                    }
                }
                if pos < full_text.len() {
                    job.append(&full_text[pos..], 0.0, content_fmt);
                }
            }

            if word_wrap {
                job.wrap.max_width = content_max_w;
                // Character-based wrapping (like a code editor), NOT egui's
                // default word-based wrap.  With mixed CJK + latin + tabs the
                // word-based wrap leaves big gaps at the end of rows (breaking
                // early because a "word" can't fit), which also breaks the
                // row↔character mapping used for selection highlights.
                job.wrap.break_anywhere = true;
            }

            let galley = ui.fonts(|f| f.layout_job(job));

            let line_height = if word_wrap {
                galley.rect.height().max(row_h as f32)
            } else {
                row_h as f32
            };

            let line_px = galley.rect.width() as f64;
            if line_px > longest_px {
                longest_px = line_px;
            }

            laid.push(LaidLine {
                line: line_no,
                y_top: y_cursor,
                height: line_height,
                galley,
                chunk_char: 0, // 普通行：整行一段，字符起点 0
            });
            y_cursor += line_height;
            vrow += row_mult;
            phys_line += 1;
        }

        // Update content width for horizontal scrolling.
        // The longest line by BYTES may not be the widest by PIXELS: ASCII is
        // 1px/byte but CJK is only 0.67px/byte, so a shorter ASCII line can be
        // wider than a longer CJK one.  For small files we scan every line once
        // and measure its real pixel width — exact.  For large files we measure
        // the longest-bytes line (good enough for ASCII-dominant logs).
        // Computed BEFORE the selection handler so drag-to-select auto-scroll
        // can clamp the horizontal scroll to the same range.
        let measured_w = longest_px;
        let mut text_w = measured_w.max(app.max_content_w);
        const FULL_SCAN_LINE_LIMIT: u64 = 20_000;
        // 横向滚动范围只统计**非超长行**：超长行（> CHUNK_LINE_BYTES）无论是否
        // word_wrap 都被强制分块换行，永远不占横向空间。若把它们的字节/像素宽度
        // 计入 max_content_w，横向滚动条范围会被一条几 MB 的行顶到几千万像素，
        // 导致普通行想看行尾时「稍微一碰就超出很多」。extent 只跟随非超长行。
        // Horizontal scroll is only used in non-wrap mode.
        // `!full_width_scan_done` gate 在**最外层**：整块（全量扫描或快速路径）
        // 每文件只测一次。原来快速路径没被 gate，**每帧**都重读最长行 +
        // 用字体排版测量：若文件里有超长行（putty 日志常有几 MB 的单行），
        // 每帧分配一个巨大 String + galley → 内存暴涨 + 打开后持续卡顿
        // （用户实测：48MB/31万行 的文件卡，10G 的正常行文件不卡）。
        // 注意：这里复用 render 已持有的 engine guard，**不**再 lock（非重入，避免死锁）。
        if engine.index.is_complete() && !word_wrap && !app.full_width_scan_done {
            let total = engine.effective_line_count();
            if total <= FULL_SCAN_LINE_LIMIT {
                // Exact: measure every NON-huge line's pixel width once.
                let mut max_w: f64 = 0.0;
                for ln in 0..total {
                    let raw = engine.read_line(ln);
                    if raw.byte_len > CHUNK_LINE_BYTES {
                        continue; // 超长行强制换行，不计入横向滚动范围
                    }
                    let w = metrics.text_w(&raw.text) as f64;
                    if w > max_w {
                        max_w = w;
                    }
                }
                text_w = text_w.max(max_w);
            } else {
                // Fast path: measure the longest-bytes line (CJK-vs-ASCII
                // mismatch is rare in large ASCII-dominant logs). If that line
                // is a huge line, skip it — the extent grows from the widest
                // visible non-huge line as the user scrolls.
                let li = engine.longest_line_index();
                let mbl = engine.max_line_byte_len();
                // max_line_byte_len 含行尾换行（比 read_line 的 byte_len 多 1），
                // 所以阈值让 1 字节，与 CHUNK_LINE_BYTES 的内容长度口径对齐。
                if li < total && mbl > 0 && mbl <= CHUNK_LINE_BYTES as u64 + 1 {
                    let raw = engine.read_line(li);
                    let w = metrics.text_w(&raw.text) as f64;
                    text_w = text_w.max(w);
                }
            }
            app.full_width_scan_done = true;
        }
        app.max_content_w = text_w;

        // Horizontal scroll range.  The gutter scrolls WITH the content, so the
        // rightmost content edge = gutter_w + text_w.  Add EXTRA_PAD beyond the
        // line end so the longest line never sits flush against the right edge
        // (or hidden behind the vertical scrollbar) at max scroll.
        let text_extent = gutter_w as f64 + text_w;
        let overflow = (text_extent - log_w).max(0.0);
        const EXTRA_PAD: f64 = 150.0;
        let max_h_scroll = if overflow > 0.0 {
            overflow + EXTRA_PAD
        } else {
            0.0
        };
        app.h_scroll = (app.h_scroll - h_delta_raw).clamp(0.0, max_h_scroll);

        // ---- text selection mouse handling ----
        // Interact with the log content area via click‑and‑drag.  We handle
        // three states in one unified branch so drag‑initiated selections
        // work reliably:
        //
        //   1. `clicked()`         → single‑click starts a fresh selection
        //   2. `dragged()`         → extending the active selection
        //   3. `hovered + button`  → drag start (before the drag threshold is
        //                            hit) — initialise the selection here so
        //                            the following drag frames see it.
        let content_area = Rect::from_min_size(
            pos2(log_rect.left(), log_rect.top()),
            vec2(log_w as f32, log_h as f32),
        );
        let content_id = ui.make_persistent_id("log_content");
        let content_resp = ui.interact(content_area, content_id, Sense::click_and_drag());

        // Helper: screen position → (line, column).  Hit-tested against the
        // ACTUAL rendered line positions; column uses font measurement so
        // CJK double-width characters map correctly.  For wrapped lines the
        // visual sub-row is resolved against the galley's real row geometry
        // (font line height), NOT the app row_h — CJK fonts have taller line
        // boxes, so using row_h here was the off-by-one-row source.
        let pos_to_line_col =
            |pos: egui::Pos2| -> (u64, usize) {
                let mut hit: Option<&LaidLine> = None;
                for l in &laid {
                    if pos.y >= l.y_top && pos.y < l.y_top + l.height {
                        hit = Some(l);
                        break;
                    }
                }
                let (line, y_top, hit_galley, chunk_char) = match hit {
                    Some(l) => (l.line, l.y_top, Some(&l.galley), l.chunk_char),
                    None => {
                        if laid.is_empty() {
                            (0, log_rect.top(), None, 0)
                        } else if pos.y < laid[0].y_top {
                            (laid[0].line, laid[0].y_top, Some(&laid[0].galley), laid[0].chunk_char)
                        } else {
                            let l = laid.last().unwrap();
                            (l.line, l.y_top, Some(&l.galley), l.chunk_char)
                        }
                    }
                };
                let target_x = (pos.x - content_x).max(0.0);
            // 统一用 galley glyph 位置做 col 映射：
            //  - per-row 巨行（word_wrap 开或关）：galley 只有 1 行，sub_row=0、
            //    prior_chars=0，col = chunk_char + lo。之前 word_wrap=false 走了
            //    pixel_to_char(整行文本, target_x) —— 用整行做 layout 且 target_x
            //    只有第一行内的几百 px，col 完全忽略 chunk_char → 点第 N 个视觉行
            //    却映射到第一行（用户反馈：只能选中第一行 / 光标只能在第一行）。
            //  - 普通 word_wrap 行：galley 多行，sub_row + prior_chars + chunk_char(0)。
            //  - 普通非 wrap 行：单行 galley，chunk_char=0，lo 即绝对字符位置。
            let col = if let Some(g) = hit_galley {
                let mut prior_chars = 0usize;
                let mut sub_row = g.rows.len().saturating_sub(1);
                let rel_y = (pos.y - y_top).max(0.0);
                for (i, row) in g.rows.iter().enumerate() {
                    let r = row.rect;
                    if rel_y >= r.top() && rel_y < r.top() + r.height() {
                        sub_row = i;
                        break;
                    }
                }
                for row in g.rows.iter().take(sub_row) {
                    prior_chars += row.glyphs.len();
                }
                let row = &g.rows[sub_row];
                // 像素 → 行内字符列（`metrics.x_to_char` 统一二分 glyph 位置）
                let lo = metrics.x_to_char(row, target_x);
                let col = chunk_char + prior_chars + lo;
                col
            } else {
                0
            };
            (line, col)
        };

        let pointer_down = ctx.input(|i| i.pointer.primary_down());
        // Detect the first frame of a new press by comparing against the
        // previous frame's state.  egui 0.31 has no `button_just_pressed`.
        let pointer_just_pressed = pointer_down && !app.pointer_was_down;
        let pointer_just_released = !pointer_down && app.pointer_was_down;
        app.pointer_was_down = pointer_down;

        // Track the selection drag.  `selecting` is our own flag (not egui's
        // `dragged()`) so the selection keeps extending even when the pointer
        // leaves the content area mid-drag — we auto-scroll instead.
        if pointer_just_pressed && content_resp.hovered() {
            app.selecting = true;
        }
        if !pointer_down {
            app.selecting = false;
        }

        if app.selecting && pointer_down {
            // Drag is active: extend the selection toward the pointer, and
            // auto-scroll when the pointer is beyond a viewport edge so the
            // selection keeps growing into the newly revealed content.
            if let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) {
                let mut p = pos;

                // Horizontal auto-scroll (non-wrap mode only).
                if !word_wrap && max_h_scroll > 0.0 {
                    if p.x < content_area.left() {
                        let d = (content_area.left() - p.x) as f64;
                        app.h_scroll =
                            (app.h_scroll - AUTO_SCROLL_RATE * d).clamp(0.0, max_h_scroll);
                        p.x = content_area.left();
                    } else if p.x > content_area.right() {
                        let d = (p.x - content_area.right()) as f64;
                        app.h_scroll =
                            (app.h_scroll + AUTO_SCROLL_RATE * d).clamp(0.0, max_h_scroll);
                        p.x = content_area.right();
                    }
                }
                // Vertical auto-scroll.
                if p.y < log_rect.top() {
                    let d = (log_rect.top() - p.y) as f64;
                    app.scroll_y =
                        (app.scroll_y - AUTO_SCROLL_RATE * d).clamp(0.0, max_v_scroll);
                    p.y = log_rect.top();
                } else if p.y > log_rect.bottom() {
                    let d = (p.y - log_rect.bottom()) as f64;
                    app.scroll_y =
                        (app.scroll_y + AUTO_SCROLL_RATE * d).clamp(0.0, max_v_scroll);
                    p.y = log_rect.bottom();
                }

                // engine guard 在 render 顶层已持有；此处不再 lock（非重入）
                let (line, col) = pos_to_line_col(p);
                if pointer_just_pressed {
                    // New press — always start a FRESH selection anchor, never
                    // reusing the previous selection's start line.
                    app.selection = Some((line, col, line, col));
                    // In edit mode the caret follows the pointer.
                    if app.edit_mode {
                        app.edit_cursor = Some((line, col));
                        // 鼠标移动光标 = 打断输入法组合。
                        app.edit_ime_preedit.clear();
                    }
                } else {
                    match app.selection {
                        None => app.selection = Some((line, col, line, col)),
                        Some((start_line, start_col, _, _)) => {
                            app.selection = Some((start_line, start_col, line, col));
                        }
                    }
                }
            }
        }

        // A plain click already collapses the selection to a point on press
        // (the `pointer_just_pressed` branch above); we deliberately do NOT
        // reset it in `clicked()`, otherwise releasing a drag would collapse
        // the selection just made.
        if pointer_just_released {
            log_debug!("viewer", "选区结束: {:?}", app.selection);
        }

        // Note: Ctrl+C copy is handled globally in handle_copy_shortcut() (runs
        // early in update() before any widget consumes the event).  Right-click
        // context menu below serves as a backup.

        // ---- Right-click context menu (copy + annotate) ----
        if app.selection.is_some() {
            content_resp.context_menu(|ui| {
                if ui.button("📋 复制选中内容").clicked() {
                    // Defer the copy — ctx.data persists this flag across frames,
                    // and we process it at the top of the next update() call.
                    log_debug!("viewer", "右键菜单 → 复制选中内容");
                    ui.ctx().data_mut(|d| d.insert_persisted(
                        egui::Id::new("pending_copy"),
                        true,
                    ));
                    ui.close_menu();
                }
                // 批注 anchors to the saved file — an unsaved new file has no
                // real path, so adding an annotation here would be lost on save.
                // Disable (with a hint) instead of hiding, so users learn why.
                let ann_btn = egui::Button::new("📝 添加批注");
                let ann_resp = if app.is_new_file {
                    ui.add_enabled(false, ann_btn).on_disabled_hover_text(
                        "新文件尚未保存，无法添加批注（保存后即可添加）",
                    )
                } else {
                    ui.add(ann_btn)
                };
                if ann_resp.clicked() {
                    log_debug!("viewer", "右键菜单 → 添加批注");
                    // Open the annotation dialog as a NEW annotation.  Guard
                    // against an empty selection (single click collapses it to
                    // a point) by letting the dialog surface the message.
                    app.show_annotation_dialog = true;
                    app.annotation_edit_id = None;
                    app.annotation_input.clear();
                    ui.close_menu();
                }
            });
        }

        // ---- paint pass (uses pre-laid-out galleys from Phase A) ----
        for l in &laid {
            let line_no = l.line;
            let y_cursor = l.y_top;
            let line_height = l.height;

            let row_rect = Rect::from_min_size(
                pos2(log_rect.left(), y_cursor),
                vec2(log_w as f32, line_height),
            );
            if !clip.intersects(row_rect) {
                continue;
            }

            // ── 调试 log 已移除 ──

            // ---- gutter ----
            // 块是实现细节，渲染时只让首块（chunk_char == 0）画行号；续块
            // **完全不画** gutter（连分隔符 `│` 也不画），让逻辑行在视觉上
            // 等价于一次连续的自动换行——用户反馈：分隔符 `│` 看上去像「高亮竖行」，
            // 让 chunk 间看起来像不同内容。
            if show_line_nums && l.chunk_char == 0 {
                let gutter = format!("{:>6} │ ", line_no + 1);
                let gutter_galley = ui.fonts(|f| {
                    f.layout_no_wrap(gutter, mono.clone(), theme.line_number)
                });
                let gutter_pos = pos2(log_rect.left() - app.h_scroll as f32, y_cursor);
                ui.painter().galley(gutter_pos, gutter_galley, Color32::WHITE);
            }

            // ---- annotation marker (3px amber bar left of the text) ----
            // 分块行只在首块（chunk_char==0）画，避免同一行重复画条
            if l.chunk_char == 0 && app.annotated_lines.contains(&line_no) {
                let mx = (content_x - 4.0).max(log_rect.left());
                ui.painter().rect_filled(
                    Rect::from_min_size(pos2(mx, y_cursor), vec2(3.0, line_height)),
                    0.0,
                    Color32::from_rgb(224, 172, 56),
                );
            }

            // ---- agent highlight bar (ViewIntent::HighlightRange, 3px 彩色条) ----
            for &(hs, he, kind) in &app.agent_highlights {
                if line_no >= hs && line_no <= he {
                    let color = match kind {
                        HighlightKind::AgentFocus => Color32::from_rgb(0xff, 0xa5, 0x00),
                        HighlightKind::AgentMatch => Color32::from_rgb(0x40, 0xa0, 0xff),
                        HighlightKind::AgentWarning => Color32::from_rgb(0xff, 0x60, 0x60),
                        HighlightKind::Annotation => Color32::from_rgb(0xe0, 0xac, 0x38),
                    };
                    let mx = (content_x - 10.0).max(log_rect.left());
                    ui.painter().rect_filled(
                        Rect::from_min_size(pos2(mx, y_cursor), vec2(3.0, line_height)),
                        0.0,
                        color,
                    );
                    break;
                }
            }

            // ---- caret (edit mode) ----
            // per-row 巨行：caret 必须落在 caret col 实际所在的 LaidLine 那一行；
            // 其它 LaidLine 跳过 caret / IME，但仍要画文本和选区（继续往下走）。
            // 关键：不能用 `engine.read_line(line_no).text` 取 nchars——10MB 巨行
            // decode ~30ms / 次，per-row 巨行每行都调一次就是几秒卡顿。
            // 这里改成：先看 cache（巨行拿 char_count，普通行才 read_line）。
            let mut caret_in_this_row: Option<(f32, egui::Rect)> = None;
            if let Some((cur_line, cur_col)) = app.edit_cursor {
                if line_no == cur_line {
                    // nchars：巨行拿缓存 char_count，普通行 read_line 数。
                    let nchars = if let Some(huge_ci) =
                        app.huge_chunk_cache.iter().position(|c| c.line == line_no)
                    {
                        app.huge_chunk_cache[huge_ci].char_count
                    } else {
                        engine.read_line(line_no).text.chars().count()
                    };
                    let cc = cur_col.min(nchars);
                    // caret 位置计算统一走 glyph 定位 + 区间检查：
                    //  - per-row 巨行（word_wrap 开或关都有）：每个 LaidLine 是单行，
                    //    chunk_char=该行字符起点。必须只在「cc 落在本 LaidLine 字符区间」
                    //    时画 caret，否则巨行每个视觉行都画一条 → 整高竖条（用户反馈）。
                    //  - 普通行：chunk_char=0，galley 整行（或多行 wrap），同样正确。
                    let galley = &l.galley;
                    let mut char_idx = l.chunk_char;
                    let mut found_x: Option<f32> = None;
                    for row in &galley.rows {
                        let row_start = char_idx;
                        char_idx += row.glyphs.len();
                        let row_end = char_idx;
                        if cc >= row_start && cc < row_end {
                            // 字符列 → 像素（metrics.char_to_x 统一）
                            found_x = Some(content_x + metrics.char_to_x(row, cc - row_start));
                            break;
                        }
                    }
                    if let Some(caret_x) = found_x {
                        let caret_rect = Rect::from_min_size(
                            pos2(caret_x, y_cursor),
                            vec2(1.5, line_height),
                        );
                        ui.painter().rect_filled(caret_rect, 0.0, Color32::from_rgb(226, 226, 226));
                        caret_in_this_row = Some((caret_x, caret_rect));
                    }
                    // 没找到 = caret 不在这一行（巨行换行后 caret col 落在别的视觉行）。
                    // 跳过 caret / IME，但下面仍画文本 / 选区。
                }
            }

            // ---- IME：只在 caret 所在行发布 IMEOutput ----
            if let Some((caret_x, caret_rect)) = caret_in_this_row {
                if app.edit_mode {
                    if !ctx.wants_keyboard_input() {
                        let ime_rect = Rect::from_min_size(
                            pos2(caret_x, y_cursor),
                            vec2(4.0, line_height),
                        );
                        ctx.output_mut(|o| {
                            o.ime = Some(egui::output::IMEOutput {
                                rect: ime_rect,
                                cursor_rect: caret_rect,
                            });
                        });
                    }
                    let preedit = app.edit_ime_preedit.clone();
                    if !preedit.is_empty() {
                        let pe_galley = ui.fonts(|f| {
                            let mut job = LayoutJob::default();
                            job.append(
                                &preedit,
                                0.0,
                                TextFormat {
                                    font_id: mono.clone(),
                                    color: theme.text_primary,
                                    underline: egui::Stroke::new(1.0_f32, theme.text_primary),
                                    ..Default::default()
                                },
                            );
                            f.layout_job(job)
                        });
                        ui.painter()
                            .galley(pos2(caret_x + 2.0, y_cursor), pe_galley, Color32::WHITE);
                    }
                }
            }

            // ---- selection highlight ----
            if let Some((s1_line, s1_col, s2_line, s2_col)) = app.selection {
                let (from_line, from_col, to_line, to_col) = if s1_line < s2_line
                    || (s1_line == s2_line && s1_col <= s2_col)
                {
                    (s1_line, s1_col, s2_line, s2_col)
                } else {
                    (s2_line, s2_col, s1_line, s1_col)
                };
                if line_no >= from_line && line_no <= to_line {
                    // 超长行分块（含 chunk_char==0 的首块）：nchars 用本块字符范围，
                    // 不读整行 6MB；普通行才读整行。
                    let is_huge_line =
                        huge_list.binary_search_by_key(&line_no, |&(l, _)| l).is_ok();
                    let is_huge_chunk = is_huge_line || l.chunk_char > 0;
                    let nchars = if is_huge_chunk {
                        l.chunk_char
                            + l.galley.rows.iter().map(|r| r.glyphs.len()).sum::<usize>()
                    } else {
                        engine.read_line(line_no).text.chars().count()
                    };
                    let sel_col_start = if line_no == from_line { from_col.min(nchars) } else { 0 };
                    let sel_col_end = if line_no == to_line { to_col.min(nchars) } else { nchars };
                    if sel_col_start < sel_col_end {
                        let hi_color = Color32::from_rgba_unmultiplied(80, 120, 200, 80);
                        // 分块行（或超长行的任一可见块）→ 行级高亮，且不用读整行。
                        let use_rows = word_wrap || is_huge_chunk;
                        if use_rows {
                            // Per-visual-row highlight using galley rows + glyph
                            // positions so selection follows the wrap exactly.
                            let galley = &l.galley;
                            // 分块行：字符索引从块起点开始（普通行 chunk_char=0）
                            let mut char_idx = l.chunk_char;
                            for row in &galley.rows {
                                let row_start = char_idx;
                                // 1 glyph per char for monospace log lines.
                                char_idx += row.glyphs.len();
                                let row_end = char_idx;
                                let lo = sel_col_start.max(row_start);
                                let hi = sel_col_end.min(row_end);
                                if lo >= hi {
                                    continue;
                                }
                                let row_y = y_cursor + row.rect.top();
                                let row_h_actual = row.rect.height();
                                // 字符列 → 像素（metrics.char_to_x 统一；lo/hi 是绝对字符，
                                // 转行内列 = -row_start）
                                let x0 = content_x + metrics.char_to_x(row, lo - row_start);
                                let x1 = content_x + metrics.char_to_x(row, hi - row_start);
                                if x1 > x0 {
                                    ui.painter().rect_filled(
                                        Rect::from_min_max(pos2(x0, row_y), pos2(x1, row_y + row_h_actual)),
                                        0.0, hi_color,
                                    );
                                }
                            }
                        } else {
                            // Single rect (non-wrap) — font measurement handles CJK.
                            // Whole-line selections reuse the already-laid-out
                            // galley width — no per-line font re-layout (that
                            // was the drag lag with large selections).
                            if sel_col_start == 0 && sel_col_end == nchars {
                                let sel_w = l.galley.rect.width();
                                if sel_w > 0.0 {
                                    ui.painter().rect_filled(
                                        Rect::from_min_size(
                                            pos2(content_x, y_cursor),
                                            vec2(sel_w, line_height),
                                        ),
                                        2.0, hi_color,
                                    );
                                }
                            } else {
                                // 非换行单块选择（普通行才到这）。
                                // **关键**：不能用 `metrics.text_w`（cells 估算宽度）—— tab 的实际
                                // 宽度是「跳到下一个 tab stop」（如 14px），cells('\t')=1 只算 1 格，
                                // 导致 prefix_w 估算偏 → 高亮偏（用户实测：tab 行选 base 高亮 atab）。
                                // 改用 galley 的 glyph 位置（char_to_x，像素级精确），与超长行/word_wrap
                                // 路径一致。
                                if let Some(row) = l.galley.rows.first() {
                                    let x0 = content_x + metrics.char_to_x(row, sel_col_start);
                                    let x1 = content_x + metrics.char_to_x(row, sel_col_end);
                                    if x1 > x0 {
                                        let sel_rect = Rect::from_min_max(
                                            pos2(x0, y_cursor),
                                            pos2(x1, y_cursor + line_height),
                                        );
                                        ui.painter().rect_filled(sel_rect, 2.0, hi_color);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ---- paint text galley ----
            ui.painter().galley(pos2(content_x, y_cursor), l.galley.clone(), Color32::WHITE);
        }

        ui.set_clip_rect(ui.max_rect());

        // ---- vertical scrollbar ----
        {
            let vsb_rect = Rect::from_min_size(
                pos2(log_rect.right() + gap as f32, log_rect.top()),
                vec2(sb_w as f32, log_h as f32),
            );
            ui.painter()
                .rect_filled(vsb_rect, 4.0, theme.scrollbar_track);

            let thumb_frac = if content_h > 0.0 {
                (log_h / content_h) as f32
            } else {
                1.0
            };
            let thumb_h = (vsb_rect.height() * thumb_frac).max(24.0);
            let thumb_y = vsb_rect.top()
                + (app.scroll_y / content_h) as f32 * vsb_rect.height();
            let thumb_rect = Rect::from_min_size(
                pos2(vsb_rect.left(), thumb_y),
                vec2(sb_w as f32, thumb_h),
            );

            let thumb_id = ui.make_persistent_id("v_scroll_thumb");
            let resp = ui.interact(thumb_rect, thumb_id, Sense::drag());
            let thumb_color = if resp.hovered() || app.scrollbar_dragging {
                theme.scrollbar_hover
            } else {
                theme.scrollbar_thumb
            };
            ui.painter().rect_filled(thumb_rect, 4.0, thumb_color);

            if resp.dragged() {
                app.scrollbar_dragging = true;
                let delta =
                    resp.drag_delta().y as f64 / vsb_rect.height() as f64 * content_h;
                app.scroll_y = (app.scroll_y + delta).clamp(0.0, max_v_scroll);
            } else {
                app.scrollbar_dragging = false;
            }

            if ui
                .interact(vsb_rect, ui.next_auto_id(), Sense::click())
                .clicked()
            {
                if let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) {
                    // Compute fraction in f64 to avoid precision loss when
                    // multiplying by large `content_h` (can be > 1e9 pixels).
                    let frac = ((pos.y as f64 - vsb_rect.top() as f64)
                        / vsb_rect.height() as f64).clamp(0.0, 1.0);
                    app.scroll_y = frac * content_h;
                }
            }
        }

        // ---- horizontal scrollbar (hidden when word-wrap is on or no overflow) ----
        if !app.word_wrap && max_h_scroll > 2.0 {
            let hsb_rect = Rect::from_min_size(
                pos2(log_rect.left(), log_rect.bottom() + gap as f32),
                vec2(log_w as f32, h_sb as f32),
            );
            ui.painter()
                .rect_filled(hsb_rect, 4.0, theme.scrollbar_track);

            // Total scrollable width = gutter + text + extra pad; consistent
            // with max_h_scroll so the thumb reaches the right edge at max.
            let effective_w2 = (text_extent + EXTRA_PAD).max(log_w + 1.0);
            let thumb_frac = (log_w / effective_w2).min(1.0) as f32;
            let thumb_w = (hsb_rect.width() * thumb_frac).max(28.0);
            let thumb_x = if max_h_scroll > 0.0 {
                hsb_rect.left()
                    + (app.h_scroll / effective_w2) as f32 * hsb_rect.width()
            } else {
                hsb_rect.left()
            };
            let thumb_x = thumb_x.min(hsb_rect.right() - thumb_w);
            let thumb_rect = Rect::from_min_size(
                pos2(thumb_x, hsb_rect.top()),
                vec2(thumb_w, h_sb as f32),
            );

            let thumb_id = ui.make_persistent_id("h_scroll_thumb");
            let resp = ui.interact(thumb_rect, thumb_id, Sense::drag());
            let thumb_color = if resp.hovered() || resp.dragged() {
                theme.scrollbar_hover
            } else {
                theme.scrollbar_thumb
            };
            ui.painter().rect_filled(thumb_rect, 4.0, thumb_color);

            if resp.dragged() {
                let delta =
                    resp.drag_delta().x as f64 / hsb_rect.width() as f64 * effective_w2;
                app.h_scroll = (app.h_scroll + delta).clamp(0.0, max_h_scroll);
            }

            if ui
                .interact(hsb_rect, ui.next_auto_id(), Sense::click())
                .clicked()
            {
                if let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) {
                    let frac = ((pos.x as f64 - hsb_rect.left() as f64)
                        / hsb_rect.width() as f64).clamp(0.0, 1.0);
                    app.h_scroll = (frac * effective_w2).min(max_h_scroll);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    // line_visual_rows / VisualRowModel 的测试已迁至 `layout::mapping`，这里不再重复。

    /// 验证 char_pos ↔ byte_pos 转换在含多字节字符 + 跨采样点时精确。
    /// 这是用户「选 version_id 复制出 ersion_id」的根因回归测试。
    #[test]
    fn char_pos_to_byte_pos_is_exact_across_stride() {
        // 每 5 字符一个中文（3 字节），总长超过 1024 字符，触发多个采样点
        let text: String = (0..3000)
            .map(|i| if i % 5 == 2 { '你' } else { 'a' })
            .collect();
        let byte_index = build_byte_index(&text);
        let chars: Vec<char> = text.chars().collect();

        // 验证采样点本身正确：byte_index[k] = (char_pos, chars[char_pos] 的字节偏移)
        for &(char_pos, byte_off) in &byte_index {
            let expected_byte = text
                .char_indices()
                .nth(char_pos)
                .map(|(b, _)| b)
                .unwrap_or(text.len());
            assert_eq!(byte_off, expected_byte,
                "采样点 char_pos={char_pos} 的 byte_off 错误");
        }

        // 验证 char_pos_to_byte_pos 对所有 char_pos 精确（含跨采样点）
        for cp in [0usize, 1, 5, 1023, 1024, 1025, 2047, 2048, 2049, chars.len()] {
            let expected = text.char_indices().nth(cp).map(|(b, _)| b).unwrap_or(text.len());
            let got = char_pos_to_byte_pos(&byte_index, &text, cp);
            assert_eq!(got, expected, "char_pos={cp} 转 byte_pos 错误");
        }

        // 关键回归：跨过第 1024 个采样点后，转换必须精确（旧 bug 会偏 1）
        let cp = 1500;
        let expected = text.char_indices().nth(cp).map(|(b, _)| b).unwrap();
        let got = char_pos_to_byte_pos(&byte_index, &text, cp);
        assert_eq!(got, expected, "跨采样点 char_pos={cp} 偏了（旧 build_byte_index bug）");
        // chars[1500] 是 'a'（1500%5==0），验证 got 处确实是 'a' 的字节位置
        assert_eq!(&text[got..got + 1], "a");
    }
}
