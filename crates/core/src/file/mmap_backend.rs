//! Mmap-backed file access. Zero-copy slices from the mmap; OS pages in on demand.
//!
//! [`MmapBackend`] maps the whole file read-only and is used by the engine for
//! interactive browsing (pages load on demand, bounded by what you look at).
//!
//! Full-file scans (index build, search) DON'T use mmap — they use
//! [`ScanReader`](super::ScanReader), a windowed streaming reader that reads
//! one 64 MiB window into a reusable buffer at a time. mmap's demand-paged
//! reads can't saturate an NVMe and the pages linger in the system cache;
//! see `scan_reader.rs` for why streaming raw reads are both faster and
//! bounded in memory.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use memmap2::Mmap;

/// Read-only mmap handle. Cheap to clone (Arc inside).
#[derive(Clone)]
pub struct MmapBackend {
    inner: Arc<MmapBackendInner>,
}

struct MmapBackendInner {
    mmap: Mmap,
    path: PathBuf,
    size: u64,
}

impl MmapBackend {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
        let size = file.metadata()?.len();

        let mmap = unsafe {
            memmap2::MmapOptions::new()
                .map(&file)
                .with_context(|| format!("mmap {}", path.display()))?
        };

        #[cfg(unix)]
        {
            use memmap2::Advice;
            let _ = mmap.advise(Advice::Sequential);
        }

        Ok(Self {
            inner: Arc::new(MmapBackendInner { mmap, path, size }),
        })
    }

    #[inline]
    pub fn size(&self) -> u64 {
        self.inner.size
    }

    #[inline]
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Byte slice for range `[start, start+len)`. Clamped to EOF.
    #[inline]
    pub fn slice(&self, start: u64, len: usize) -> &[u8] {
        let s = start as usize;
        let e = (s + len).min(self.inner.mmap.len());
        &self.inner.mmap[s..e]
    }

    /// Full backing slice. Used by the index builder; don't hold long.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.inner.mmap
    }

    /// Find next `\n` at or after `start`. Returns 1 + the index, or None.
    pub fn find_newline_after(&self, start: u64) -> Option<u64> {
        let slice = self.slice(start, self.size().saturating_sub(start) as usize);
        memchr::memchr(b'\n', slice).map(|p| start + p as u64 + 1)
    }

    /// Read one line from `start`. Returns (bytes, offset_of_next_line).
    pub fn read_line(&self, start: u64) -> (&[u8], u64) {
        let slice = self.slice(start, self.size().saturating_sub(start) as usize);
        match memchr::memchr(b'\n', slice) {
            Some(nl) => (&slice[..nl], start + nl as u64 + 1),
            None => (slice, self.size()),
        }
    }

    /// Re-mmap from disk (for tail -f after file growth).
    pub fn refresh(&mut self) -> Result<()> {
        let file = File::open(&self.inner.path)?;
        let new_size = file.metadata()?.len();
        let mmap = unsafe { memmap2::MmapOptions::new().map(&file)? };
        #[cfg(unix)]
        {
            use memmap2::Advice;
            let _ = mmap.advise(Advice::Sequential);
        }
        self.inner = Arc::new(MmapBackendInner {
            mmap,
            path: self.inner.path.clone(),
            size: new_size,
        });
        Ok(())
    }
}

