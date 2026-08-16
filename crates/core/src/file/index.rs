//! Line offset index and streaming builder. Builds offsets in parallel chunks
//! via memchr + rayon, merges into a sorted vec.
//!
//! 大文件保 RSS：扫描通过 [`ScanReader`](super::scan_reader::ScanReader) 流式
//! 读入（Windows 上 `FILE_FLAG_NO_BUFFERING` 直接绕过文件缓存，Unix 上
//! `posix_fadvise(DONTNEED)` 逐窗口释放），文件只读一遍，物理内存占用与
//! 文件大小无关。
//!
//! 稀疏索引：当文件行数非常多时，全量偏移数组会占用大量内存。稀疏索引
//! 只存储每 SPARSE_FACTOR 行的偏移（约 8 字节/行 → ~0.06 字节/行），
//! 通过二分查找 + 向前扫描定位精确偏移。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use parking_lot::RwLock;
use rayon::prelude::*;

use super::mmap_backend::MmapBackend;
use super::scan_reader::SCAN_WINDOW;

/// Sparse line index: stores every N lines' byte offset.
/// Dramatically reduces memory for large files (70M lines → ~560KB instead of ~560MB).
pub const SPARSE_FACTOR: u32 = 128;

/// Result of a sparse index build: the sampled offsets plus the exact metadata
/// `LineIndex` and the `.qli` cache need.  Building this directly avoids ever
/// materialising the full N×8-byte offsets array (800 MB for 100M lines).
#[derive(Debug, Clone)]
pub struct IndexBuildOutcome {
    /// Start-byte offsets of lines whose index is a multiple of `SPARSE_FACTOR`
    /// (line 0 always included), i.e. `sparse[i] = start of line i*SPARSE_FACTOR`.
    pub sparse: Vec<u64>,
    /// Exact line count.
    pub total_lines: u64,
    /// Exact byte span of the longest line (including its trailing newline).
    pub max_line_bytes: u64,
    /// Index of the line with the longest span.
    pub max_line_index: u64,
}

/// Per-chunk data gathered during the sparse build's sampling pass.
struct SparseChunk {
    /// Offsets of line starts whose global line index is a multiple of SPARSE_FACTOR.
    offsets: Vec<u64>,
    /// Byte of the first newline in this chunk.
    first_nl: Option<u64>,
    /// Byte of the last newline in this chunk.
    last_nl: Option<u64>,
    /// Largest gap between consecutive newlines inside this chunk.
    max_gap: u64,
    /// Global line index of that largest internal gap.
    max_gap_line: u64,
}


/// Sorted line-start offsets (sparse). Built in background; readers gracefully degrade
/// via coarse byte-position estimate + forward scan while building.
pub struct LineIndex {
    inner: Arc<RwLock<Inner>>,
}

struct Inner {
    /// Sparse offsets: every SPARSE_FACTOR lines. sparse[i] = byte offset of line i*SPARSE_FACTOR.
    sparse: Vec<u64>,
    byte_len: u64,
    complete: bool,
    /// The sparse factor used for this index.
    sparse_factor: u32,
    /// Total line count (accurate once complete).
    total_lines: u64,
    /// Byte length of the longest line (exact, includes trailing newline).
    /// Computed from full offsets — used for horizontal scroll range.
    max_line_bytes: u64,
    /// 0-based index of the longest line (by byte length).
    max_line_index: u64,
}

