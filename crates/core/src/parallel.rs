//! Shared, CPU-bounded rayon pool for heavy background scans (index building,
//! search).  The pool ALWAYS leaves one core free — never saturating the
//! machine — for two reasons:
//!
//! 1. The UI thread needs a core so a huge-file scan doesn't freeze the GUI.
//! 2. The streaming scan's reader thread needs to be woken promptly after each
//!    disk read completes.  On a fully-saturated machine (no free core) the
//!    reader's wakeup is delayed by preemption, the next 64 MiB read is issued
//!    late, and the disk idles between windows — measured as a ~20 % throughput
//!    drop even though the CPU passes themselves got faster.
//!
//! The thread count is configurable via [`set_scan_threads`]: `0` = auto
//! (`available_parallelism − 1`), `≥ 1` = force that exact count (capped at
//! `available_parallelism − 1`, min 1 — the cap is what protects the reader).
//! The value is read once, when the pool is first built — call
//! [`set_scan_threads`] at engine startup, before any scan.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

/// `0` = auto (leave one core for the UI). Overridden by [`set_scan_threads`].
static SCAN_THREADS: AtomicU32 = AtomicU32::new(0);

/// Configure how many threads the scan pool uses. `0` = auto
/// (`available_parallelism − 1`). Must be called before the pool is first
/// built (i.e. at engine startup); once built the count is fixed.
pub fn set_scan_threads(n: u32) {
    SCAN_THREADS.store(n, Ordering::Relaxed);
}

/// Dedicated scan pool.  Uses `available_parallelism − 1` threads (min 1),
/// or the count set via [`set_scan_threads`], always capped at
/// `available_parallelism − 1` so the reader thread is never starved.
pub fn scan_pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let avail = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .max(1);
        let cap = avail.saturating_sub(1).max(1); // always leave one core free
        let want = SCAN_THREADS.load(Ordering::Relaxed);
        let threads = if want > 0 { want as usize } else { avail.saturating_sub(1) }
            .max(1)
            .min(cap);
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("qview-scan-{i}"))
            .build()
            .expect("build scan thread pool")
    })
}
