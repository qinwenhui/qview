//! Background search worker. Runs search on a thread, sends progress updates
//! to the main thread via channel so the UI doesn't freeze.
//!
//! Two-pass parallel search:
//! Pass 1: count total hits (atomic counter)
//! Pass 2: collect up to MAX_STORED hits (bounded memory)

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use anyhow::Result;

use crate::file::MmapBackend;
use crate::search::{parse_query, BlockIndex, SearchOptions};

/// Coarse-grained progress from the worker.
#[derive(Debug, Clone)]
pub enum SearchProgress {
    Started(String),
    Percent(u8),
    Done(Arc<BlockIndex>), // BlockIndex carries total_count + sampled hits
    Cancelled,
    Failed(String),
}

pub struct BackgroundSearch {
    rx: Receiver<SearchProgress>,
    #[allow(dead_code)]
    handle: Option<JoinHandle<()>>,
    started: Instant,
    cancel_flag: Arc<AtomicBool>,
    /// Bytes scanned so far (both passes). Exposed for progress reporting
    /// (e.g. timeout messages that tell the caller how much of a huge file
    /// was covered before giving up).
    scanned: Arc<AtomicUsize>,
}

impl BackgroundSearch {
    pub fn spawn(
        mmap: MmapBackend,
        query_text: String,
        opts: SearchOptions,
        scan_window: u64,
        sample_interval: u32,
        max_samples: usize,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let started = Instant::now();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel = cancel_flag.clone();
        // Bytes actually scanned so far (both passes), for real progress.
        let scanned = Arc::new(AtomicUsize::new(0));
        // The worker closure takes the `scanned` Arc; keep a clone for the
        // struct so `scanned_bytes()` stays readable after `spawn` returns.
        let scanned_struct = scanned.clone();
        // Set once the worker finishes so the progress poller exits its loop.
        let finished = Arc::new(AtomicBool::new(false));
        let total = mmap.size();
        let _ = tx.send(SearchProgress::Started(query_text.clone()));

        let handle = thread::spawn(move || {
            let query = match parse_query(&query_text, &opts) {
                Ok(q) => q,
                Err(e) => {
                    let _ = tx.send(SearchProgress::Failed(format!("bad query: {e}")));
                    return;
                }
            };

            // Progress poller: reports REAL progress from the scanned-byte
            // counter (Pass 1 scans the whole file → 50%, Pass 2 → 100%).
            // Exits when the worker finishes or is cancelled.
            let tx2 = tx.clone();
            let ping_cancel = cancel.clone();
            let scanned2 = scanned.clone();
            let finished2 = finished.clone();
            let total2 = total;
            let _ping_handle = thread::spawn(move || {
                loop {
                    thread::sleep(std::time::Duration::from_millis(150));
                    if ping_cancel.load(Ordering::Relaxed) {
                        let _ = tx2.send(SearchProgress::Cancelled);
                        return;
                    }
                    if finished2.load(Ordering::Relaxed) {
                        return;
                    }
                    let s = scanned2.load(Ordering::Relaxed) as u64;
                    let pct = if total2 > 0 {
                        (((s * 100) / (2 * total2)) as u8).clamp(0, 99)
                    } else {
                        99
                    };
                    if tx2.send(SearchProgress::Percent(pct)).is_err() {
                        return;
                    }
                }
            });

            let cancel2 = cancel.clone();
            let scanned2 = scanned.clone();
            let result = Self::run_search(
                query, mmap, scan_window, cancel2, scanned2, sample_interval, max_samples,
            );
            finished.store(true, Ordering::Relaxed);

            match result {
                Ok(index) => {
                    if cancel.load(Ordering::Relaxed) {
                        let _ = tx.send(SearchProgress::Cancelled);
                        return;
                    }
                    let _ = tx.send(SearchProgress::Done(Arc::new(index)));
                }
                Err(e) => {
                    let _ = tx.send(SearchProgress::Failed(e.to_string()));
                }
            }
        });

        Self { rx, handle: Some(handle), started, cancel_flag, scanned: scanned_struct }
    }

    /// Bytes scanned so far (both passes). Cheap, lock-free.
    pub fn scanned_bytes(&self) -> u64 {
        self.scanned.load(Ordering::Relaxed) as u64
    }

    fn run_search(
        query: crate::search::Query,
        mmap: MmapBackend,
        scan_window: u64,
        cancel: Arc<AtomicBool>,
        scanned: Arc<AtomicUsize>,
        sample_interval: u32,
        max_samples: usize,
    ) -> Result<BlockIndex> {
        if mmap.size() == 0 {
            return Ok(BlockIndex::empty());
        }
        let (samples, interval, total_count) = super::scan_hits(
            &query,
            mmap.path(),
            scan_window,
            sample_interval,
            max_samples,
            Some(cancel.as_ref()),
            Some(scanned.as_ref()),
        )?;
        Ok(BlockIndex::from_samples(mmap, samples, interval, total_count, query))
    }

    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
    }

    pub fn poll(&self) -> Option<SearchProgress> {
        loop {
            match self.rx.try_recv() {
                Ok(p) => return Some(p),
                Err(_) => return None,
            }
        }
    }

    pub fn elapsed(&self) -> std::time::Duration {
        self.started.elapsed()
    }
}
