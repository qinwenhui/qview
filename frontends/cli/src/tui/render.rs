//! Terminal rendering. Each frame does O(visible_rows) work through the
//! two-tier cache; misses fall back to the mmap via the line index.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::{App, Mode};
use qview_core::cache::{DisplayKey, DisplayLine, RawLine};
use crate::tui::tokenize;

/// Everything the status line + title needs. Owned strings to allow the caller
/// to drop its borrows of `app` before the frame is drawn.
pub struct StatusInfo {
    pub total_lines: u64,
    pub file_size: u64,
    pub indexed: bool,
    pub search_query: String,
    pub search_hits: usize,
    pub search_cursor: Option<usize>,
    pub mode: Mode,
    pub message: Option<String>,
    pub input_buffer: String,
    pub file_path: String,
    pub tail_mode: bool,
    pub show_help: bool,
    /// True when there are unsaved in-memory edits.
    pub modified: bool,
    /// P5：器灵当前阶段（None = 空闲）。
    pub agent_phase: Option<String>,
}

/// Main render entrypoint. Called every frame.
///
/// Performance: per-frame work is O(visible_rows) hash lookups in the
/// two-tier cache. On hit, no string allocation. On miss, a single mmap
/// slice + tokenizer pass fills both tiers. Ratatui's built-in diff
/// strategy then only redraws cells that changed in the final buffer.
pub fn render(frame: &mut Frame, app: &mut App, info: StatusInfo) {
    let area = frame.area();

    // Clear the frame first — without this, residue from previous frames
    // leaks through when widget sizes change, especially on Windows cmd.
    frame.render_widget(Clear, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    draw_title(
        frame,
        chunks[0],
        &info.file_path,
        info.file_size,
        info.total_lines,
        info.indexed,
        info.tail_mode,
        info.modified,
    );

    let viewport_height = chunks[1].height;
    // Gutter: 4 chars (marker + space) + 6 chars (line number + space) = 10.
    // Fixed width prevents the TUI from wrapping mid-line.
    let viewport_width = chunks[1].width.saturating_sub(10);
    let main_area = chunks[1];

    draw_viewport(frame, main_area, app, viewport_width, viewport_height);

    let needs_cmd_line = matches!(
        info.mode,
        Mode::Command | Mode::Search | Mode::SearchEdit
    );
    let cmd_area = Rect {
        x: 0,
        y: chunks[2].y.saturating_sub(1),
        width: area.width,
        height: 1,
    };
    let cmd_mode = info.mode;
    let cmd_buf = &info.input_buffer;
    draw_status(frame, chunks[2], &info);

    if needs_cmd_line {
        draw_command_line(frame, cmd_area, cmd_mode, cmd_buf);
    }

    if info.show_help {
        draw_help(frame, area);
    }
}

fn draw_title(
    frame: &mut Frame,
    area: Rect,
    path: &str,
    size: u64,
    total_lines: u64,
    indexed: bool,
    tail_mode: bool,
    modified: bool,
) {
    let idx = if indexed { "[idx]" } else { "[...]" };
    let tail = if tail_mode { " [TAIL]" } else { "" };
    let mod_indicator = if modified { " [+]" } else { "" };
    let text = format!(
        " qview  {}{}{}  {}  size={}  lines={} ",
        path, tail, mod_indicator, idx, human_bytes(size), total_lines
    );
    let p = Paragraph::new(text).style(
        Style::default()
            .bg(Color::DarkGray)
            .fg(if tail_mode || modified { Color::Yellow } else { Color::White }),
    );
    frame.render_widget(p, area);
}

fn draw_viewport(frame: &mut Frame, area: Rect, app: &mut App, width: u16, height: u16) {
    let top = app.viewport.top_line;
    let horiz = app.viewport.horiz_offset;

    // Snapshot the things we need (immutable) before going &mut on app.
    let total_lines = app.effective_line_count();
    let active_search: String = if matches!(app.mode, Mode::SearchEdit)
        && !app.input_buffer.is_empty()
    {
        app.input_buffer.clone()
    } else {
        app.engine.search_query.clone()
    };
    let search_hash_base = app.engine.search_hash;
    let live_hash = if !active_search.is_empty() {
        xxhash_rust::xxh3::xxh3_64(active_search.as_bytes())
    } else {
        search_hash_base
    };
    let finder: Option<memchr::memmem::Finder> = if !active_search.is_empty() {
        Some(memchr::memmem::Finder::new(active_search.as_bytes()))
    } else {
        None
    };
    let live_key = DisplayKey {
        width,
        horiz,
        search_hash: live_hash,
    };

    // Pre-build the ListItems.
    let items: Vec<ListItem> = (0..height as u64)
        .map(|i| {
            let line_no = top + i;
            if line_no >= total_lines {
                return ListItem::new(Line::from(""));
            }

            // 1. Try display cache.
            if let Some(d) = app.engine.cache.get_display(line_no, live_key) {
                return line_to_item(line_no, d);
            }

            // 2. Try raw cache; fall back to mmap.
            let raw_line = if let Some(r) = app.engine.cache.get_raw(line_no) {
                r.clone()
            } else {
                let r = fetch_raw(app, line_no);
                app.engine.cache.put_raw(line_no, r.clone());
                r
            };

            // 3. Build display entry.
            let mut display = build_display(&raw_line, horiz, width, finder.as_ref());
            display.modified = raw_line.modified;
            app.engine
                .cache
                .put_display(line_no, live_key, display.clone());
            line_to_item(line_no, &display)
        })
        .collect();

    let block = Block::default().borders(Borders::NONE);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    // Clear viewport before drawing: residual cells from truncated
    // content cause visual duplication, especially when scrolling horizontally.
    frame.render_widget(Clear, inner);
    let list = List::new(items);
    frame.render_widget(list, inner);
}

fn line_to_item(line_no: u64, d: &DisplayLine) -> ListItem<'static> {
    // Gutter total width is fixed at 10 columns:
    //   - 4-char marker: "[+]" + " " when modified, "    " otherwise
    //   - 6-char line number: "{:>5} "
    // Keeping the gutter at a fixed width prevents mid-row wrap that would
    // otherwise corrupt the terminal display.
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(d.matches.len() * 2 + 3);
    if d.modified {
        spans.push(Span::styled(
            "[+]",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(" ", Style::default()));
    } else {
        spans.push(Span::styled("    ", Style::default()));
    }
    spans.push(Span::styled(
        format!("{:>5} ", line_no + 1),
        Style::default().fg(Color::DarkGray),
    ));

    if d.text.is_empty() {
        return ListItem::new(Line::from(spans));
    }

    // Get base styled spans (tokens applied).
    let base = tokenize::style_spans(d);

    if d.matches.is_empty() {
        for s in base {
            spans.push(Span::styled(s.text, s.style));
        }
    } else {
        // Walk the matches, slicing base spans by match ranges.
        // We need to split each base span when a match crosses it.
        for s in base {
            split_base_by_matches(&s.text, s.style, &d.matches, &mut spans);
        }
    }

    ListItem::new(Line::from(spans))
}

/// Split `text` (already styled with `base`) by `matches`, yielding spans
/// where the matched ranges get the highlight style.
fn split_base_by_matches(
    text: &str,
    base: Style,
    matches: &[(usize, usize)],
    out: &mut Vec<Span<'static>>,
) {
    if matches.is_empty() {
        out.push(Span::styled(text.to_string(), base));
        return;
    }
    let mut last = 0usize;
    for &(s, e) in matches {
        if s >= text.len() {
            break;
        }
        let e = e.min(text.len());
        if s > last {
            out.push(Span::styled(text[last..s].to_string(), base));
        }
        out.push(Span::styled(
            text[s..e].to_string(),
            Style::default()
                .bg(Color::Yellow)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ));
        last = e;
    }
    if last < text.len() {
        out.push(Span::styled(text[last..].to_string(), base));
    }
}

/// Fetch a line's raw bytes from the mmap via the index. No caching here;
/// caller handles cache.
pub fn fetch_raw(app: &App, line_no: u64) -> RawLine {
    // Routed through `App::read_line` so the edit buffer is honored.
    app.read_line(line_no)
}

/// Build a display-ready line: truncate, then locate match positions for
/// highlight. Returns owned strings so the entry can live in the LRU.
fn build_display(
    raw: &RawLine,
    horiz: u16,
    width: u16,
    finder: Option<&memchr::memmem::Finder>,
) -> DisplayLine {
    let horiz = horiz as usize;
    let width = width as usize;

    // Truncate horizontally: skip `horiz` display columns from the left, then
    // take up to `width` columns.
    let (visible, truncated_left, truncated_right) =
        truncate_columns(&raw.text, horiz, width);

    // Locate match positions within `visible`.
    let matches = if let Some(f) = finder {
        f.find_iter(visible.as_bytes())
            .map(|m| (m, (m + f.needle().len()).min(visible.len())))
            .collect()
    } else {
        Vec::new()
    };

    DisplayLine {
        text: visible,
        matches,
        truncated_left,
        truncated_right,
        modified: false,
    }
}

/// Truncate `s` by display columns. Returns (visible_text, clipped_left, clipped_right).
fn truncate_columns(s: &str, skip_cols: usize, take_cols: usize) -> (String, bool, bool) {
    if s.is_empty() || take_cols == 0 {
        return (String::new(), skip_cols > 0, false);
    }
    let mut out = String::with_capacity(s.len().min(take_cols.saturating_mul(4)));
    let mut skipped = 0usize;
    let mut done_skipping = skip_cols == 0;
    let mut taken = 0usize;
    let mut clipped_right = false;
    let mut clipped_left = skip_cols > 0;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if !done_skipping {
            if skipped + cw <= skip_cols {
                skipped += cw;
                continue;
            }
            done_skipping = true;
        }
        if taken + cw > take_cols {
            clipped_right = true;
            break;
        }
        out.push(ch);
        taken += cw;
    }
    if !done_skipping {
        // entire string was skipped
        clipped_left = skip_cols > 0;
        clipped_right = false;
    }
    (out, clipped_left, clipped_right)
}

