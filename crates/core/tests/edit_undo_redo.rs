//! Integration tests for edit undo/redo, typing coalescing, atomic batches,
//! and writeback (save + backup). Uses a real temp file so the mmap + index
//! path is exercised.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use qview_core::engine::Engine;

/// 临时文件唯一名计数器（`as_nanos()` 在本机分辨率不足，并行测试会撞名互删文件）。
static TMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Create an engine over `contents`, indexed synchronously.
fn open(contents: &str) -> (Engine, PathBuf) {
    let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir()
        .join(format!("qview-edit-undo-{}-{}.log", std::process::id(), seq));
    std::fs::write(&path, contents).unwrap();
    let mut e = Engine::new(path.clone()).unwrap();
    e.build_index_blocking().unwrap();
    (e, path)
}

/// Engine's logical-line text with the trailing newline stripped.
fn line(e: &Engine, n: u64) -> String {
    e.read_line(n)
        .text
        .trim_end_matches('\n')
        .trim_end_matches('\r')
        .to_string()
}

#[test]
fn typing_burst_is_one_undo_step_and_redos() {
    let (mut e, path) = open("aaa\nbbb\nccc");
    // Three keystrokes on line 1 coalesce into a single undo step.
    e.replace_logical_line(1, b"bbbx".to_vec());
    e.replace_logical_line(1, b"bbbxy".to_vec());
    e.replace_logical_line(1, b"bbbxyz".to_vec());
    assert_eq!(line(&e, 1), "bbbxyz");

    assert!(e.undo_one());
    assert_eq!(line(&e, 1), "bbb", "burst must undo in one step");
    assert!(!e.undo_one(), "second undo must be a no-op");

    assert!(e.redo_one());
    assert_eq!(line(&e, 1), "bbbxyz", "redo restores the full burst");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn batch_split_is_one_undo_step() {
    let (mut e, path) = open("abc\ndef");
    e.begin_edit_batch();
    e.replace_logical_line(0, b"ab".to_vec());
    e.insert_logical_line_after(0, b"c".to_vec());
    e.end_edit_batch();
    assert_eq!(line(&e, 0), "ab");
    assert_eq!(line(&e, 1), "c");
    assert_eq!(line(&e, 2), "def");

    assert!(e.undo_one());
    assert_eq!(line(&e, 0), "abc", "whole split undone at once");
    assert_eq!(line(&e, 1), "def");
    assert_eq!(e.effective_line_count(), 2);

    assert!(e.redo_one());
    assert_eq!(line(&e, 0), "ab");
    assert_eq!(line(&e, 1), "c");
    assert_eq!(line(&e, 2), "def");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn insert_after_inserted_line_and_undo() {
    let (mut e, path) = open("aaa\nbbb\nccc");
    assert!(e.insert_logical_line_after(0, b"x".to_vec()));
    assert!(e.insert_logical_line_after(1, b"y".to_vec())); // line 1 is inserted
    assert_eq!(line(&e, 0), "aaa");
    assert_eq!(line(&e, 1), "x");
    assert_eq!(line(&e, 2), "y");
    assert_eq!(line(&e, 3), "bbb");

    assert!(e.undo_one());
    assert_eq!(line(&e, 1), "x");
    assert_eq!(line(&e, 2), "bbb");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn save_writes_back_and_backs_up_original() {
    let (mut e, path) = open("aaa\nbbb\nccc");
    e.replace_logical_line(1, b"BBB".to_vec());
    e.replace_logical_line(2, b"CCC-D".to_vec());
    e.save().unwrap();

    let saved = std::fs::read_to_string(&path).unwrap();
    assert_eq!(saved, "aaa\nBBB\nCCC-D\n");

    // The engine's live view must reflect the new content (line-cache reset).
    assert_eq!(line(&e, 0), "aaa");
    assert_eq!(line(&e, 1), "BBB");
    assert_eq!(line(&e, 2), "CCC-D");
    assert!(!e.is_modified(), "edits cleared after save");

    // path "x.log" → with_extension("log.bak") = "x.log.bak"
    let backup = path.with_extension("log.bak");
    let orig = std::fs::read_to_string(&backup).unwrap();
    assert_eq!(orig, "aaa\nbbb\nccc", "backup holds the ORIGINAL content");

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&backup);
}

#[test]
fn background_save_completes() {
    let (mut e, path) = open("aaa\nbbb\nccc");
    e.replace_logical_line(0, b"AAA".to_vec());
    assert!(e.submit_save(), "background save must start");
    assert!(!e.submit_save(), "second submit must be refused while in flight");

    // Poll until done (bounded loop so a hang fails instead of spinning).
    let mut saved_ok = false;
    for _ in 0..200 {
        let (done, msg, ok) = e.poll_bg_save();
        if done {
            assert!(ok, "save should succeed, got {msg:?}");
            assert!(
                msg.as_deref().is_some_and(|m| m.contains("保存完成")),
                "success message must include the result, got {msg:?}"
            );
            saved_ok = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(saved_ok, "save never finished");
    assert!(!e.save_in_flight(), "save should have finished");

    let saved = std::fs::read_to_string(&path).unwrap();
    assert_eq!(saved, "AAA\nbbb\nccc"); // original had no trailing \n — preserved
    let backup = path.with_extension("log.bak");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&backup);
}

#[test]
fn one_line_seed_is_editable_like_a_new_file() {
    // The GUI's 新建 feature backs a new file with a temp file containing "\n",
    // which the engine must see as ONE editable empty line.
    let (mut e, path) = open("\n");
    assert_eq!(e.effective_line_count(), 1);
    assert!(e.replace_logical_line(0, b"hello".to_vec()), "seed line must be replaceable");
    assert!(e.insert_logical_line_after(0, b"world".to_vec()), "must insert after line 0");
    assert_eq!(line(&e, 0), "hello");
    assert_eq!(line(&e, 1), "world");
    // Typing on the inserted line (ReplaceBlock path) must work too.
    assert!(e.replace_logical_line(1, b"WORLD".to_vec()));
    assert_eq!(line(&e, 1), "WORLD");

    // Saving writes the edited content back (no orphan seed line).
    e.save().unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert_eq!(saved, "hello\nWORLD\n");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("log.bak"));
}

#[test]
fn save_writes_back_edited_inserted_line() {
    // Insert a line, type on it (ReplaceBlock), then save — the block edits
    // must stream to disk exactly as seen on screen.
    let (mut e, path) = open("aaa\nbbb\nccc");
    e.insert_logical_line_after(0, b"x".to_vec());
    e.replace_logical_line(1, b"xy".to_vec());
    e.replace_logical_line(1, b"xyz".to_vec());
    e.save().unwrap();

    let saved = std::fs::read_to_string(&path).unwrap();
    assert_eq!(saved, "aaa\nxyz\nbbb\nccc");
    assert!(!e.is_modified());
    assert_eq!(line(&e, 1), "xyz");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("log.bak"));
}

#[test]
fn typing_on_inserted_line_works_and_undo_redos() {
    // Regression: replacing a line INSIDE an inserted block used to fail
    // silently (logical_to_physical returns None for block lines), so typing
    // on a freshly-Entered line did nothing.
    let (mut e, path) = open("aaa\nbbb\nccc");
    assert!(e.insert_logical_line_after(0, b"x".to_vec()));
    assert_eq!(line(&e, 1), "x");
    assert!(e.replace_logical_line(1, b"xy".to_vec()), "must accept replaces on inserted lines");
    assert!(e.replace_logical_line(1, b"xyz".to_vec()));
    assert_eq!(line(&e, 1), "xyz");
    assert_eq!(line(&e, 2), "bbb", "physical lines below the block must be intact");

    // The two replaces on the same block entry coalesce into one undo step.
    assert!(e.undo_one());
    assert_eq!(line(&e, 1), "x", "typing burst undone at once");
    // The next step is the original insert (the GUI wraps Enter in a batch, so
    // in the real editor this is "undo the split").
    assert!(e.undo_one());
    assert_eq!(line(&e, 1), "bbb", "insert undone, original line back");
    assert!(e.redo_one());
    assert_eq!(line(&e, 1), "x", "insert redone");
    assert!(e.redo_one());
    assert_eq!(line(&e, 1), "xyz", "typing burst redone");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn delete_inserted_line_undo_redos() {
    let (mut e, path) = open("aaa\nbbb\nccc");
    assert!(e.insert_logical_line_after(0, b"x".to_vec()));
    let removed = e.delete_logical_line_and_return(1).expect("block line must be deletable");
    assert_eq!(removed, b"x");
    assert_eq!(line(&e, 1), "bbb", "block gone, physical line back in place");

    assert!(e.undo_one());
    assert_eq!(line(&e, 1), "x", "undo restores the deleted block line");
    assert_eq!(line(&e, 2), "bbb");
    assert!(e.redo_one());
    assert_eq!(line(&e, 1), "bbb", "redo deletes it again");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn batch_join_that_deletes_inserted_line_undo_redos() {
    // GUI backspace at the start of an inserted line: replace the previous
    // physical line AND delete the inserted line — one atomic undo step.
    let (mut e, path) = open("aaa\nbbb");
    assert!(e.insert_logical_line_after(0, b"x".to_vec()));
    e.begin_edit_batch();
    assert!(e.replace_logical_line(0, b"aaax".to_vec()));
    assert!(e.delete_logical_line(1));
    e.end_edit_batch();
    assert_eq!(line(&e, 0), "aaax");
    assert_eq!(line(&e, 1), "bbb");
    assert_eq!(e.effective_line_count(), 2);

    assert!(e.undo_one());
    assert_eq!(line(&e, 0), "aaa");
    assert_eq!(line(&e, 1), "x");
    assert_eq!(line(&e, 2), "bbb");

    assert!(e.redo_one());
    assert_eq!(line(&e, 0), "aaax");
    assert_eq!(line(&e, 1), "bbb");
    let _ = std::fs::remove_file(&path);
}