impl LineIndex {
    pub fn new(byte_len: u64) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner {
                sparse: Vec::new(),
                byte_len,
                complete: false,
                sparse_factor: SPARSE_FACTOR,
                total_lines: 0,
                max_line_bytes: 0,
                max_line_index: 0,
            })),
        }
    }

    /// Compute the longest line's byte span and its 0-based index from full
    /// line-start offsets. Line i spans [offsets[i], offsets[i+1]) (or
    /// [offsets[i], byte_len) for the last line).
    pub fn compute_max_line(offsets: &[u64], byte_len: u64) -> (u64, u64) {
        let mut max_bytes = 0u64;
        let mut max_idx = 0u64;
        for i in 0..offsets.len() {
            let end = offsets.get(i + 1).copied().unwrap_or(byte_len);
            let span = end.saturating_sub(offsets[i]);
            if span > max_bytes {
                max_bytes = span;
                max_idx = i as u64;
            }
        }
        (max_bytes, max_idx)
    }

    /// Build from a full offsets vec (e.g. loaded from .qli or from builder).
    /// Automatically downsamples to sparse format.
    pub fn from_vec(offsets: Vec<u64>, byte_len: u64) -> Self {
        let sparse = Self::build_sparse(&offsets, SPARSE_FACTOR);
        let complete = !offsets.is_empty();
        let total_lines = offsets.len() as u64;
        let (max_line_bytes, max_line_index) = Self::compute_max_line(&offsets, byte_len);
        Self {
            inner: Arc::new(RwLock::new(Inner {
                sparse,
                byte_len,
                complete,
                sparse_factor: SPARSE_FACTOR,
                total_lines,
                max_line_bytes,
                max_line_index,
            })),
        }
    }

    /// Build from already-sparse offsets (e.g. loaded from .qli v2).
    /// No additional downsampling — the data is stored as-is.
    pub fn from_sparse(
        sparse: Vec<u64>,
        byte_len: u64,
        sparse_factor: u32,
        total_lines: u64,
        max_line_bytes: u64,
        max_line_index: u64,
    ) -> Self {
        let complete = !sparse.is_empty();
        Self {
            inner: Arc::new(RwLock::new(Inner {
                sparse,
                byte_len,
                complete,
                sparse_factor,
                total_lines,
                max_line_bytes,
                max_line_index,
            })),
        }
    }

    /// Build sparse offsets from full offsets, sampling every `factor` lines.
    fn build_sparse(offsets: &[u64], factor: u32) -> Vec<u64> {
        if offsets.is_empty() || factor == 0 {
            return Vec::new();
        }
        let sparse_count = ((offsets.len() + factor as usize - 1) / factor as usize).max(1);
        let mut sparse = Vec::with_capacity(sparse_count);
        // Always include line 0 offset (byte 0).
        sparse.push(offsets[0]);
        for i in (factor as usize..offsets.len()).step_by(factor as usize) {
            sparse.push(offsets[i]);
        }
        sparse
    }

    /// Total line count (snapshot; subject to change while building).
    pub fn line_count(&self) -> u64 {
        self.inner.read().total_lines
    }

    pub fn is_complete(&self) -> bool {
        self.inner.read().complete
    }

    pub fn sparse_factor(&self) -> u32 {
        self.inner.read().sparse_factor
    }

    pub fn byte_len(&self) -> u64 {
        self.inner.read().byte_len
    }

    /// Byte length of the longest line in the file (exact once complete).
    /// Used for horizontal scroll range estimation.
    pub fn max_line_bytes(&self) -> u64 {
        self.inner.read().max_line_bytes
    }

    /// 0-based index of the longest line (by byte length).
    pub fn max_line_index(&self) -> u64 {
        self.inner.read().max_line_index
    }

    /// Returns the sparse offsets for persistence.
    pub fn sparse_offsets(&self) -> Vec<u64> {
        self.inner.read().sparse.clone()
    }

    /// Snapshot all currently-known sparse offsets. Clones the vec.
    pub fn snapshot_offsets(&self) -> Vec<u64> {
        self.inner.read().sparse.clone()
    }

    /// Append offsets (called by the builder). `byte_len` may grow over time.
    /// The builder sends full offsets; we downsampling on-the-fly.
    pub fn extend(&self, more: &[u64], new_byte_len: u64) {
        let g = self.inner.read();
        let factor = g.sparse_factor;
        drop(g);
        let mut w = self.inner.write();
        // Downsample and append.
        let sparse_count = if w.sparse.is_empty() {
            0
        } else {
            (w.sparse.len() - 1) as u64 * factor as u64
        };
        let mut i = 0u64;
        for &off in more {
            let line_no = sparse_count + i;
            if line_no % factor as u64 == 0 {
                w.sparse.push(off);
            }
            i += 1;
        }
        w.total_lines += more.len() as u64;
        w.byte_len = new_byte_len;
    }

    /// Mark the index complete and set the final total line count.
    pub fn mark_complete_with_lines(&self, total_lines: u64) {
        let mut w = self.inner.write();
        w.complete = true;
        w.total_lines = total_lines;
    }

    /// Get the byte offset of the start of `line` (0-indexed).
    /// Returns `None` if the index is empty or `line` is past the last known line.
    ///
    /// Uses sparse index: returns the nearest sparse byte offset. For non-sparse lines,
    /// this is an underestimate — the caller must scan forward from this offset
    /// to find the exact line start.
    pub fn offset_of_line(&self, line: u64) -> Option<u64> {
        let g = self.inner.read();
        if g.sparse.is_empty() {
            return None;
        }
        let factor = g.sparse_factor as u64;
        // Clamp to last sparse entry if beyond.
        let sparse_idx = if (line / factor) as usize >= g.sparse.len() {
            g.sparse.len().saturating_sub(1)
        } else {
            (line / factor) as usize
        };
        let base_offset = g.sparse[sparse_idx];
        Some(base_offset)
    }

    /// Returns (anchor_byte, anchor_line) where anchor is the sparse entry
    /// at or before `line`. Uses binary search on sparse array.
    pub fn resolve_anchor(&self, line: u64) -> (u64, u64) {
        let g = self.inner.read();
        if g.sparse.is_empty() {
            return (0, 0);
        }
        let factor = g.sparse_factor as u64;
        let line = line.min(g.total_lines.saturating_sub(1));
        let sparse_idx = (line / factor) as usize;
        let sparse_idx = sparse_idx.min(g.sparse.len().saturating_sub(1));
        let anchor_byte = g.sparse[sparse_idx];
        let anchor_line = sparse_idx as u64 * factor;
        (anchor_byte, anchor_line)
    }

    /// Returns (anchor_byte, anchor_line) where anchor is the sparse entry
    /// at or before `byte`. Uses binary search on sparse array.
    pub fn anchor_for_byte(&self, byte: u64) -> (u64, u64) {
        let g = self.inner.read();
        if g.sparse.is_empty() {
            return (0, 0);
        }
        let factor = g.sparse_factor as u64;
        let i = match g.sparse.binary_search(&byte) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        };
        let anchor_byte = g.sparse[i];
        let anchor_line = i as u64 * factor;
        (anchor_byte, anchor_line)
    }

    /// Find the line that contains byte offset `byte`.
    /// Returns the 0-indexed line number.
    ///
    /// Uses sparse index: binary search on sparse entries + scan.
    pub fn line_of_byte(&self, byte: u64) -> u64 {
        let g = self.inner.read();
        if g.sparse.is_empty() || byte == 0 {
            return 0;
        }
        let factor = g.sparse_factor as u64;
        // Binary search on sparse: find largest sparse offset <= byte.
        match g.sparse.binary_search(&byte) {
            Ok(i) => i as u64 * factor,
            Err(0) => 0,
            Err(i) => {
                // sparse[i-1] < byte <= sparse[i]
                // Scan forward from sparse[i-1] to find exact line.
                let base_offset = g.sparse[i - 1];
                let base_line = (i - 1) as u64 * factor;
                // We need to scan the file to find exact line.
                // Since we don't have mmap here, return an estimate.
                // The caller (engine) will use read_line_scan for precision.
                let next_offset = if i < g.sparse.len() {
                    g.sparse[i]
                } else {
                    g.byte_len
                };
                let block_size = next_offset - base_offset;
                if block_size == 0 {
                    return base_line;
                }
                // Estimate how many lines in this block.
                let est_lines_in_block = if i < g.sparse.len() {
                    factor
                } else {
                    // Last block: estimate
                    ((g.byte_len - base_offset) / 64).min(factor) as u64
                };
                if est_lines_in_block == 0 {
                    return base_line;
                }
                let offset_in_block = byte - base_offset;
                let estimated = base_line + (est_lines_in_block * offset_in_block / block_size);
                estimated.min(base_line + factor)
            }
        }
    }
}

