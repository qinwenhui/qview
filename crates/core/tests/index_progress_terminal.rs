//! Regression test: the background indexer must never emit a progress ping
//! AFTER the terminal `Done` message.
//!
//! For a tiny file the build finishes within the ping thread's 200ms cadence,
//! which used to let the ping thread enqueue `Percent` after `Done`.  The
//! engine's poller then resurrected `index_progress` after clearing it and
//! dropped `bg_indexer`, leaving a progress bar stuck forever.

use std::time::{Duration, Instant};

use qview_core::config::IndexBuildMode;
use qview_core::file::background_indexer::{BackgroundIndexer, IndexProgress};
use qview_core::file::MmapBackend;

#[test]
fn no_progress_after_terminal_message() {
    let mut path = std::env::temp_dir();
    path.push(format!("qview_idx_terminal_{}.txt", std::process::id()));
    std::fs::write(&path, b"line one\nline two\nline three\n").unwrap();

    let mmap = MmapBackend::open(&path).unwrap();
    let bg = BackgroundIndexer::spawn(
        mmap,
        path.clone(),
        None,
        IndexBuildMode::Sparse,
        qview_core::file::SCAN_WINDOW,
    );

    // Drain messages; keep listening for ~1.2s after the terminal message to
    // catch straggler pings (the ping thread used to keep sending at 200ms
    // intervals for up to 1s).
    let start = Instant::now();
    let hard_deadline = start + Duration::from_secs(5);
    let quiet_until = start + Duration::from_millis(1200);
    let mut terminal_seen = false;

    loop {
        match bg.poll() {
            Some(IndexProgress::Percent(_)) => {
                assert!(!terminal_seen, "Percent emitted after a terminal state");
            }
            Some(IndexProgress::Done(_))
            | Some(IndexProgress::Cancelled)
            | Some(IndexProgress::Failed(_)) => {
                terminal_seen = true;
            }
            None => {
                if terminal_seen && Instant::now() > quiet_until {
                    break;
                }
                if Instant::now() > hard_deadline {
                    panic!("indexer never produced a terminal message");
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        }
    }
    assert!(terminal_seen, "expected a terminal message");

    let _ = std::fs::remove_file(&path);
}
