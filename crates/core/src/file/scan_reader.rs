//! Streaming full-file scanner for index builds and search.
//!
//! Two problems the old windowed-mmap scan (one `SCAN_WINDOW` mmap at a time)
//! had, and how this replaces it:
//!
//! 1. **Throughput** — an mmap window is demand-paged; the page-fault queue is
//!    too shallow to saturate an NVMe (measured ~1.5 GB/s vs the drive's raw
//!    ceiling). A single sequential `ReadFile` with a large buffer keeps the
//!    drive streaming (~2.6 GB/s on real log data, ~8 GB/s on a sparse file),
//!    and beats parallel chunked reads too (deep per-request queue + no
//!    per-chunk overhead).
//! 2. **System memory** — every byte read here is read exactly once and
//!    discarded. On Windows the scan handle is opened with
//!    `FILE_FLAG_NO_BUFFERING`, which bypasses the OS file cache entirely, so
//!    scanning a 27 GB file never grows the *system* cache (the original
//!    "97 % memory" complaint). If the filesystem rejects `NO_BUFFERING` (e.g. a
//!    network share), we fall back to a buffered handle flagged
//!    `FILE_FLAG_SEQUENTIAL_SCAN`. On unix we use `posix_fadvise(DONTNEED)` to
//!    evict each window's pages after it has been read.
//!
//! To keep the disk busy while the caller's parallel pool processes a window,
//! [`WindowStream`] runs a dedicated reader thread that streams windows into
//! two alternating buffers (double buffering): the caller processes one window
//! while the reader thread fills the next. Without it, read→process→read
//! serializes and loses ~30 % of the disk's throughput to CPU time and thread
//! wake-ups.
//!
//! The engine's whole-file [`MmapBackend`](super::MmapBackend) is unaffected —
//! that still backs interactive browsing (random access, pages on demand).

use std::fs::File;
use std::io;
use std::path::Path;

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};

/// Default scan window: 64 MiB. A multiple of the OS page size, so window
/// starts are always 4096-aligned (required by `FILE_FLAG_NO_BUFFERING`).
pub const SCAN_WINDOW: u64 = 64 * 1024 * 1024;

/// Bytes of the previous window prepended to a search window ("lead"), so the
/// regex scan sees the real line-start context at the window boundary and can
/// align each chunk to a line start. 64 KiB covers any realistic log line; the
/// alignment backward-search in `search/mod.rs` shares this bound. Page-aligned
/// (a 4096 multiple), required for the raw `NO_BUFFERING` read path.
///
/// Only active when the window carries an `extra` overlap (i.e. search); index
/// builds open with `extra = 0` and keep `slice[0] == file[window.start()]`.
pub const MAX_LEAD: usize = 64 * 1024;

const PAGE: usize = 4096;

/// A reusable buffer whose data region is 4096-aligned (required by the raw
/// `NO_BUFFERING` read path). `Vec` alone only guarantees element alignment.
struct AlignedBuf {
    base: Vec<u8>,
    off: usize, // aligned data offset into `base`
}

impl AlignedBuf {
    fn with_capacity(cap: usize) -> Self {
        let base = vec![0u8; cap + PAGE];
        let off = (PAGE - ((base.as_ptr() as usize) & (PAGE - 1))) & (PAGE - 1);
        Self { base, off }
    }

    #[inline]
    fn cap(&self) -> usize {
        self.base.len() - PAGE
    }

    /// Grow to hold `cap` bytes (reallocating; old contents are discarded).
    fn ensure(&mut self, cap: usize) {
        if cap <= self.cap() {
            return;
        }
        *self = Self::with_capacity(cap);
    }

    /// Mutable slice of the aligned data region, first `len` bytes.
    #[inline]
    fn as_mut_slice(&mut self, len: usize) -> &mut [u8] {
        debug_assert!(len <= self.cap());
        &mut self.base[self.off..self.off + len]
    }