/// Streaming index builder.
///
/// Strategy: chunk the file into 8 MiB slices, find newlines in parallel via
/// rayon + `memchr`. Each chunk's offsets are stored as a `Vec<u64>` and
/// merged into the global index as they complete.
pub struct IndexBuilder {
    mmap: MmapBackend,
    /// Streaming scan window in bytes (default [`SCAN_WINDOW`], from
    /// `EngineConfig::scan_window_mb`).
    scan_window: u64,
    /// Optional early-abort flag (set by `set_cancel`). When set, the build
    /// stops between windows within ~one window of I/O (~tens of ms) instead
    /// of scanning the whole file to the end.
    cancel: Option<Arc<AtomicBool>>,
}

impl IndexBuilder {
    pub fn new(mmap: MmapBackend) -> Self {
        Self {
            mmap,
            scan_window: SCAN_WINDOW,
            cancel: None,
        }
    }

    /// Configure the streaming scan window in bytes (callers derive it from
    /// `EngineConfig::scan_window_mb`). The result of the build is identical
    /// for any window size — it only trades memory (two windows resident)
    /// against boundary overhead.
    pub fn set_scan_window(&mut self, bytes: u64) {
        self.scan_window = bytes.max(4096);
    }

    /// Parallel sub-chunk size: split the window so there's ~one chunk per
    /// scan-pool thread, clamped to [512 KiB, 8 MiB]. Scales with the
    /// configured window and the machine's core count.
    fn scan_chunk(window: u64) -> usize {
        let threads = crate::parallel::scan_pool().current_num_threads().max(1);
        ((window as usize) / threads).clamp(512 * 1024, 8 * 1024 * 1024)
    }

