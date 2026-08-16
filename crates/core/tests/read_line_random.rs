//! Regression test for `Engine::read_line` random-access correctness.
//!
//! The sparse-index fast path in `read_line` used to fire for ANY forward read
//! inside the same 128-line bucket (`phys > last_resolved_line`), returning the
//! line at the cached position instead of the requested line.  A jump from line
//! 0 to line 39 would return line 1.  This corrupted the viewer's multi-line
//! highlight window scan (which reads `first` then `last-1`) and any
//! hit-testing read below the visible range.

use qview_core::engine::Engine;

fn make_file() -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("qview_readline_test_{}.txt", std::process::id()));
    let mut content = String::with_capacity(64 * 1024);
    for i in 0..256u32 {
        content.push_str(&format!("LINE_{:03}_{}\r\n", i, "x".repeat(i as usize % 20)));
    }
    std::fs::write(&path, &content).expect("write test file");
    path
}

#[test]
fn jump_within_sparse_bucket_returns_correct_line() {
    let path = make_file();
    let engine = Engine::new(path.clone()).expect("open");
    let total = engine.effective_line_count();
    assert_eq!(total, 256);

    let line_text = |i: u64| format!("LINE_{:03}_{}", i, "x".repeat(i as usize % 20));

    // Query construction reads line 4 then line 5 (sequential — fast path).
    assert_eq!(engine.read_line(4).text, line_text(4));
    assert_eq!(engine.read_line(5).text, line_text(5));

    // A jump from line 0 to line 39 (same 128-line sparse bucket) MUST return
    // line 39, not the cached line right after line 0.
    assert_eq!(engine.read_line(0).text, line_text(0));
    assert_eq!(engine.read_line(39).text, line_text(39));
    assert_eq!(engine.read_line(39).start_byte, engine.read_line(38).start_byte + engine.read_line(38).text.len() as u64 + 2);

    // Jump to the very last line after a mid-file read.
    assert_eq!(engine.read_line(200).text, line_text(200));
    assert_eq!(engine.read_line(255).text, line_text(255));
    // Last line must end at the file size (no `\n` after it).
    assert_eq!(engine.read_line(255).start_byte + engine.read_line(255).text.len() as u64 + 2, engine.mmap.size());

    // Sequential scan must still be exact after random access.
    for i in 0..256u64 {
        let raw = engine.read_line(i);
        let expect = format!("LINE_{:03}_{}", i, "x".repeat(i as usize % 20));
        assert_eq!(raw.text, expect, "line {i} content mismatch");
        assert_eq!(raw.start_byte, engine.read_line(i).start_byte, "line {i} unstable");
    }
    std::fs::remove_file(&path).ok();
}
