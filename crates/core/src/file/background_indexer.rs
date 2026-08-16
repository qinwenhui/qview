//! Background index builder. Runs IndexBuilder on a worker thread, persists
//! the `.qli` index cache, then notifies the main thread. All I/O happens
//! off the main thread so the UI never blocks.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crate::config::IndexBuildMode;
use super::index::{IndexBuilder, IndexBuildOutcome};
use super::mmap_backend::MmapBackend;
use super::persist::{file_meta, write_index};

/// Progress updates from the indexer worker.
#[derive(Debug, Clone)]
pub enum IndexProgress {
    /// Worker is X% done (coarse estimate).
    Percent(u8),
    /// Indexing + persist finished; carries the sparse build outcome.
    /// At this point the `.qli` cache file has already been written.
    Done(IndexBuildOutcome),
    /// Indexing was cancelled by user.
    Cancelled,
    /// Indexing failed.
    Failed(String),
}

/// Background indexing handle. The worker uses rayon internally for fast
/// parallel scanning, then writes the `.qli` cache, then sends `Done`.
/// The main thread never touches disk.
pub struct BackgroundIndexer {
    rx: Receiver<IndexProgress>,
    #[allow(dead_code)]
    handle: Option<JoinHandle<()>>,
    started: Instant,
    cancel_flag: Arc<AtomicBool>,
}

impl BackgroundIndexer {
    /// Spawn a worker thread to build the line index and optionally persist
    /// the `.qli` cache to `cache_path`.
    ///
    /// `log_path` is the original log file.
    /// `cache_path` is where to write the `.qli` index, or `None` to skip
    /// persistence (e.g. small files or caching disabled).
    /// `build_mode` selects the index-build strategy (sparse vs full).
    pub fn spawn(
        mmap: MmapBackend,
        log_path: PathBuf,
        cache_path: Option<PathBuf>,
        build_mode: IndexBuildMode,
        scan_window: u64,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let started = Instant::now();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel = cancel_flag.clone();

        let handle = thread::spawn(move || {
            // Periodic pings so the UI can show a progress bar.  Stops as soon
            // as the build finishes (set by the worker before sending `Done`)
            // so a trailing ping can never follow the terminal message.
            let tx2 = tx.clone();
            let ping_cancel = cancel.clone();
            let finished = Arc::new(AtomicBool::new(false));
            let ping_finished = finished.clone();
            let ping_handle = thread::spawn(move || {
                for pct in [10u8, 25, 50, 75, 90] {
                    thread::sleep(std::time::Duration::from_millis(200));
                    if ping_cancel.load(Ordering::Relaxed) {
                        let _ = tx2.send(IndexProgress::Cancelled);
                        return;
                    }
                    if ping_finished.load(Ordering::Relaxed) {
                        return;
                    }
                    if tx2.send(IndexProgress::Percent(pct)).is_err() {
                        return;
                    }
                }
            });

            // Build the line index (rayon parallel scan — CPU intensive).
            // The build checks `cancel` between windows, so a file switch
            // stops the scan within ~one window (~tens of ms) instead of
            // reading the whole file to the end.
            let mut builder = IndexBuilder::new(mmap.clone());
            builder.set_scan_window(scan_window);
            builder.set_cancel(cancel.clone());
            let outcome = match build_mode {
                // Inline sparse sampling: low memory, but a second CPU pass
                // over each buffered window (the file is read from disk once).
                IndexBuildMode::Sparse => builder.build_sparse_with_progress(|_, _| {}),
                // Legacy full-offset build: one pass, higher peak memory.
                IndexBuildMode::Full => builder.build_full_with_progress(|_, _| {}),
            };
            let outcome = match outcome {
                Ok(o) => o,
                Err(e) => {
                    // A cancelled build surfaces as an error — report it as a
                    // clean Cancelled rather than a failure.
                    let msg = if cancel.load(Ordering::Relaxed) {
                        IndexProgress::Cancelled
                    } else {
                        IndexProgress::Failed(format!("{e}"))
                    };
                    let _ = tx.send(msg);
                    return;
                }
            };

            if cancel.load(Ordering::Relaxed) {
                let _ = tx.send(IndexProgress::Cancelled);
                return;
            }

            // Persist .qli cache on this worker thread — main thread is
            // never blocked on disk I/O. The file is guaranteed complete
            // when `Done` arrives at the main thread.
            if let Some(ref cache_path) = cache_path {
                // Ensure parent directory exists.
                if let Some(parent) = cache_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Ok(meta) = file_meta(&log_path) {
                    let _ = write_index(
                        cache_path,
                        meta.size,
                        meta.mtime,
                        meta.inode,
                        outcome.total_lines,
                        &outcome.sparse,
                        super::index::SPARSE_FACTOR,
                        outcome.max_line_bytes,
                        outcome.max_line_index,
                    );
                }
            }

            if cancel.load(Ordering::Relaxed) {
                let _ = tx.send(IndexProgress::Cancelled);
                return;
            }
            // Stop the ping thread BEFORE the terminal message, so `Done` is
            // guaranteed to be the last thing in the channel.
            finished.store(true, Ordering::Relaxed);
            let _ = tx.send(IndexProgress::Done(outcome));
            drop(ping_handle);
        });

        Self {
            rx,
            handle: Some(handle),
            started,
            cancel_flag,
        }
    }

    /// Signal the worker to stop.
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
    }

    /// Drain any pending progress messages.
    pub fn poll(&self) -> Option<IndexProgress> {
        match self.rx.try_recv() {
            Ok(p) => Some(p),
            Err(_) => None,
        }
    }

    pub fn elapsed(&self) -> std::time::Duration {
        self.started.elapsed()
    }
}