    /// Enable early abort: the build checks `flag` between windows and returns
    /// an error as soon as it is set. Used by the background indexer so a file
    /// switch stops the stale scan quickly instead of reading the whole file.
    pub fn set_cancel(&mut self, flag: Arc<AtomicBool>) {
        self.cancel = Some(flag);
    }

    #[inline]
    fn canceled(&self) -> bool {
        self.cancel.as_ref().map_or(false, |c| c.load(Ordering::Relaxed))
    }

    /// Build the index fully and return the collected offsets.
    /// `progress` is called with bytes scanned / total bytes for UI feedback.
    ///
    /// Scans via [`ScanReader`](super::scan_reader::ScanReader): one window at
    /// a time, streamed into a reusable buffer, so a huge file never floods the
    /// OS page cache.
    ///
    /// A line start is reported by the window containing its `\n`, so window
    /// boundaries never split or double-count a line.
    pub fn build_with_progress<F>(&self, mut progress: F) -> Result<Vec<u64>>
    where
        F: FnMut(u64, u64),
    {
        let total = self.mmap.size();
        if total == 0 {
            return Ok(Vec::new());
        }

        let stream = super::scan_reader::WindowStream::open(self.mmap.path(), 0, self.scan_window)?;
        let chunk_size = Self::scan_chunk(self.scan_window);

        let mut offsets: Vec<u64> = Vec::with_capacity(total as usize / 80);
        progress(0, total);

        while let Some(win) = stream.next()? {
            if self.canceled() {
                anyhow::bail!("cancelled");
            }
            let slice = win.as_slice();
            let wstart = win.start();
            let chunks: Vec<Vec<u64>> = crate::parallel::scan_pool().install(|| {
                slice
                    .par_chunks(chunk_size)
                    .enumerate()
                    .map(|(i, chunk)| {
                        let base = wstart + (i * chunk_size) as u64;
                        memchr::memchr_iter(b'\n', chunk)
                            .map(|nl| base + nl as u64 + 1)
                            .collect()
                    })
                    .collect()
            });
            // Merge. Every offset reported is a valid line start (the byte
            // immediately after a `\n`). We DO NOT drop any offset.
            for mut local in chunks {
                offsets.append(&mut local);
            }
            progress(win.start() + win.len() as u64, total);
        }

        // Ensure line 0 starts at byte 0. The file always begins a line,
        // whether or not it starts with `\n`.
        if offsets.first().copied() != Some(0) {
            offsets.insert(0, 0);
        }

        progress(total, total);
        Ok(offsets)
    }