    /// Immutable slice of the aligned data region, first `len` bytes.
    #[inline]
    fn as_slice(&self, len: usize) -> &[u8] {
        debug_assert!(len <= self.cap());
        &self.base[self.off..self.off + len]
    }
}

/// Streaming single-pass scanner. Reads a range of the file into a provided
/// aligned buffer (raw `NO_BUFFERING` on Windows, `read_at` + fadvise on unix).
pub struct ScanReader {
    file: File,          // buffered handle (tails, fallback; SEQUENTIAL_SCAN on Windows)
    raw: Option<File>,   // Windows NO_BUFFERING handle; None = fallback / non-Windows
    size: u64,
    buf: AlignedBuf,
    #[cfg(unix)]
    last_range: Option<(u64, u64)>, // previous window, for POSIX_FADV_DONTNEED
}

impl ScanReader {
    /// Open a dedicated scan handle for `path`.
    pub fn open(path: &Path) -> Result<Self> {
        #[cfg(windows)]
        let raw = {
            use std::os::windows::fs::OpenOptionsExt;
            match std::fs::OpenOptions::new()
                .read(true)
                // FILE_FLAG_NO_BUFFERING (0x20000000) | FILE_FLAG_SEQUENTIAL_SCAN (0x08000000):
                // bypass the cache entirely (raw DMA to user buffer).
                .custom_flags(0x2000_0000 | 0x0800_0000)
                .open(path)
            {
                Ok(f) => Some(f),
                Err(_) => {
                    // e.g. a network share — fall back to a buffered handle.
                    eprintln!(
                        "[qview] NO_BUFFERING unsupported on this volume; scan will use the file cache"
                    );
                    None
                }
            }
        };
        #[cfg(not(windows))]
        let raw = None;

        #[cfg(windows)]
        let file = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                // SEQUENTIAL_SCAN: hint the cache manager not to retain pages.
                .custom_flags(0x0800_0000)
                .open(path)
                .with_context(|| format!("open (scan) {}", path.display()))?
        };
        #[cfg(not(windows))]
        let file = File::open(path).with_context(|| format!("open (scan) {}", path.display()))?;

        let size = file.metadata()?.len();
        Ok(Self {
            file,
            raw,
            size,
            buf: AlignedBuf::with_capacity(SCAN_WINDOW as usize + PAGE),
            #[cfg(unix)]
            last_range: None,
        })
    }

    /// File size in bytes.
    #[inline]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Read the window at `start` into the internal reusable buffer: `len`
    /// bytes of owned data plus `extra` overlap bytes (search uses this so
    /// patterns spanning a window boundary are found). Clamped to EOF.
    ///
    /// Returns a slice valid until the next call to [`read_window`]
    /// (the internal buffer is reused).
    pub fn read_window(&mut self, start: u64, len: u64, extra: usize) -> Result<&[u8]> {
        let read_len =
            ((len as usize + extra) as u64).min(self.size.saturating_sub(start)) as usize;
        debug_assert!(read_len > 0, "read_window with empty range");
        #[cfg(unix)]
        self.fadvise_prev();
        self.buf.ensure(read_len);
        Self::read_into(&self.file, self.raw.as_ref(), &mut self.buf, start, read_len, 0)?;
        #[cfg(unix)]
        {
            self.last_range = Some((start, read_len as u64));
        }
        Ok(self.buf.as_slice(read_len))
    }

    /// Drop the previous window's file-cache pages (they've been processed).
    ///
    /// `posix_fadvise` is Linux/FreeBSD-only; macOS has no per-fd cache-drop
    /// API, so there this is a no-op and the OS evicts the pages under memory
    /// pressure.
    #[cfg(unix)]
    fn fadvise_prev(&self) {
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            if let Some((start, len)) = self.last_range {
                use std::os::unix::io::AsRawFd;
                unsafe {
                    libc::posix_fadvise(
                        self.file.as_raw_fd(),
                        start as libc::off_t,
                        len as libc::off_t,
                        libc::POSIX_FADV_DONTNEED,
                    );
                }
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
        {
            let _ = self.last_range;
        }
    }

    /// Fill `dst[dst_off..dst_off+read_len]` with file bytes
    /// `[start, start+read_len)`. `dst` must already be sized
    /// ≥ `dst_off + read_len`, and `dst_off` must be 4096-aligned (the lead for
    /// a search window is [`MAX_LEAD`], a page multiple).
    fn read_into(
        file: &File,
        raw: Option<&File>,
        dst: &mut AlignedBuf,
        start: u64,
        read_len: usize,
        dst_off: usize,
    ) -> Result<()> {
        debug_assert!(start % PAGE as u64 == 0, "read offset must be page-aligned");
        let total = dst_off + read_len;
        if let Some(raw) = raw {
            // Raw path: the whole window must be read into a 4096-aligned
            // buffer in 4096-multiples. Read the aligned prefix via the raw
            // handle; the < 4096-byte tail goes through the buffered handle
            // (a partial raw read would hit ERROR_HANDLE_EOF at a non-aligned
            // file end).
            let aligned_len = read_len & !(PAGE - 1);
            if aligned_len > 0 {
                let slice = dst.as_mut_slice(total);
                read_exact_at(raw, &mut slice[dst_off..dst_off + aligned_len], start)?;
            }
            if aligned_len < read_len {
                let slice = dst.as_mut_slice(total);
                read_exact_at(
                    file,
                    &mut slice[dst_off + aligned_len..total],
                    start + aligned_len as u64,
                )?;
            }
        } else {
            let slice = dst.as_mut_slice(total);
            read_exact_at(file, &mut slice[dst_off..total], start)?;
        }
        Ok(())
    }
}