fn draw_status(frame: &mut Frame, area: Rect, info: &StatusInfo) {
    let search = if !info.search_query.is_empty() {
        if info.search_hits > 0 {
            let cur = info.search_cursor.map(|c| c + 1).unwrap_or(0);
            format!("  /{}/ {}/{}", info.search_query, cur, info.search_hits)
        } else {
            format!("  /{}/ no hits", info.search_query)
        }
    } else {
        String::new()
    };

    let mode = match info.mode {
        Mode::Normal => "-- NORMAL --",
        Mode::Search => "-- SEARCH --",
        Mode::Command => "-- COMMAND --",
        Mode::SearchEdit => "-- SEARCH (editing) --",
        Mode::Operator(c) => match c {
            'd' => "-- OPERATOR (d) --",
            'y' => "-- OPERATOR (y) --",
            _ => "-- OPERATOR --",
        },
        Mode::Visual => "-- VISUAL --",
    };

    let msg = info.message.as_deref().unwrap_or("");
    let tail_indicator = if info.tail_mode { "  [tail]" } else { "" };
    let modified_indicator = if info.modified { "  [Modified]" } else { "" };
    let agent_indicator = match info.agent_phase.as_deref() {
        Some(p) => format!("  [AI:{}]", p),
        None => String::new(),
    };

    let text = format!(
        " {} | lines: {} | bytes: {}{}{}{}{}  {} ",
        mode,
        info.total_lines,
        human_bytes(info.file_size),
        search,
        tail_indicator,
        modified_indicator,
        agent_indicator,
        msg
    );
    let p = Paragraph::new(text).style(
        Style::default()
            .bg(Color::DarkGray)
            .fg(if info.tail_mode || info.modified { Color::Yellow } else { Color::White }),
    );
    frame.render_widget(p, area);
}