    /// Build a SPARSE line index directly, without ever materialising the full
    /// offsets array (the old path built all N line starts first, then
    /// downsampled — 800 MB transient for 100M lines).
    ///
    /// Fused single pass over the file, windowed ([`ScanReader`](super::scan_reader::ScanReader)):
    /// each window first counts its `\n`s (sub-pass A), then samples the byte
    /// offset of each line whose GLOBAL index is a multiple of `SPARSE_FACTOR`
    /// (sub-pass B, using the running count as the window's global start line).
    /// The file is read from disk ONCE — sub-pass B reads the window from RAM
    /// while it is still buffered. Memory stays bounded to ~one window
    /// regardless of file size.
    pub fn build_sparse_with_progress<F>(&self, mut progress: F) -> Result<IndexBuildOutcome>
    where
        F: FnMut(u64, u64),
    {
        let total = self.mmap.size();
        if total == 0 {
            return Ok(IndexBuildOutcome {
                sparse: Vec::new(),
                total_lines: 0,
                max_line_bytes: 0,
                max_line_index: 0,
            });
        }
        let chunk = Self::scan_chunk(self.scan_window);
        let factor = SPARSE_FACTOR as u64;
        let stream = super::scan_reader::WindowStream::open(self.mmap.path(), 0, self.scan_window)?;

        let mut sparse: Vec<u64> = Vec::new();
        let mut max_span = 0u64;
        let mut max_idx = 0u64;
        sparse.push(0); // line 0 always starts at byte 0
        let mut total_newlines = 0u64; // newlines strictly before the current window
        let mut prev_last_nl: Option<u64> = None; // last `\n` of the previous chunk
        let mut first_part = true;
        let mut ends_with_newline = false;

        progress(0, total);
        while let Some(win) = stream.next()? {
            if self.canceled() {
                anyhow::bail!("cancelled");
            }
            let slice = win.as_slice();
            let start = win.start();
            let len = win.len() as u64;
            if start + len >= total {
                ends_with_newline = slice.last() == Some(&b'\n');
            }

            // Sub-pass A: newline count per chunk (parallel).
            let counts: Vec<u64> = crate::parallel::scan_pool().install(|| {
                slice
                    .par_chunks(chunk)
                    .map(|c| memchr::memchr_iter(b'\n', c).count() as u64)
                    .collect()
            });
            // prefix[i] = newlines before chunk i within this window.
            let mut prefix = Vec::with_capacity(counts.len() + 1);
            let mut acc = 0u64;
            for &c in &counts {
                prefix.push(acc);
                acc += c;
            }

            // Sub-pass B: sample lines at global multiples of SPARSE_FACTOR
            // and track consecutive-newline gaps (parallel; reads window from RAM).
            let parts: Vec<SparseChunk> = crate::parallel::scan_pool().install(|| {
                slice
                    .par_chunks(chunk)
                    .enumerate()
                    .map(|(i, c)| {
                        let base = start + (i * chunk) as u64;
                        let g_start = total_newlines + prefix[i];
                        // Local newline j is global newline #(g_start + j); it ends
                        // line (g_start + j), so the NEXT line (index g_start + j + 1)
                        // starts at byte p+1. Keep it iff that index is a multiple.
                        let first_keep = (factor - ((g_start + 1) % factor)) % factor;
                        let mut skip = first_keep;
                        let mut offsets: Vec<u64> = Vec::new();
                        let mut first_nl: Option<u64> = None;
                        let mut last_nl: Option<u64> = None;
                        let mut prev_nl: Option<u64> = None;
                        let mut max_gap = 0u64;
                        let mut max_gap_line = 0u64;
                        for (j, nl) in memchr::memchr_iter(b'\n', c).enumerate() {
                            let p = base + nl as u64;
                            if first_nl.is_none() {
                                first_nl = Some(p);
                            }
                            // Gap between local newline j-1 and j belongs to line
                            // (g_start + j) — equal to the span of consecutive line starts.
                            if let Some(pp) = prev_nl {
                                let gap = p - pp;
                                if gap > max_gap {
                                    max_gap = gap;
                                    max_gap_line = g_start + j as u64;
                                }
                            }
                            prev_nl = Some(p);
                            last_nl = Some(p);
                            if skip == 0 {
                                offsets.push(p + 1);
                                skip = factor - 1;
                            } else {
                                skip -= 1;
                            }
                        }
                        SparseChunk { offsets, first_nl, last_nl, max_gap, max_gap_line }
                    })
                    .collect()
            });

            // Merge this window's parts into the global sparse array.
            for (i, part) in parts.iter().enumerate() {
                // Line 0's span: (start of line 1) − 0 = first newline + 1.
                if first_part {
                    if let Some(f) = part.first_nl {
                        let s = f + 1;
                        if s > max_span {
                            max_span = s;
                            max_idx = 0;
                        }
                    }
                    first_part = false;
                }
                if part.max_gap > max_span {
                    max_span = part.max_gap;
                    max_idx = part.max_gap_line;
                }
                // Gap spanning a chunk (or window) boundary belongs to the first
                // line of the next chunk (its global start line).
                if let Some(prev_last) = prev_last_nl {
                    if let Some(next_first) = part.first_nl {
                        let gap = next_first - prev_last;
                        if gap > max_span {
                            max_span = gap;
                            max_idx = total_newlines + prefix[i];
                        }
                    }
                }
                sparse.extend_from_slice(&part.offsets);
                prev_last_nl = part.last_nl;
            }

            total_newlines += acc;
            progress(start + len, total);
        }

        // Final (un-terminated) line's span: bytes after the last newline.
        if let Some(last_nl) = prev_last_nl {
            let s = total - (last_nl + 1);
            if s > max_span {
                // Only beats the others when the file doesn't end in `\n`,
                // in which case this is exactly the last line (total_lines − 1).
                max_span = s;
                max_idx = total_newlines;
            }
        }

        let total_lines = if ends_with_newline {
            total_newlines
        } else {
            total_newlines + 1
        };

        progress(total, total);
        Ok(IndexBuildOutcome {
            sparse,
            total_lines,
            max_line_bytes: max_span,
            max_line_index: max_idx,
        })
    }