/// One scanned window, owned. Processing it (via [`Window::as_slice`]) doesn't
/// block the reader thread, which is already filling the other buffer. Dropping
/// it returns its buffer to the reader for reuse.
///
/// Layout when a lead is active (search): `as_slice()` = `[lead][owned][overlap]`
/// where `lead` is the tail of the previous window, so the scan can align chunk
/// starts to real line boundaries. Index builds (`extra = 0`) always have
/// `lead == 0` and `as_slice()[0]` is `file[window.start()]`, as before.
pub struct Window {
    buf: Option<AlignedBuf>,
    start: u64,
    lead: usize,  // prepended look-back bytes (0 for index builds)
    len: usize,   // total bytes in the slice (lead + owned + overlap)
    owned: usize, // bytes owned by this window
    recycle: Sender<AlignedBuf>,
}

impl Window {
    /// File offset where this window's owned region starts.
    #[inline]
    pub fn start(&self) -> u64 {
        self.start
    }

    /// Bytes prepended from the previous window (look-back); always 0 for
    /// index builds. `as_slice()[..lead]` is the previous window's tail.
    #[inline]
    pub fn lead(&self) -> usize {
        self.lead
    }

    /// Total bytes in the slice (lead + owned + overlap).
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Bytes owned by this window (the overlap tail belongs to the next one).
    #[inline]
    pub fn owned(&self) -> usize {
        self.owned
    }

    /// The window's bytes.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        self.buf.as_ref().expect("window buffer").as_slice(self.len)
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        if let Some(b) = self.buf.take() {
            let _ = self.recycle.send(b);
        }
    }
}

/// Windowed stream over the whole file with a dedicated reader thread.
///
/// The reader thread reads one window at a time (size passed to
/// [`WindowStream::open`]) into one of two alternating aligned buffers and
/// hands it to the caller, who processes it while the reader fills the other
/// (double buffering). Peak memory is ~ two windows + the caller's results,
/// independent of file size.
pub struct WindowStream {
    rx: Receiver<Result<Window>>,
    _reader: std::thread::JoinHandle<()>,
    size: u64,
}