fn draw_command_line(frame: &mut Frame, area: Rect, mode: Mode, buf: &str) {
    let prefix = match mode {
        Mode::Search | Mode::SearchEdit => "/",
        Mode::Command => ":",
        Mode::Normal => "",
        Mode::Operator(_) => "",
        Mode::Visual => "",
    };
    let text = format!("{}{}", prefix, buf);
    let p = Paragraph::new(text).style(Style::default().bg(Color::Black).fg(Color::Green));
    frame.render_widget(p, area);
}

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

fn draw_help(frame: &mut Frame, area: Rect) {
    let lines: Vec<Line> = vec![
        Line::from(" qview — Help "),
        Line::from(""),
        Line::from("  Movement"),
        Line::from("    j / k           scroll down / up one line"),
        Line::from("    d / u           half-page down / up"),
        Line::from("    space / PgDn    page down"),
        Line::from("    PgUp            page up"),
        Line::from("    g / G           top / bottom"),
        Line::from("    h / l, ← / →    scroll left / right"),
        Line::from(""),
        Line::from("  Search"),
        Line::from("    /pattern        literal search (case-sensitive)"),
        Line::from("                    live highlight while typing"),
        Line::from("    n / N           next / prev hit"),
        Line::from("    ] / [           jump 10 hits"),
        Line::from("    } / {           last / first hit"),
        Line::from(""),
        Line::from("  Commands"),
        Line::from("    :<N>            jump to line N (1-based)"),
        Line::from("    :t              toggle tail -f mode"),
        Line::from("    :q              quit"),
        Line::from(""),
        Line::from("  Misc"),
        Line::from("    F               toggle tail follow"),
        Line::from("    ?               toggle this help"),
        Line::from("    Ctrl-C          quit"),
        Line::from(""),
        Line::from("  Press ? to close"),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help ")
        .style(Style::default().bg(Color::Black).fg(Color::White));
    let p = Paragraph::new(lines).block(block);
    // Center the overlay.
    let w = 60.min(area.width.saturating_sub(2));
    let h = 32.min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let overlay = Rect::new(x, y, w, h);
    frame.render_widget(p, overlay);
}