    /// Legacy strategy: materialise the FULL offsets array (every line start),
    /// then downsample to sparse.  Uses ~8 bytes per line transient memory but
    /// scans the file only ONCE — faster on cold-cache first opens, at the cost
    /// of a much larger peak (800 MB for 100M lines).  Produces the identical
    /// `IndexBuildOutcome`, so callers/persist need not know which strategy ran.
    pub fn build_full_with_progress<F>(&self, mut progress: F) -> Result<IndexBuildOutcome>
    where
        F: FnMut(u64, u64),
    {
        let full = self.build_with_progress(&mut progress)?;
        let size = self.mmap.size();
        if full.is_empty() {
            return Ok(IndexBuildOutcome {
                sparse: Vec::new(),
                total_lines: 0,
                max_line_bytes: 0,
                max_line_index: 0,
            });
        }
        let total_lines =
            if size > 0 && self.mmap.as_slice()[size as usize - 1] == b'\n' {
                full.len() as u64 - 1
            } else {
                full.len() as u64
            };
        let (max_line_bytes, max_line_index) = LineIndex::compute_max_line(&full, size);
        let sparse = LineIndex::build_sparse(&full, SPARSE_FACTOR);
        Ok(IndexBuildOutcome {
            sparse,
            total_lines,
            max_line_bytes,
            max_line_index,
        })
    }
}