impl WindowStream {
    /// Stream windows of `path`, each `window` owned bytes plus `extra`
    /// overlap bytes (search patterns straddling a boundary; 0 for index
    /// builds). The reader thread is started here.
    ///
    /// `window` is the streaming scan window in bytes (the caller derives it
    /// from `EngineConfig::scan_window_mb`); it should be a 4096 multiple.
    pub fn open(path: &Path, extra: usize, window: u64) -> Result<Self> {
        let size = std::fs::metadata(path)
            .with_context(|| format!("metadata {}", path.display()))?
            .len();
        let reader = ScanReader::open(path)?;
        let (win_tx, win_rx) = bounded(2);
        let (free_tx, free_rx) = bounded(2);
        let cap = window as usize + MAX_LEAD + PAGE;
        free_tx.send(AlignedBuf::with_capacity(cap)).expect("send buf");
        free_tx.send(AlignedBuf::with_capacity(cap)).expect("send buf");
        // Only search windows (which carry an `extra` overlap) get the
        // look-back lead; index builds keep `slice[0] == file[window.start()]`.
        let use_lead = extra > 0;
        let _reader = std::thread::Builder::new()
            .name("qview-scan-reader".to_string())
            .spawn(move || {
                let mut start = 0u64;
                while start < size {
                    let len = (size - start).min(window);
                    let is_last = start + len >= size;
                    let ex = if is_last { 0 } else { extra };
                    let read_len = ((len as usize + ex) as u64).min(size - start) as usize;
                    let lead = if use_lead { MAX_LEAD.min(start as usize) } else { 0 };
                    let need = lead + read_len;
                    let mut buf = match free_rx.recv() {
                        Ok(b) => b,
                        Err(_) => break, // consumer gone
                    };
                    buf.ensure(need);
                    #[cfg(unix)]
                    reader.fadvise_prev();
                    let res = (|| {
                        if lead > 0 {
                            // Look-back: the previous window's tail, so the scan
                            // can align to line starts across the boundary. Read
                            // via the buffered handle (no alignment requirement);
                            // ≤64 KiB re-read per window — negligible vs the
                            // 64 MiB window itself.
                            let dst = buf.as_mut_slice(need);
                            read_exact_at(&reader.file, &mut dst[..lead], start - lead as u64)?;
                        }
                        ScanReader::read_into(
                            &reader.file,
                            reader.raw.as_ref(),
                            &mut buf,
                            start,
                            read_len,
                            lead,
                        )?;
                        Ok(Window {
                            buf: Some(buf),
                            start,
                            lead,
                            len: need,
                            owned: len as usize,
                            recycle: free_tx.clone(),
                        })
                    })();
                    #[cfg(unix)]
                    {
                        reader.last_range = Some((start, read_len as u64));
                    }
                    if win_tx.send(res).is_err() {
                        break; // consumer dropped the stream
                    }
                    start += len;
                }
            })?;
        Ok(Self { rx: win_rx, _reader, size })
    }

    /// Total file size in bytes.
    #[inline]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Next window, or `None` at end of file. The returned [`Window`] must be
    /// dropped (or fall out of scope) to return its buffer to the reader.
    pub fn next(&self) -> Result<Option<Window>> {
        match self.rx.recv() {
            Ok(Ok(win)) => Ok(Some(win)),
            Ok(Err(e)) => Err(e),
            Err(_) => Ok(None),
        }
    }
}

/// Positional `read_exact` that doesn't disturb the shared cursor, so the same
/// handle is safe to read at arbitrary offsets (thread-safe on Windows/unix).
#[cfg(windows)]
fn read_exact_at(file: &File, mut buf: &mut [u8], mut off: u64) -> Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, off) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short read").into()),
            Ok(n) => {
                off += n as u64;
                buf = &mut buf[n..];
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn read_exact_at(file: &File, mut buf: &mut [u8], mut off: u64) -> Result<()> {
    use std::os::unix::fs::FileExt;
    while !buf.is_empty() {
        match file.read_at(buf, off) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short read").into()),
            Ok(n) => {
                off += n as u64;
                buf = &mut buf[n..];
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
