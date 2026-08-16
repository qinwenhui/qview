//! Log-line syntax highlighting. Single-pass token detection for timestamps,
//! log levels, IPs, UUIDs, key=value pairs, and HTTP status codes.

use ratatui::style::{Color, Modifier, Style};

use qview_core::cache::DisplayLine;

/// Style theme for log tokens.
pub fn style_for_level(level: &[u8]) -> Option<Style> {
    match level {
        b"ERROR" | b"FATAL" | b"CRIT" => Some(
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ),
        b"WARN" | b"WARNING" => Some(Style::default().fg(Color::Yellow)),
        b"INFO" | b"NOTICE" => Some(Style::default().fg(Color::Green)),
        b"DEBUG" => Some(Style::default().fg(Color::Cyan)),
        b"TRACE" => Some(Style::default().fg(Color::DarkGray)),
        _ => None,
    }
}

/// Build a fully-styled span list for a DisplayLine. Honors the line's
/// match ranges (search hits) and applies log-token coloring on top.
///
/// `text` is the already-truncated visible text; `horiz` is the column
/// offset at which the text starts in the source line.
pub fn style_spans(d: &DisplayLine) -> Vec<StyledSpan> {
    let text = &d.text;
    if text.is_empty() {
        return vec![StyledSpan::plain(text.clone())];
    }

    // 1. Collect log-token boundaries in source coordinates.
    let mut tokens: Vec<Token> = Vec::with_capacity(8);
    detect_tokens(text, &mut tokens);

    // 2. Merge search-match ranges (they take precedence: yellow background).
    let mut spans: Vec<StyledSpan> = Vec::new();
    if d.matches.is_empty() {
        spans = split_by_tokens(text, &tokens);
    } else {
        // Walk through the text, splitting at match boundaries, then for
        // non-match ranges apply token coloring.
        let mut last = 0usize;
        for &(s, e) in &d.matches {
            if s > last {
                push_split(&mut spans, &text[last..s], &tokens, last);
            }
            spans.push(StyledSpan {
                text: text[s..e].to_string(),
                style: Style::default()
                    .bg(Color::Yellow)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            });
            last = e;
        }
        if last < text.len() {
            push_split(&mut spans, &text[last..], &tokens, last);
        }
    }

    // 3. Prepend a left-clip indicator if the line is truncated on the left.
    if d.truncated_left {
        // No special rendering for now; the existing rendering shows the
        // visible text as-is.
    }

    spans
}

#[derive(Debug, Clone, Copy)]
pub struct Token {
    pub start: usize,
    pub end: usize,
    pub kind: TokenKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Timestamp,
    Level,
    Ip,
    Status,
    Uuid,
    Url,
    KeyValueKey,
    KeyValueValue,
    Number,
}

pub struct StyledSpan {
    pub text: String,
    pub style: Style,
}

impl StyledSpan {
    pub fn plain(text: String) -> Self {
        Self {
            text,
            style: Style::default(),
        }
    }
}

fn style_for_token(kind: TokenKind) -> Option<Style> {
    match kind {
        TokenKind::Timestamp => Some(Style::default().fg(Color::DarkGray)),
        TokenKind::Level => None, // handled by style_for_level
        TokenKind::Ip => Some(Style::default().fg(Color::Magenta)),
        TokenKind::Status => Some(Style::default().fg(Color::Blue)),
        TokenKind::Uuid => Some(Style::default().fg(Color::DarkGray)),
        TokenKind::Url => Some(Style::default().fg(Color::Cyan).add_modifier(Modifier::UNDERLINED)),
        TokenKind::KeyValueKey => Some(Style::default().fg(Color::Blue)),
        TokenKind::KeyValueValue => None,
        TokenKind::Number => Some(Style::default().fg(Color::Yellow)),
    }
}

