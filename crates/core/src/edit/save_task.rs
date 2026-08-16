//! Background save. Writes the post-edit file to a temp path on a worker
//! thread so a large-file save never blocks the UI. The caller finalizes by
//! swapping the mmap + atomic rename once `Done` arrives (see `engine`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use anyhow::Result;

use super::{writeback, EditBuffer};
use crate::file::MmapBackend;

/// Progress updates from the save worker.
#[derive(Debug)]
pub enum SaveProgress {
    /// Write phase is roughly X% done (coarse estimate).
    Percent(u8),
    /// The temp file is fully written (or the write failed). On success the
    /// caller renames it over the original; on failure it should be deleted.
    Done(Result<()>),
    /// Save was cancelled by the user. The temp file is NOT removed by the
    /// worker — the caller cleans up.
    Cancelled,
}

/// Background save handle. The worker backs up the original, snapshots line
/// offsets, and streams the post-edit content to `<path>.writetmp`.
pub struct BackgroundSave {
    rx: Receiver<SaveProgress>,
    // Kept alive so dropping the struct detaches cleanly (the worker signals
    // completion via the channel; the handle is never joined).
    #[allow(dead_code)]
    handle: Option<JoinHandle<()>>,
    started: Instant,
    cancel_flag: Arc<AtomicBool>,
    tmp_path: PathBuf,
}

impl BackgroundSave {
    /// Spawn a worker to write the post-edit file. `mmap` and `edits` are
    /// cloned so the caller keeps working; further edits are disabled by the
    /// frontend while a save is in flight.
    pub fn spawn(
        mmap: MmapBackend,
        path: PathBuf,
        original_lines: u64,
        edits: EditBuffer,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let started = Instant::now();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel = cancel_flag.clone();
        let tmp_path = {
            let mut p = path.as_os_str().to_owned();
            p.push(".writetmp");
            PathBuf::from(p)
        };
        let tmp = tmp_path.clone();

        let handle = thread::spawn(move || {
            // Coarse progress pings for the write phase.
            let tx2 = tx.clone();
            let ping_cancel = cancel.clone();
            let finished = Arc::new(AtomicBool::new(false));
            let ping_finished = finished.clone();
            let ping_handle = thread::spawn(move || {
                for pct in [30u8, 60, 85] {
                    thread::sleep(std::time::Duration::from_millis(150));
                    if ping_cancel.load(Ordering::Relaxed) || ping_finished.load(Ordering::Relaxed) {
                        return;
                    }
                    if tx2.send(SaveProgress::Percent(pct)).is_err() {
                        return;
                    }
                }
            });

            let result: Result<()> = (|| {
                // 1. Backup the ORIGINAL before overwriting (first save only).
                let backup = path.with_extension("log.bak");
                if !backup.exists() {
                    std::fs::copy(&path, &backup)
                        .map_err(|e| anyhow::anyhow!("备份失败: {}", e))?;
                }
                // 2. Snapshot line offsets (one full scan on the worker thread).
                let offsets = writeback::full_offsets(&mmap);
                let new_lc = writeback::projected_line_count(original_lines, &edits);
                // 3. Stream original + edits to the temp file.
                writeback::write_to_path(&tmp, mmap.as_slice(), &offsets, &edits, new_lc)?;
                Ok(())
            })();

            finished.store(true, Ordering::Relaxed);
            let msg = if cancel.load(Ordering::Relaxed) {
                SaveProgress::Cancelled
            } else {
                SaveProgress::Done(result)
            };
            let _ = tx.send(msg);
            drop(ping_handle);
        });

        Self {
            rx,
            handle: Some(handle),
            started,
            cancel_flag,
            tmp_path,
        }
    }

    /// Spawn a worker to write the post-edit content to an ARBITRARY path
    /// ("另存为"). No `.bak` backup, and the engine's working file is untouched
    /// — the caller just reports the result. `write_to_path` handles its own
    /// atomic temp+rename to `dst`.
    pub fn spawn_copy(mmap: MmapBackend, dst: PathBuf, original_lines: u64, edits: EditBuffer) -> Self {
        let (tx, rx) = mpsc::channel();
        let started = Instant::now();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel = cancel_flag.clone();
        let tmp_path = {
            let mut p = dst.as_os_str().to_owned();
            p.push(".writetmp");
            PathBuf::from(p)
        };
        let dst2 = dst.clone();

        let handle = thread::spawn(move || {
            let result: Result<()> = (|| {
                let offsets = writeback::full_offsets(&mmap);
                let new_lc = writeback::projected_line_count(original_lines, &edits);
                writeback::write_to_path(&dst2, mmap.as_slice(), &offsets, &edits, new_lc)?;
                Ok(())
            })();
            let msg = if cancel.load(Ordering::Relaxed) {
                SaveProgress::Cancelled
            } else {
                SaveProgress::Done(result)
            };
            let _ = tx.send(msg);
        });

        Self {
            rx,
            handle: Some(handle),
            started,
            cancel_flag,
            tmp_path,
        }
    }

    /// Signal the worker to stop (leaves a partial temp file; the caller
    /// removes it when the cancellation surfaces).
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
    }

    /// Drain any pending progress messages.
    pub fn poll(&self) -> Option<SaveProgress> {
        match self.rx.try_recv() {
            Ok(p) => Some(p),
            Err(_) => None,
        }
    }

    pub fn elapsed(&self) -> std::time::Duration {
        self.started.elapsed()
    }

    /// The temp file the worker writes to (for cleanup / final rename).
    pub fn tmp_path(&self) -> PathBuf {
        self.tmp_path.clone()
    }
}
