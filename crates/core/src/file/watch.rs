//! File-size watcher for tail -f. Polls `stat()` on a background thread.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::Result;

/// Polls the file every `interval` and sends the new byte size when it grows.
pub struct FileWatcher {
    rx: mpsc::Receiver<u64>,
    _handle: thread::JoinHandle<()>,
}

impl FileWatcher {
    pub fn spawn(path: PathBuf, interval: Duration) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let mut last: u64 = std::fs::metadata(&path)
                .map(|m| m.len())
                .unwrap_or(0);
            loop {
                thread::sleep(interval);
                let cur = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                if cur != last {
                    if tx.send(cur).is_err() {
                        return;
                    }
                    last = cur;
                }
            }
        });
        Ok(Self { rx, _handle: handle })
    }

    /// Drain pending events, return the largest reported size.
    pub fn try_next(&self) -> Option<u64> {
        let mut latest = None;
        loop {
            match self.rx.try_recv() {
                Ok(s) => latest = Some(latest.map_or(s, |prev: u64| prev.max(s))),
                Err(_) => break,
            }
        }
        latest
    }
}

pub fn derive_index_path(file: &Path) -> PathBuf {
    let mut p = file.to_path_buf();
    p.set_extension("qli");
    p
}