/// Detect log tokens in `text`. Pushes (start, end, kind) tuples into `out`.
fn detect_tokens(text: &str, out: &mut Vec<Token>) {
    let bytes = text.as_bytes();
    let mut i = 0;

    // Timestamp: leading YYYY-MM-DD or YYYY/MM/DD
    if bytes.len() >= 10 {
        let year = std::str::from_utf8(&bytes[0..4]).ok();
        if let Some(y) = year {
            if y.chars().all(|c| c.is_ascii_digit()) {
                let sep = bytes[4];
                if (sep == b'-' || sep == b'/') && bytes[5..7].iter().all(|b| b.is_ascii_digit()) {
                    let sep2 = bytes[7];
                    if (sep2 == b'-' || sep2 == b'/') && bytes[8..10].iter().all(|b| b.is_ascii_digit())
                    {
                        // Find end: skip digits and common separators (T, :)
                        let mut j = 10;
                        while j < bytes.len() && is_ts_char(bytes[j]) {
                            j += 1;
                        }
                        out.push(Token {
                            start: 0,
                            end: j,
                            kind: TokenKind::Timestamp,
                        });
                        i = j;
                    }
                }
            }
        }
    }

    // Walk the rest looking for known token shapes.
    while i < bytes.len() {
        // Level: bracketed uppercase word like [INFO] or [ERROR]
        if bytes[i] == b'[' {
            if let Some(end) = match_bracketed_level(&bytes[i..]) {
                let word_start = i + 1;
                let word_end = i + end - 1;
                let level = &bytes[word_start..word_end];
                if let Some(style) = style_for_level(level) {
                    out.push(Token {
                        start: i,
                        end: i + end,
                        kind: TokenKind::Level,
                    });
                    let _ = style; // we apply per-span below
                }
                i += end;
                continue;
            }
        }

        // key=value or key="value with spaces"
        if let Some(end) = match_key_value(&bytes[i..]) {
            let (key_end, val_end) = end;
            out.push(Token {
                start: i,
                end: i + key_end,
                kind: TokenKind::KeyValueKey,
            });
            out.push(Token {
                start: i + key_end,
                end: i + val_end,
                kind: TokenKind::KeyValueValue,
            });
            i += val_end;
            continue;
        }

        // IPv4: \b\d+\.\d+\.\d+\.\d+\b
        if bytes[i].is_ascii_digit() {
            if let Some(end) = match_ipv4(&bytes[i..]) {
                out.push(Token {
                    start: i,
                    end: i + end,
                    kind: TokenKind::Ip,
                });
                i += end;
                continue;
            }
        }

        // UUID
        if bytes[i] == b'-' || bytes[i].is_ascii_hexdigit() {
            if let Some(end) = match_uuid(&bytes[i..]) {
                out.push(Token {
                    start: i,
                    end: i + end,
                    kind: TokenKind::Uuid,
                });
                i += end;
                continue;
            }
        }

        // HTTP status: status=NNN
        if i + 7 <= bytes.len() && &bytes[i..i + 7] == b"status=" {
            let mut j = i + 7;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 7 {
                out.push(Token {
                    start: i,
                    end: j,
                    kind: TokenKind::Status,
                });
                i = j;
                continue;
            }
        }

        i += 1;
    }
}

fn is_ts_char(b: u8) -> bool {
    b.is_ascii_digit() || b == b'T' || b == b':' || b == b'-' || b == b'.' || b == b'Z' || b == b'+'
}

fn match_bracketed_level(bytes: &[u8]) -> Option<usize> {
    if bytes.is_empty() || bytes[0] != b'[' {
        return None;
    }
    let mut j = 1;
    while j < bytes.len() && j < 16 && bytes[j] != b']' {
        if !bytes[j].is_ascii_uppercase() {
            return None;
        }
        j += 1;
    }
    if j < bytes.len() && bytes[j] == b']' {
        Some(j + 1)
    } else {
        None
    }
}

