//! Quick test: read first line, print it, then truncate at various horiz
//! offsets, print result.

use std::env;
use std::path::PathBuf;

use anyhow::Result;
use qview::tui::render::fetch_raw;

fn main() -> Result<()> {
    let path = PathBuf::from(env::args().nth(1).expect("usage: debug-truncate <path>"));
    use qview::app::App;
    let mut app = App::new(path)?;
    app.build_index_blocking()?;
    let raw = fetch_raw(&app, 0);
    println!("line 0 raw: {:?}", raw.text);
    println!("line 0 byte_len: {}", raw.byte_len);

    let raw1 = fetch_raw(&app, 1);
    println!("line 1 raw: {:?}", raw1.text);
    println!("line 1 byte_len: {}", raw1.byte_len);

    // Truncate line 1 at horiz = 0, 8, 16, 24, 40:
    for h in [0usize, 8, 16, 24, 40, 80] {
        let (v, _tl, _tr) = truncate(&raw1.text, h, 200);
        println!("horiz={h} ({} chars) -> {:?}", v.len(), v);
    }

    Ok(())
}

fn truncate(s: &str, skip_cols: usize, take_cols: usize) -> (String, bool, bool) {
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
        clipped_left = skip_cols > 0;
        clipped_right = false;
    }
    (out, clipped_left, clipped_right)
}
