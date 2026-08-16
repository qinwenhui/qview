//! Integration tests for the edit buffer + LineView + writeback.
//!
//! These tests use a temp file so we exercise the real mmap + index path,
//! not a hand-rolled fake.

use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

use qview::app::{parse_substitute, App};
use qview_core::edit::{EditBuffer, LineEditor};
use qview_core::file::LineIndex;

/// 临时文件唯一名计数器（`as_nanos()` 在本机分辨率不足，并行测试会撞名互删文件）。
static TMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Write `contents` to a fresh temp file and return the path.
fn temp_log(contents: &str) -> std::path::PathBuf {
    let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir()
        .join(format!("qview-edit-{}-{}.log", std::process::id(), seq));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(contents.as_bytes()).unwrap();
    f.flush().unwrap();
    path
}

fn open_app(contents: &str) -> (App, std::path::PathBuf) {
    let path = temp_log(contents);
    let mut app = App::new(path.clone()).unwrap();
    app.build_index_blocking().unwrap();
    (app, path)
}

// ---------- EditBuffer basics ----------

#[test]
fn edit_buffer_new_is_empty() {
    let eb = EditBuffer::new();
    assert!(eb.is_empty());
    assert!(!eb.dirty);
    assert_eq!(eb.edit_count(), 0);
    assert_eq!(eb.net_line_delta(), 0);
}

#[test]
fn edit_buffer_inserts_change_line_count() {
    let mut eb = EditBuffer::new();
    eb.inserted.insert(0, vec![b"Z".to_vec(), b"Y".to_vec()]);
    eb.deleted.insert(1);
    assert_eq!(eb.net_line_delta(), 1); // +2 -1
    eb.dirty = true;
}

// ---------- LineView resolution ----------