/// Match `key=value` or `key="value"`. Returns (key_end, value_end) within
/// `bytes`, where `key_end` is the index just past `=`.
///
/// If the value contains an IP address or numeric token, the caller is
/// responsible for splitting the value further in `detect_tokens`. We just
/// report the boundaries here.
fn match_key_value(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut j = 0;
    // key: [a-zA-Z_][a-zA-Z0-9_.\-]*
    if j >= bytes.len() || !(bytes[j].is_ascii_alphabetic() || bytes[j] == b'_') {
        return None;
    }
    j += 1;
    while j < bytes.len() && j < 40
        && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'.' || bytes[j] == b'-')
    {
        j += 1;
    }
    if j >= bytes.len() || bytes[j] != b'=' {
        return None;
    }
    let key_end = j + 1;
    let mut k = key_end;
    if k < bytes.len() && (bytes[k] == b'"' || bytes[k] == b'\'') {
        let quote = bytes[k];
        k += 1;
        while k < bytes.len() && bytes[k] != quote {
            k += 1;
        }
        if k < bytes.len() && bytes[k] == quote {
            k += 1;
        }
    } else {
        while k < bytes.len()
            && !bytes[k].is_ascii_whitespace()
            && bytes[k] != b','
            && bytes[k] != b']'
        {
            k += 1;
        }
    }
    Some((key_end, k))
}

fn match_ipv4(bytes: &[u8]) -> Option<usize> {
    let mut j = 0;
    let mut dots = 0;
    let mut group_len = 0;
    while j < bytes.len() {
        if bytes[j].is_ascii_digit() {
            group_len += 1;
            if group_len > 3 {
                return None;
            }
        } else if bytes[j] == b'.' {
            if group_len == 0 {
                return None;
            }
            dots += 1;
            group_len = 0;
            if dots > 3 {
                return None;
            }
        } else {
            break;
        }
        j += 1;
    }
    if dots == 3 && group_len > 0 {
        Some(j)
    } else {
        None
    }
}

fn match_uuid(bytes: &[u8]) -> Option<usize> {
    // 8-4-4-4-12 hex with dashes, or 32 hex
    let hex = |b: u8| b.is_ascii_hexdigit();
    let target_with_dashes: [usize; 5] = [8, 4, 4, 4, 12];
    let mut j = 0;
    let mut g = 0;
    let mut ok = true;
    while g < 5 {
        let need = target_with_dashes[g];
        if g > 0 {
            if j >= bytes.len() || bytes[j] != b'-' {
                ok = false;
                break;
            }
            j += 1;
        }
        for _ in 0..need {
            if j >= bytes.len() || !hex(bytes[j]) {
                ok = false;
                break;
            }
            j += 1;
        }
        if !ok {
            break;
        }
        g += 1;
    }
    if ok && g == 5 {
        return Some(j);
    }
    // Try 32-char hex
    let mut k = 0;
    while k < 32 && k < bytes.len() && hex(bytes[k]) {
        k += 1;
    }
    if k == 32 {
        Some(k)
    } else {
        None
    }
}

/// Split `text` into spans per `tokens`, looking up styles.
fn split_by_tokens(text: &str, tokens: &[Token]) -> Vec<StyledSpan> {
    if tokens.is_empty() {
        return vec![StyledSpan::plain(text.to_string())];
    }
    let mut spans = Vec::new();
    push_split(&mut spans, text, tokens, 0);
    spans
}

/// Helper: split `text` according to the tokens overlapping with offset `base`.
fn push_split(spans: &mut Vec<StyledSpan>, text: &str, tokens: &[Token], base: usize) {
    if text.is_empty() {
        return;
    }
    let end = base + text.len();
    let mut cursor = 0usize;
    for t in tokens.iter().filter(|t| t.end > base && t.start < end) {
        let s = t.start.max(base) - base;
        let e = t.end.min(end) - base;
        if s > cursor {
            spans.push(StyledSpan::plain(text[cursor..s].to_string()));
        }
        if e > s {
            let style = match t.kind {
                TokenKind::Level => {
                    // Token text is "[LEVEL]" or a clipped subset — strip
                    // leading '[' / trailing ']' to get the inner word.
                    let level_text = &text[s..e];
                    let inner = level_text
                        .trim_start_matches('[')
                        .trim_end_matches(']');
                    style_for_level(inner.as_bytes()).unwrap_or_default()
                }
                _ => style_for_token(t.kind).unwrap_or_default(),
            };
            spans.push(StyledSpan {
                text: text[s..e].to_string(),
                style,
            });
            cursor = e;
        }
    }
    if cursor < text.len() {
        spans.push(StyledSpan::plain(text[cursor..].to_string()));
    }
}