#[test]
fn line_view_resolve_basic() {
    // No trailing \n — avoids the index's "extra empty line past EOF" quirk.
    let path = temp_log("a\nb\nc");
    let mmap = qview_core::file::MmapBackend::open(&path).unwrap();
    let builder = qview_core::file::IndexBuilder::new(mmap.clone());
    let offsets = builder.build_with_progress(|_, _| {}).unwrap();
    let index = LineIndex::from_vec(offsets, mmap.size());

    let edits = EditBuffer::new();
    let view = qview_core::edit::LineView::new(&mmap, &index, &edits, &edits.mapping);
    assert_eq!(view.resolve(0).unwrap(), b"a");
    assert_eq!(view.resolve(1).unwrap(), b"b");
    assert_eq!(view.resolve(2).unwrap(), b"c");
    assert!(view.resolve(3).is_none());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn line_view_resolve_with_delete() {
    let path = temp_log("a\nb\nc");
    let mmap = qview_core::file::MmapBackend::open(&path).unwrap();
    let builder = qview_core::file::IndexBuilder::new(mmap.clone());
    let offsets = builder.build_with_progress(|_, _| {}).unwrap();
    let index = LineIndex::from_vec(offsets, mmap.size());

    let mut edits = EditBuffer::new();
    edits.deleted.insert(1);
    edits.rebuild_mapping();
    let view = qview_core::edit::LineView::new(&mmap, &index, &edits, &edits.mapping);
    assert_eq!(view.resolve(0).unwrap(), b"a");
    assert_eq!(view.resolve(1).unwrap(), b"c"); // b is gone
    assert!(view.resolve(2).is_none());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn line_view_resolve_with_replace() {
    let path = temp_log("a\nb\nc\n");
    let mmap = qview_core::file::MmapBackend::open(&path).unwrap();
    let builder = qview_core::file::IndexBuilder::new(mmap.clone());
    let offsets = builder.build_with_progress(|_, _| {}).unwrap();
    let index = LineIndex::from_vec(offsets, mmap.size());

    let mut edits = EditBuffer::new();
    edits.replaced.insert(1, b"BB".to_vec());
    // `replaced` doesn't change mapping (line count is preserved).
    let view = qview_core::edit::LineView::new(&mmap, &index, &edits, &edits.mapping);
    assert_eq!(view.resolve(0).unwrap(), b"a");
    assert_eq!(view.resolve(1).unwrap(), b"BB");
    assert_eq!(view.resolve(2).unwrap(), b"c");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn line_view_resolve_with_insert() {
    let path = temp_log("a\nb\nc\n");
    let mmap = qview_core::file::MmapBackend::open(&path).unwrap();
    let builder = qview_core::file::IndexBuilder::new(mmap.clone());
    let offsets = builder.build_with_progress(|_, _| {}).unwrap();
    let index = LineIndex::from_vec(offsets, mmap.size());

    let mut edits = EditBuffer::new();
    // Insert "X" after physical line 0 (i.e. between a and b).
    edits.inserted.insert(0, vec![b"X".to_vec()]);
    edits.rebuild_mapping();
    let view = qview_core::edit::LineView::new(&mmap, &index, &edits, &edits.mapping);
    assert_eq!(view.resolve(0).unwrap(), b"a");
    assert_eq!(view.resolve(1).unwrap(), b"X");
    assert_eq!(view.resolve(2).unwrap(), b"b");
    assert_eq!(view.resolve(3).unwrap(), b"c");

    let _ = std::fs::remove_file(&path);
}

// ---------- LineEditor mutations ----------

#[test]
fn line_editor_replace_records_undo() {
    let path = temp_log("a\nb\nc\n");
    let mmap = qview_core::file::MmapBackend::open(&path).unwrap();
    let builder = qview_core::file::IndexBuilder::new(mmap.clone());
    let offsets = builder.build_with_progress(|_, _| {}).unwrap();
    let index = LineIndex::from_vec(offsets, mmap.size());

    let mut edits = EditBuffer::new();
    let mut editor = LineEditor::new(&mmap, &index, &mut edits);
    let old = editor.replace_line(1, b"BB".to_vec());
    assert_eq!(old, Some(b"b".to_vec()));
    assert_eq!(edits.undo_count(), 1);
    assert!(edits.dirty);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn line_editor_undo_round_trip() {
    let path = temp_log("a\nb\nc\n");
    let mmap = qview_core::file::MmapBackend::open(&path).unwrap();
    let builder = qview_core::file::IndexBuilder::new(mmap.clone());
    let offsets = builder.build_with_progress(|_, _| {}).unwrap();
    let index = LineIndex::from_vec(offsets, mmap.size());

    let mut edits = EditBuffer::new();
    {
        let mut editor = LineEditor::new(&mmap, &index, &mut edits);
        editor.replace_line(1, b"BB".to_vec());
    }
    assert!(edits.replaced.contains_key(&1));
    {
        let mut editor = LineEditor::new(&mmap, &index, &mut edits);
        let ok = editor.undo();
        assert!(ok);
    }
    // After undo of replace: replaced should contain the original "b".
    assert_eq!(edits.replaced.get(&1).unwrap(), &b"b".to_vec());

    let _ = std::fs::remove_file(&path);
}

// ---------- :s/foo/bar/ parsing ----------

#[test]
fn parse_substitute_basic() {
    let (pat, repl, global) = parse_substitute("/foo/bar/").unwrap();
    assert_eq!(pat, "foo");
    assert_eq!(repl, "bar");
    assert!(!global);
}

#[test]
fn parse_substitute_with_g() {
    let (pat, repl, global) = parse_substitute(",foo,bar,g").unwrap();
    assert_eq!(pat, "foo");
    assert_eq!(repl, "bar");
    assert!(global);
}

#[test]
fn parse_substitute_alt_delim() {
    let (pat, repl, global) = parse_substitute("|192.168.0.1|10.0.0.1|").unwrap();
    assert_eq!(pat, "192.168.0.1");
    assert_eq!(repl, "10.0.0.1");
    assert!(!global);
}

// ---------- App end-to-end: dd + :w round-trip ----------

#[test]
fn app_dd_writes_back() {
    let (mut app, path) = open_app("alpha\nbeta\ngamma\ndelta\n");

    // Cursor on line 1 (0-indexed) = "beta".
    app.viewport.top_line = 1;
    let deleted = app.delete_logical_line(1);
    assert!(deleted);
    assert!(app.is_modified());
    assert_eq!(app.effective_line_count(), 3);

    app.save().unwrap();
    assert!(!app.is_modified());

    // Read the on-disk content.
    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, "alpha\ngamma\ndelta\n");

    // Clean up.
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("log.bak"));
}

#[test]
fn app_substitute_command() {
    let (mut app, path) = open_app("alpha\nbeta\ngamma\n");
    app.viewport.top_line = 1;
    app.input_buffer = "s/beta/BETA/".to_string();
    app.submit_command().unwrap();
    assert!(app.is_modified());
    app.save().unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, "alpha\nBETA\ngamma\n");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("log.bak"));
}

#[test]
fn app_q_quits() {
    let (mut app, _path) = open_app("a\nb\n");
    app.input_buffer = "q".to_string();
    app.submit_command().unwrap();
    assert!(app.should_quit);
}

#[test]
fn app_q_bang_discards_edits() {
    let (mut app, _path) = open_app("a\nb\n");
    app.viewport.top_line = 0;
    app.delete_logical_line(0);
    assert!(app.is_modified());
    app.input_buffer = "q!".to_string();
    app.submit_command().unwrap();
    assert!(app.should_quit);
    assert!(!app.is_modified());
}

#[test]
fn app_e_bang_reload() {
    let (mut app, _path) = open_app("a\nb\n");
    app.viewport.top_line = 0;
    app.delete_logical_line(0);
    assert!(app.is_modified());
    app.input_buffer = "e!".to_string();
    app.submit_command().unwrap();
    assert!(!app.is_modified());
    assert_eq!(app.effective_line_count(), 2);
}