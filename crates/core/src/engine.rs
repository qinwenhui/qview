//! Core engine: file access, search, edit buffer. UI-agnostic — the TUI and
//! future GUI frontends both go through this module.

use std::cell::UnsafeCell;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use encoding_rs::Encoding;

use crate::cache::LineCache;
use crate::config::{self, EngineConfig, IndexBuildMode, SearchConfig};
use crate::edit::{writeback, EditBuffer, EditOp, LineEditor};
use crate::edit::save_task::{BackgroundSave, SaveProgress};
use crate::file::{BackgroundIndexer, IndexBuilder, IndexProgress, LineIndex, MmapBackend};
use crate::file::persist::{file_meta, peek_header, write_index, IndexFile};
use crate::search::{BackgroundSearch, SearchOptions, SearchResults};

/// The data/logic layer. Knows nothing about terminal modes, viewports,
/// or key bindings. A GUI frontend would create one of these and poll it.
///
/// # Windows `\\?\` prefix normalization
///
/// `std::fs::canonicalize` on Windows returns *extended-length* paths like
/// `\\?\D:\foo.log`. That string feeds the `.qli` cache key (xxhash of the
/// path), so the *same* file opened via a canonicalized path vs. a plain one
/// computes two different keys. The GUI opens files with the plain form while
/// `DocumentService` canonicalizes, so the agent's engine missed the cache and
/// re-indexed whole files. `normalize_cache_path` strips the prefix so all
/// callers land on one stable key.
pub struct Engine {
    pub mmap: MmapBackend,
    pub index: LineIndex,
    pub cache: LineCache,
    pub total_lines: u64,
    pub known_size: u64,

    // index
    pub bg_indexer: Option<BackgroundIndexer>,
    pub index_progress: Option<String>,

    // search
    pub search_query: String,
    pub search_hash: u64,
    pub search: SearchResults,
    pub bg_search: Option<BackgroundSearch>,
    pub search_progress: Option<String>,

    // save (background writeback)
    pub bg_save: Option<BackgroundSave>,
    pub save_progress: Option<u8>,
    /// True while the in-flight save is a "另存为" (write to a new path; the
    /// engine's working file and edits stay untouched).
    pub save_is_copy: bool,
    /// Destination of the last "另存为" (used in the result message / log).
    pub save_as_path: Option<PathBuf>,
    /// Active search tuning (sample interval, max samples). Copied from
    /// `EngineConfig::search` at construction time. Mutating the config on
    /// a live engine is undefined — create a new engine to change it.
    pub search_config: SearchConfig,

    /// Which index-build strategy to use for large files. Copied from
    /// `EngineConfig::index_build_mode` at construction time.
    pub index_build_mode: IndexBuildMode,

    /// Streaming scan window in bytes, from `EngineConfig::scan_window_mb`.
    /// Used by index builds and search (both read the file through the
    /// streaming [`WindowStream`]). Copied at construction time.
    pub scan_window: u64,

    // edit
    pub edits: EditBuffer,

    // file identity
    pub path: PathBuf,
    pub original_size: u64,

    /// Last resolved line number for incremental scanning during sequential reads.
    /// Set by read_line when scanning from sparse index.
    last_resolved_line: UnsafeCell<u64>,
    /// Byte offset of the end of last_resolved_line (i.e., start of next line).
    last_resolved_next_start: UnsafeCell<u64>,
    /// Sparse chunk index of last_resolved_line (for chunk-boundary detection).
    last_sparse_idx: UnsafeCell<u64>,

    // status
    pub message: Option<String>,
    pub message_until: Option<Instant>,

    /// Where to persist/load the `.qli` index cache for this file.
    /// `None` means no disk caching (small file or caching disabled).
    index_cache_path: Option<PathBuf>,

    /// Text encoding used to decode bytes → Rust `String`.
    pub encoding: &'static Encoding,
}

/// Normalize a path for a stable `.qli` cache key.
///
/// Windows `std::fs::canonicalize` yields `\\?\D:\...` (extended-length) and
/// `\\?\UNC\server\share\...` (network). Strip the prefix so the same file
/// always hashes identically no matter how the caller obtained the path.
#[cfg(windows)]
fn normalize_cache_path(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!("\\\\{}", rest))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p
    }
}

/// Non-Windows: paths are already stable, no normalization needed.
#[cfg(not(windows))]
fn normalize_cache_path(p: PathBuf) -> PathBuf {
    p
}

impl Engine {
    // ------------------------------------------------------------------
    // Construction
    // ------------------------------------------------------------------

    /// Try to load a cached `.qli` index from `index_path`.
    /// Validates file size / mtime / inode against the current log file
    /// so stale caches are rejected automatically.
    fn try_load_cache(
        log_path: &Path,
        index_path: &Path,
        size: u64,
    ) -> Option<(LineIndex, u64)> {
        if !index_path.exists() {
            return None;
        }
        let meta = file_meta(log_path).ok()?;
        let header = peek_header(index_path).ok()?;
        if header.file_size != meta.size
            || header.file_mtime != meta.mtime
            || header.file_inode != meta.inode
        {
            return None;
        }
        let idx = IndexFile::open(index_path).ok()?;
        let sparse_offsets: Vec<u64> = if idx.header.offset_size == 4 {
            idx.offsets_u32().iter().map(|&o| o as u64).collect()
        } else {
            idx.offsets_u64().to_vec()
        };
        let sparse_factor = if idx.header.flags & 1 != 0 {
            idx.header.sparse_factor
        } else {
            // v1 format: full offsets — downsample on the fly
            crate::file::index::SPARSE_FACTOR
        };
        Some((
            LineIndex::from_sparse(
                sparse_offsets,
                size,
                sparse_factor,
                idx.header.line_count,
                idx.header.max_line_bytes,
                idx.header.max_line_index,
            ),
            idx.header.line_count,
        ))
    }

    /// Backward-compatible constructor — uses default `EngineConfig`.
    pub fn new(path: PathBuf) -> Result<Self> {
        Self::with_config(path, EngineConfig::default())
    }

    /// Create an engine with the given configuration.
    ///
    /// # File-size strategy
    ///
    /// | File size           | Indexing             | Disk cache        |
    /// |---------------------|----------------------|-------------------|
    /// | ≤ threshold         | Synchronous, memory  | Never             |
    /// | > threshold         | Background thread    | If configured     |
    ///
    /// Small files are ready immediately — no progress bar, no `.qli` file.
    /// Large files try the `.qli` cache first; cache miss triggers background
    /// indexing with progress updates.
    pub fn with_config(path: PathBuf, config: EngineConfig) -> Result<Self> {
        // Apply the scan-thread setting before any index/search work can run.
        // 0 = auto (leave one core for the UI). Cheap atomic store.
        crate::parallel::set_scan_threads(config.scan_threads);

        let mmap = MmapBackend::open(&path)?;
        let size = mmap.size();
        // 归一化 Windows `\\?\` 扩展路径前缀：`.qli` 缓存键 = xxhash(canonical)，
        // 而 GUI 打开文件传的是 `D:\...`、Agent 经 DocumentService::canonical 传的是
        // `\\?\D:\...`（std::fs::canonicalize 在 Windows 会加前缀）。两者若不归一，
        // 同一文件会算出两个不同的缓存键 → Agent 侧引擎错过缓存、把整份大文件
        // 重新建一遍索引（50 GB 级要几分钟），且 get_document_info 退回「字节/80」估算。
        let canonical = normalize_cache_path(mmap.path().to_path_buf());
        let scan_window = (config.scan_window_mb as u64) << 20; // MiB → bytes

        // Determine whether and where to cache.
        let index_cache_path = if size <= config.small_file_threshold {
            // Small file — never cache to disk.
            None
        } else {
            config.cache_path(&canonical)
        };

        let (index, total_lines) = if size <= config.small_file_threshold {
            // ---- small file: synchronous index, memory only ----
            let mut builder = IndexBuilder::new(mmap.clone());
            builder.set_scan_window(scan_window);
            match builder.build_with_progress(|_, _| {}) {
                Ok(offsets) => {
                    let lines = Self::compute_line_count(&mmap, &offsets);
                    (LineIndex::from_vec(offsets, size), lines)
                }
                Err(_) => (LineIndex::new(size), 0),
            }
        } else {
            // ---- large file: try cache, else defer to background ----
            if let Some(ref cp) = index_cache_path {
                if let Some(cached) = Self::try_load_cache(&canonical, cp, size) {
                    cached
                } else {
                    (LineIndex::new(size), 0)
                }
            } else {
                (LineIndex::new(size), 0)
            }
        };

        let cache = LineCache::new(config.line_cache_capacity, config.line_cache_capacity / 2);
        let encoding = config::resolve_encoding(&config.encoding);

        Ok(Self {
            mmap,
            index,
            cache,
            total_lines,
            known_size: size,
            search_query: String::new(),
            search_hash: 0,
            search: SearchResults::new(),
            bg_search: None,
            search_progress: None,
            bg_save: None,
            save_progress: None,
            save_is_copy: false,
            save_as_path: None,
            search_config: config.search.clone(),
            index_build_mode: config.index_build_mode,
            scan_window,
            bg_indexer: None,
            index_progress: None,
            edits: {
                let mut e = EditBuffer::new();
                e.rebuild_mapping();
                e
            },
            path: canonical,
            original_size: size,
            last_resolved_line: UnsafeCell::new(u64::MAX),
            last_resolved_next_start: UnsafeCell::new(0),
            last_sparse_idx: UnsafeCell::new(u64::MAX),
            message: None,
            message_until: None,
            index_cache_path,
            encoding,
        })
    }

    /// Decode raw bytes using the engine's configured encoding.
    /// Falls back to lossy UTF-8 for undecodable sequences (same behaviour
    /// as `String::from_utf8_lossy` but encoding-aware).
    #[inline]
    fn decode(&self, bytes: &[u8]) -> String {
        if bytes.is_empty() {
            return String::new();
        }
        let (text, _, had_error) = self.encoding.decode(bytes);
        if had_error {
            // If the encoding produced replacement characters, fall back
            // to lossy UTF-8 (handles binary garbage gracefully).
            String::from_utf8_lossy(bytes).into_owned()
        } else {
            text.into_owned()
        }
    }

    /// Compute the visible line count from a list of line-start offsets.
    fn compute_line_count(mmap: &MmapBackend, offsets: &[u64]) -> u64 {
        let size = mmap.size();
        if size == 0 {
            return 0;
        }
        if offsets.is_empty() {
            return 0;
        }
        let ends_with_newline = mmap.as_slice()[size as usize - 1] == b'\n';
        if ends_with_newline {
            (offsets.len() as u64).saturating_sub(1)
        } else {
            offsets.len() as u64
        }
    }

    // ------------------------------------------------------------------
    // Messages
    // ------------------------------------------------------------------

    pub fn set_message(&mut self, msg: impl Into<String>) {
        self.message = Some(msg.into());
        self.message_until = Some(Instant::now() + std::time::Duration::from_secs(3));
    }

    pub fn clear_expired_message(&mut self) {
        if let Some(deadline) = self.message_until {
            if Instant::now() >= deadline {
                self.message = None;
                self.message_until = None;
            }
        }
    }

    // ------------------------------------------------------------------
    // Index
    // ------------------------------------------------------------------

    /// Persist the current in-memory index to the configured cache path
    /// (blocking).  No-op when caching is disabled or the file is small.
    fn persist_index(&self) {
        let cache_path = match &self.index_cache_path {
            Some(p) => p,
            None => return,
        };
        let sparse_offsets = self.index.snapshot_offsets();
        if sparse_offsets.is_empty() {
            return;
        }
        // Ensure parent directory exists (important for centralised index dir).
        if let Some(parent) = cache_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let meta = match file_meta(&self.path) {
            Ok(m) => m,
            Err(_) => return,
        };
        let sparse_factor = crate::file::index::SPARSE_FACTOR;
        let (max_line_bytes, max_line_index) =
            (self.index.max_line_bytes(), self.index.max_line_index());
        if let Err(e) = write_index(
            cache_path,
            meta.size,
            meta.mtime,
            meta.inode,
            self.total_lines,
            &sparse_offsets,
            sparse_factor,
            max_line_bytes,
            max_line_index,
        ) {
            eprintln!("[engine] persist index failed: {e}");
        }
    }

    pub fn build_index_blocking(&mut self) -> Result<()> {
        let mut builder = IndexBuilder::new(self.mmap.clone());
        builder.set_scan_window(self.scan_window);
        let outcome = match self.index_build_mode {
            IndexBuildMode::Sparse => builder.build_sparse_with_progress(|_, _| {})?,
            IndexBuildMode::Full => builder.build_full_with_progress(|_, _| {})?,
        };
        let size = self.mmap.size();
        self.total_lines = outcome.total_lines;
        self.index = LineIndex::from_sparse(
            outcome.sparse,
            size,
            crate::file::index::SPARSE_FACTOR,
            outcome.total_lines,
            outcome.max_line_bytes,
            outcome.max_line_index,
        );
        self.cache.invalidate_raw();
        self.persist_index();
        Ok(())
    }

    /// Start building the line index on a background thread.
    /// The UI stays responsive; call `poll_bg_index` each frame until done.
    /// When complete the index is automatically persisted if caching is enabled.
    pub fn submit_build_index(&mut self) {
        let mmap = self.mmap.clone();
        let log_path = self.path.clone();
        let cache_path = self.index_cache_path.clone();
        self.bg_indexer = Some(BackgroundIndexer::spawn(
            mmap,
            log_path,
            cache_path,
            self.index_build_mode,
            self.scan_window,
        ));
        self.index_progress = Some("indexing... 0%".to_string());
    }

    /// Drain progress messages from the background indexer.
    /// Returns `(done, result_message)`.
    /// When `done` is true the index is fully built and `total_lines` is set.
    pub fn poll_bg_index(&mut self) -> (bool, Option<String>) {
        let mut done = false;
        let mut result_msg: Option<String> = None;
        let mut messages: Vec<IndexProgress> = Vec::new();
        let mut elapsed = std::time::Duration::ZERO;
        if let Some(bg) = &self.bg_indexer {
            while let Some(p) = bg.poll() {
                messages.push(p);
            }
            elapsed = bg.elapsed();
        }
        for p in messages {
            match p {
                IndexProgress::Percent(pct) => {
                    // A progress ping can arrive AFTER the terminal message
                    // (the worker's ping thread races a fast build).  Once we
                    // have seen a terminal state, ignore it — otherwise the
                    // progress bar would come back to life after completion.
                    if done {
                        continue;
                    }
                    self.index_progress =
                        Some(format!("indexing... {}%", pct));
                }
                IndexProgress::Done(outcome) => {
                    let last_byte = self.mmap.size();
                    let total_lines = outcome.total_lines;
                    self.total_lines = total_lines;
                    self.known_size = last_byte;
                    self.index = LineIndex::from_sparse(
                        outcome.sparse,
                        last_byte,
                        crate::file::index::SPARSE_FACTOR,
                        outcome.total_lines,
                        outcome.max_line_bytes,
                        outcome.max_line_index,
                    );
                    self.cache.invalidate_raw();
                    self.index_progress = None;

                    let elapsed_str = if elapsed.as_secs() >= 1 {
                        format!("{:.1}s", elapsed.as_secs_f64())
                    } else {
                        format!("{}ms", elapsed.as_millis())
                    };
                    result_msg = Some(format!(
                        "{} lines indexed in {}",
                        total_lines, elapsed_str,
                    ));
                    done = true;
                }
                IndexProgress::Cancelled => {
                    self.index_progress = None;
                    result_msg = Some("index cancelled".to_string());
                    done = true;
                }
                IndexProgress::Failed(e) => {
                    self.index_progress = None;
                    result_msg = Some(format!("index failed: {}", e));
                    done = true;
                }
            }
        }
        if done {
            self.bg_indexer = None;
        }
        (done, result_msg)
    }

    /// Cancel a running background index. The worker will stop at the next
    /// check-point and the partial results are discarded.
    pub fn cancel_index(&mut self) {
        if let Some(ref bg) = self.bg_indexer {
            bg.cancel();
        }
    }

    pub fn extend_index(&mut self, new_size: u64) -> Result<()> {
        let start = self.known_size;
        if new_size <= start {
            return Ok(());
        }
        let new_bytes = self.mmap.slice(start, (new_size - start) as usize);
        let mut new_offsets: Vec<u64> = memchr::memchr_iter(b'\n', new_bytes)
            .map(|nl| start + nl as u64 + 1)
            .collect();

        let prev_total = self.total_lines;
        let added_newlines = new_offsets.len() as u64;

        let mut current = self.index.snapshot_offsets();
        if let Some(&last) = current.last() {
            if last == self.known_size {
                current.pop();
            }
        }
        current.append(&mut new_offsets);
        self.known_size = new_size;
        self.total_lines = prev_total + added_newlines;

        self.index = LineIndex::from_vec(current, new_size);
        self.cache.invalidate_raw();
        Ok(())
    }

    // ------------------------------------------------------------------
    // Line reading
    // ------------------------------------------------------------------

    /// Exact line number for a given byte offset. Uses sparse index to find
    /// the nearest anchor, then counts newlines in mmap for precision.
    pub fn line_of_byte(&self, byte: u64) -> u64 {
        let slice = self.mmap.as_slice();
        if byte == 0 || slice.is_empty() {
            return 0;
        }
        let byte = byte.min(slice.len() as u64);
        let (anchor_byte, anchor_line) = self.index.anchor_for_byte(byte);
        let count = memchr::memchr_iter(b'\n', &slice[anchor_byte as usize..byte as usize]).count();
        anchor_line + count as u64
    }

    pub fn effective_line_count(&self) -> u64 {
        let delta = self.edits.net_line_delta();
        let base = if self.index.is_complete() {
            self.total_lines
        } else {
            // Coarse estimate during background indexing (~80 bytes/line).
            // Snaps to accurate count when indexing finishes.
            (self.mmap.size() / 80).max(1)
        };
        if delta >= 0 {
            base + delta as u64
        } else {
            base.saturating_sub((-delta) as u64)
        }
    }

    /// 后台索引未完成时，读取逻辑行 `line_no` 的**估算**线性扫描代价（字节）。
    ///
    /// 索引未完成时 `read_line` 退化为 `read_line_scan`——从字节 0 顺序找换行，
    /// 代价 ≈ 该行在文件中的字节偏移。按 ~80 字节/行粗估（与
    /// `effective_line_count` 的估算口径一致），供工具层做「深行读取护栏」：
    /// 代价超过阈值时拒绝扫描，避免大文件深行读取卡住几十秒。
    ///
    /// 索引已完成 → 返回 `None`（走稀疏索引快路径，无需估算）。
    pub fn estimate_read_cost_bytes(&self, line_no: u64) -> Option<u64> {
        if self.index.is_complete() {
            return None;
        }
        Some(line_no.saturating_mul(80).min(self.mmap.size()))
    }

    /// 找出所有超长行（行**内容**字节数 > `threshold`），返回 `(行号, 内容字节数)`。
    ///
    /// 性能铁律：**绝不整文件扫描**（大文件 UI 会卡十几秒）。
    /// - 后台索引未完成 → `None`（调用方暂缓；索引完成后会变成 `Some`）。
    /// - 没有超长行（常见日志）→ 空列表，O(1) 短路，不碰文件内容。
    /// - 有超长行 → 按稀疏锚点窗口定向扫描：每窗口 = `SPARSE_FACTOR` 行，
    ///   窗口字节跨距 ≤ threshold 的直接跳过，只扫可能含超长行的窗口。
    pub fn huge_lines(&self, threshold: u64) -> Option<Vec<(u64, u64)>> {
        if !self.index.is_complete() {
            return None;
        }
        if self.index.max_line_bytes() <= threshold {
            return Some(Vec::new());
        }
        let slice = self.mmap.as_slice();
        let sparse = self.index.sparse_offsets();
        let factor = self.index.sparse_factor() as u64;
        let mut out: Vec<(u64, u64)> = Vec::new();
        for (i, &start_byte) in sparse.iter().enumerate() {
            let end_byte = sparse.get(i + 1).copied().unwrap_or(slice.len() as u64);
            if end_byte.saturating_sub(start_byte) <= threshold {
                continue; // 窗口内不可能有超长行
            }
            let mut byte = start_byte as usize;
            let end = end_byte as usize;
            let mut line = i as u64 * factor;
            while byte < end {
                let rel = memchr::memchr(b'\n', &slice[byte..end]);
                let seg_end = match rel {
                    Some(o) => byte + o,
                    None => end,
                };
                let len = (seg_end - byte) as u64;
                if len > threshold {
                    out.push((line, len));
                }
                line += 1;
                match rel {
                    Some(o) => byte += o + 1,
                    None => break,
                }
            }
        }
        Some(out)
    }

    pub fn read_line(&self, line_no: u64) -> crate::cache::RawLine {
        // During initial background indexing the line-offset index isn't
        // complete yet. Fall back to sequential scanning so the user sees
        // content immediately rather than a blank screen.
        if !self.index.is_complete() {
            return self.read_line_scan(line_no);
        }
        let (phys, inserted_block) = match self.edits.mapping.resolve(
            &self.edits.inserted,
            line_no,
            self.index.line_count(),
        ) {
            Some(t) => t,
            None => return crate::cache::RawLine::default(),
        };
        // Path A: inserted line.
        if let Some((anchor, idx)) = inserted_block {
            let bytes = match self.edits.inserted.get(&anchor).and_then(|v| v.get(idx)) {
                Some(b) => b.clone(),
                None => return crate::cache::RawLine::default(),
            };
            return crate::cache::RawLine {
                text: self.decode(&bytes),
                byte_len: bytes.len(),
                modified: true,
                start_byte: 0,
            };
        }
        // Path B: replaced line.
        let phys = match phys {
            Some(p) => p,
            None => return crate::cache::RawLine::default(),
        };
        if let Some(repl) = self.edits.replaced.get(&phys) {
            return crate::cache::RawLine {
                text: self.decode(repl),
                byte_len: repl.len(),
                modified: true,
                start_byte: 0,
            };
        }
        // Path C: read from mmap.
        let (start, end) = self.mmap_line_bounds(phys);
        let raw_end = if end > 0 && self.mmap.as_slice()[(end - 1) as usize] == b'\n' {
            end - 1
        } else {
            end
        };
        let end_clean = if raw_end > start
            && self.mmap.as_slice()[(raw_end - 1) as usize] == b'\r'
        {
            raw_end - 1
        } else {
            raw_end
        };
        let len = (end_clean - start) as usize;
        let raw = self.mmap.slice(start, len);
        crate::cache::RawLine {
            text: self.decode(raw),
            byte_len: len,
            modified: false,
            start_byte: start,
        }
    }

    /// mmap 中物理行 `phys` 的原始字节范围 `[start, end)`（含行尾换行符，含 \r\n 的 \r）。
    /// 与 `read_line` 的 Path C 共用同一套稀疏快路径 / 锚点扫描，但不做 UTF-8 解码。
    fn mmap_line_bounds(&self, phys: u64) -> (u64, u64) {
        // With sparse index, offset_of_line returns an approximate value (nearest sparse entry).
        // Use chunk-aware incremental scanning: if we're in the same sparse chunk as the
        // last read, continue from the last position (O(1) per line).
        let sparse_factor = self.index.sparse_factor() as u64;
        let current_sparse_idx = phys / sparse_factor;
        let (start, end) = if phys > 0
            && current_sparse_idx == unsafe { *self.last_sparse_idx.get() }
            // Fast path is ONLY valid for the immediately-next physical line:
            // `last_resolved_next_start` holds the byte right after the previous
            // line, i.e. the start of line (last_resolved_line + 1).  Any other
            // forward read inside the same sparse bucket (e.g. a jump from line
            // 0 to line 39) MUST re-anchor via binary search, otherwise it would
            // return the line stored at the cached position instead of the
            // requested line — corrupting window scans, selection anchors, etc.
            && phys == unsafe { *self.last_resolved_line.get() } + 1
        {
            // Same sparse chunk, exact next line: continue from cached position (O(1)).
            let start = unsafe { *self.last_resolved_next_start.get() };
            let slice = self.mmap.as_slice();
            let total = slice.len();
            let line_end = match memchr::memchr(b'\n', &slice[start as usize..]) {
                Some(nl) => start + nl as u64 + 1,
                None => total as u64,
            };
            (start, line_end)
        } else {
            // Different chunk or random access: binary search in sparse index to find
            // the correct anchor, then scan from there.
            let (anchor_byte, anchor_line) = self.index.resolve_anchor(phys);
            Self::scan_line_from_anchor(self.mmap.as_slice(), anchor_byte, anchor_line, phys)
        };
        // Update incremental state.
        unsafe { *self.last_resolved_line.get() = phys; }
        unsafe { *self.last_resolved_next_start.get() = end; }
        unsafe { *self.last_sparse_idx.get() = current_sparse_idx; }
        (start, end)
    }

    /// 逻辑行 `line_no` 的字节范围 `[start, end)`（**不含**行尾换行符 / \r）。
    /// 不做 UTF-8 解码 —— 供超长行扫描 / 视觉行模型 / 命中字节定位等
    /// 只关心边界不关心内容的热路径使用（`read_line` 对几 MB 单行会整行解码，
    /// 这里 O(1)-ish 返回范围即可，省掉每次几 MB 的 String 分配）。
    pub fn line_byte_range(&self, line_no: u64) -> Option<(u64, u64)> {
        if !self.index.is_complete() {
            // 后台建索引期间回退到扫描式读行（此时文件通常还没交互浏览完）。
            let raw = self.read_line(line_no);
            return Some((raw.start_byte, raw.start_byte + raw.byte_len as u64));
        }
        let (phys, inserted_block) = match self.edits.mapping.resolve(
            &self.edits.inserted,
            line_no,
            self.index.line_count(),
        ) {
            Some(t) => t,
            None => return None,
        };
        // Path A: inserted line.
        if let Some((anchor, idx)) = inserted_block {
            let bytes = match self.edits.inserted.get(&anchor).and_then(|v| v.get(idx)) {
                Some(b) => b,
                None => return None,
            };
            return Some((0, bytes.len() as u64));
        }
        // Path B: replaced line.
        let phys = match phys {
            Some(p) => p,
            None => return None,
        };
        if let Some(repl) = self.edits.replaced.get(&phys) {
            return Some((0, repl.len() as u64));
        }
        // Path C: read from mmap.
        let (start, end) = self.mmap_line_bounds(phys);
        let slice = self.mmap.as_slice();
        let raw_end = if end > 0 && slice[(end - 1) as usize] == b'\n' {
            end - 1
        } else {
            end
        };
        let end_clean = if raw_end > start && slice[(raw_end - 1) as usize] == b'\r' {
            raw_end - 1
        } else {
            raw_end
        };
        Some((start, end_clean))
    }

    /// Scan forward from `approx_start` to find the exact byte range of `target_line`.
    /// Uses sparse_factor to determine how many lines to scan from the anchor.
    /// Scan from a known sparse anchor (byte offset `anchor_byte`, which is the start of
    /// line `anchor_line`) forward to find the exact byte range of `target_line`.
    /// `anchor_line` is the line number at `anchor_byte`.
    fn scan_line_from_anchor(
        slice: &[u8],
        anchor_byte: u64,
        anchor_line: u64,
        target_line: u64,
    ) -> (u64, u64) {
        let total = slice.len();
        let target = target_line as usize;
        let anchor = anchor_line as usize;
        let start = anchor_byte as usize;

        let mut pos = start;
        let mut current = anchor;
        loop {
            if current >= target {
                break;
            }
            match memchr::memchr(b'\n', &slice[pos..]) {
                Some(nl) => {
                    pos += nl + 1;
                    current += 1;
                }
                None => {
                    pos = total;
                    break;
                }
            }
        }
        let line_start = pos as u64;
        let line_end = match memchr::memchr(b'\n', &slice[pos..]) {
            Some(nl) => (pos + nl + 1) as u64,
            None => total as u64,
        };
        (line_start, line_end)
    }

    /// Fallback line reader used while the index is building.
    /// Scans for newlines sequentially — O(line_no) — but fast enough for
    /// the first few hundred visible lines.
    fn read_line_scan(&self, line_no: u64) -> crate::cache::RawLine {
        let slice = self.mmap.as_slice();
        let total_len = slice.len();
        let mut current = 0u64;
        let mut pos = 0usize;

        while current < line_no && pos < total_len {
            match memchr::memchr(b'\n', &slice[pos..]) {
                Some(nl) => {
                    pos += nl + 1;
                    current += 1;
                }
                None => {
                    pos = total_len;
                    break;
                }
            }
        }

        if pos >= total_len {
            return crate::cache::RawLine::default();
        }

        let line_end = match memchr::memchr(b'\n', &slice[pos..]) {
            Some(nl) => pos + nl,
            None => total_len,
        };

        // Strip trailing \r if present.
        let end = if line_end > pos && slice[line_end - 1] == b'\r' {
            line_end - 1
        } else {
            line_end
        };

        crate::cache::RawLine {
            text: self.decode(&slice[pos..end]),
            byte_len: end - pos,
            modified: false,
            start_byte: pos as u64,
        }
    }

    /// Byte length of the longest line in the file (exact once index complete).
    pub fn max_line_byte_len(&self) -> u64 {
        self.index.max_line_bytes()
    }

    /// 0-based index of the longest line (by byte length).
    pub fn longest_line_index(&self) -> u64 {
        self.index.max_line_index()
    }

    pub fn is_modified(&self) -> bool {
        self.edits.dirty
    }

    // ------------------------------------------------------------------
    // Save / reload
    // ------------------------------------------------------------------

    pub fn reload(&mut self) -> Result<()> {
        self.edits.clear();
        self.cache.invalidate_raw();
        let new_mmap = MmapBackend::open(&self.path)?;
        let size = new_mmap.size();
        self.mmap = new_mmap;
        self.known_size = size;
        self.original_size = size;
        self.reset_line_cache();
        let mut builder = IndexBuilder::new(self.mmap.clone());
        builder.set_scan_window(self.scan_window);
        let offsets = builder.build_with_progress(|_, _| {})?;
        self.total_lines = Self::compute_line_count(&self.mmap, &offsets);
        self.index = LineIndex::from_vec(offsets, size);
        self.cache.invalidate_raw();
        Ok(())
    }

    /// Synchronous save (TUI path). Writes the post-edit file atomically:
    /// original → `.bak` backup (first save only), temp write, atomic rename,
    /// then a full reindex. For the GUI, prefer `submit_save` (background).
    pub fn save(&mut self) -> Result<()> {
        if !self.edits.dirty {
            return Ok(());
        }
        let offsets = writeback::full_offsets(&self.mmap);
        let backup = self.path.with_extension("log.bak");
        let new_lc = writeback::projected_line_count(self.total_lines, &self.edits);
        let path = self.path.clone();
        let tmp = {
            let mut p = path.as_os_str().to_owned();
            p.push(".writetmp");
            std::path::PathBuf::from(p)
        };

        // Backup the ORIGINAL before overwriting (never clobber a prior backup).
        if !backup.exists() {
            std::fs::copy(&path, &backup)
                .with_context(|| format!("备份到 {}", backup.display()))?;
        }

        // Stream the original straight from the live mmap — no full-file Vec
        // copy, so a large file save costs memory O(1) instead of O(file).
        writeback::write_to_path(&tmp, self.mmap.as_slice(), &offsets, &self.edits, new_lc)?;

        // Release the original mmap (Windows would otherwise lock `path`),
        // then atomically replace it with the written file.
        let mut tmp_mmap = MmapBackend::open(&tmp)?;
        std::mem::swap(&mut self.mmap, &mut tmp_mmap);
        drop(tmp_mmap);
        std::fs::rename(&tmp, &path)?;

        self.mmap = MmapBackend::open(&path)?;
        let size = self.mmap.size();
        self.known_size = size;
        self.original_size = size;
        self.reset_line_cache();

        let mut builder = IndexBuilder::new(self.mmap.clone());
        builder.set_scan_window(self.scan_window);
        let offsets = builder.build_with_progress(|_, _| {})?;
        self.total_lines = Self::compute_line_count(&self.mmap, &offsets);
        self.index = LineIndex::from_vec(offsets, size);
        self.edits.clear();
        self.cache.invalidate_raw();
        Ok(())
    }

    /// The fast-path line-resolution cache caches byte offsets into the OLD
    /// mmap; it MUST be invalidated whenever the mmap is swapped (reload/save).
    fn reset_line_cache(&mut self) {
        unsafe {
            *self.last_resolved_line.get() = u64::MAX;
            *self.last_resolved_next_start.get() = 0;
            *self.last_sparse_idx.get() = u64::MAX;
        }
    }

    // ------------------------------------------------------------------
    // Background save (GUI path)
    // ------------------------------------------------------------------

    /// Start an asynchronous save of the current edits. The write happens on a
    /// worker thread; poll `poll_bg_save` until it reports done. Returns false
    /// if there is nothing to save or a save is already in flight.
    pub fn submit_save(&mut self) -> bool {
        if !self.edits.dirty || self.bg_save.is_some() {
            return false;
        }
        self.save_is_copy = false;
        self.bg_save = Some(BackgroundSave::spawn(
            self.mmap.clone(),
            self.path.clone(),
            self.total_lines,
            self.edits.clone(),
        ));
        self.save_progress = Some(0);
        true
    }

    /// Start an asynchronous "另存为": write the post-edit content to `dst`
    /// without touching the working file. Returns false if a save is already
    /// in flight.
    pub fn submit_save_as(&mut self, dst: PathBuf) -> bool {
        if self.bg_save.is_some() {
            return false;
        }
        self.save_is_copy = true;
        self.save_as_path = Some(dst.clone());
        self.bg_save = Some(BackgroundSave::spawn_copy(
            self.mmap.clone(),
            dst,
            self.total_lines,
            self.edits.clone(),
        ));
        self.save_progress = Some(0);
        true
    }

    pub fn cancel_save(&mut self) {
        if let Some(bg) = &self.bg_save {
            bg.cancel();
        }
    }

    pub fn save_in_flight(&self) -> bool {
        self.bg_save.is_some()
    }

    /// Drain save progress. Returns `(done, message, ok)` — `done` true once the
    /// save finished (success or failure), `message` a user-facing result
    /// (success includes the elapsed time and the written path), `ok` true only
    /// when the save actually succeeded.
    pub fn poll_bg_save(&mut self) -> (bool, Option<String>, bool) {
        let events: Vec<SaveProgress> = {
            let Some(bg) = &self.bg_save else {
                return (false, None, false);
            };
            let mut v = Vec::new();
            while let Some(p) = bg.poll() {
                v.push(p);
            }
            v
        };
        if events.is_empty() {
            return (false, None, false);
        }

        // Measure elapsed BEFORE clearing the handle.
        let elapsed = self
            .bg_save
            .as_ref()
            .map(|b| b.elapsed())
            .unwrap_or_default();
        let elapsed_str = if elapsed.as_secs() >= 1 {
            format!("{:.2}s", elapsed.as_secs_f64())
        } else {
            format!("{}ms", elapsed.as_millis())
        };

        let is_copy = self.save_is_copy;
        let mut done = false;
        let mut tmp: Option<PathBuf> = None;
        let mut failed: Option<String> = None;
        let mut cancelled = false;
        for e in events {
            match e {
                SaveProgress::Percent(pct) => self.save_progress = Some(pct),
                SaveProgress::Done(Ok(())) => {
                    tmp = self.bg_save.as_ref().map(|b| b.tmp_path());
                    done = true;
                }
                SaveProgress::Done(Err(err)) => {
                    tmp = self.bg_save.as_ref().map(|b| b.tmp_path());
                    failed = Some(format!("{err}"));
                    done = true;
                }
                SaveProgress::Cancelled => {
                    tmp = self.bg_save.as_ref().map(|b| b.tmp_path());
                    cancelled = true;
                    done = true;
                }
            }
        }

        self.bg_save = None;
        self.save_progress = None;
        self.save_is_copy = false;

        let (msg, ok) = if cancelled {
            if let Some(t) = &tmp {
                let _ = std::fs::remove_file(t);
            }
            (Some("保存已取消".to_string()), false)
        } else if let Some(e) = failed {
            if let Some(t) = &tmp {
                let _ = std::fs::remove_file(t);
            }
            (Some(format!("保存失败: {}", e)), false)
        } else if let Some(t) = tmp {
            if is_copy {
                // 另存为 wrote its own temp+rename; nothing to finalize and the
                // working file's edits stay intact.
                let dst = self.save_as_path.clone().unwrap_or_else(|| self.path.clone());
                (
                    Some(format!("已另存为 → {} ({})", dst.display(), elapsed_str)),
                    true,
                )
            } else {
                match self.finalize_saved_file(&t) {
                    Some(e) => (Some(e), false),
                    None => (
                        Some(format!("保存完成 → {} ({})", self.path.display(), elapsed_str)),
                        true,
                    ),
                }
            }
        } else {
            (None, false)
        };
        (done, msg, ok)
    }

    /// Swap the saved temp file over the original and rebuild the index in the
    /// background. Returns `None` on success or an error message.
    fn finalize_saved_file(&mut self, tmp: &Path) -> Option<String> {
        let path = self.path.clone();
        let result: Result<()> = (|| {
            let mut tmp_mmap = MmapBackend::open(tmp)?;
            std::mem::swap(&mut self.mmap, &mut tmp_mmap);
            drop(tmp_mmap);
            std::fs::rename(tmp, &path)?;
            self.mmap = MmapBackend::open(&path)?;
            let size = self.mmap.size();
            self.known_size = size;
            self.original_size = size;
            self.reset_line_cache();
            self.edits.clear();
            self.cache.invalidate_raw();
            self.cache.invalidate_display();
            self.submit_build_index();
            Ok(())
        })();
        match result {
            Ok(()) => None,
            Err(e) => {
                let _ = std::fs::remove_file(tmp);
                Some(format!("保存失败: {}", e))
            }
        }
    }

    // ------------------------------------------------------------------
    // Edit operations
    // ------------------------------------------------------------------

    pub fn delete_logical_line(&mut self, line_no: u64) -> bool {
        self.edits.clear_redo();
        let (phys, blk) = match self.edits.mapping.resolve(
            &self.edits.inserted,
            line_no,
            self.index.line_count(),
        ) {
            Some(t) => t,
            None => return false,
        };
        let phys = match phys {
            Some(p) => p,
            None => {
                let (anchor, idx) = match blk {
                    Some((a, i)) => (a, i),
                    None => return false,
                };
                // Delete a line INSIDE an inserted block: remove the block entry
                // (undoable via DeleteBlock) and yank its bytes.
                let entry = match self.edits.inserted.get_mut(&anchor) {
                    Some(e) => e,
                    None => return false,
                };
                if idx >= entry.len() {
                    return false;
                }
                let bytes = entry.remove(idx);
                if entry.is_empty() {
                    self.edits.inserted.remove(&anchor);
                }
                self.edits.push_undo(EditOp::DeleteBlock {
                    anchor,
                    index: idx,
                    bytes: bytes.clone(),
                });
                self.edits.dirty = true;
                self.edits.yank_line(bytes);
                self.edits.rebuild_mapping();
                self.cache.invalidate_raw();
                return true;
            }
        };
        let mut editor = LineEditor::new(&self.mmap, &self.index, &mut self.edits);
        let bytes = match editor.delete_line(phys) {
            Some(b) => b,
            None => return false,
        };
        self.edits.yank_line(bytes);
        self.cache.invalidate_raw();
        true
    }

    pub fn yank_logical_line(&mut self, line_no: u64) -> bool {
        let bytes = match self.read_line(line_no).text.into_bytes() {
            b if !b.is_empty() => b,
            _ => return false,
        };
        self.edits.yank_line(bytes);
        true
    }

    pub fn paste_after(&mut self, line_no: u64) -> bool {
        self.edits.clear_redo();
        let yanked = match self.edits.take_yank() {
            Some(v) => v,
            None => return false,
        };
        let phys = match self.edits.mapping.logical_to_physical(
            &self.edits.inserted,
            line_no,
            self.index.line_count(),
        ) {
            Some(p) => p,
            None => u64::MAX,
        };
        let mut editor = LineEditor::new(&self.mmap, &self.index, &mut self.edits);
        editor.insert_lines(phys, yanked);
        self.cache.invalidate_raw();
        true
    }

    pub fn undo_one(&mut self) -> bool {
        if self.edits.undo_count() == 0 {
            return false;
        }
        let mut editor = LineEditor::new(&self.mmap, &self.index, &mut self.edits);
        let ok = editor.undo();
        self.cache.invalidate_raw();
        ok
    }

    pub fn redo_one(&mut self) -> bool {
        if self.edits.redo_stack.is_empty() {
            return false;
        }
        let mut editor = LineEditor::new(&self.mmap, &self.index, &mut self.edits);
        let ok = editor.redo();
        self.cache.invalidate_raw();
        ok
    }

    /// Start recording an atomic edit batch — ops applied until `end_edit_batch`
    /// become ONE undo/redo step (used by the GUI for split/join/paste/selection).
    pub fn begin_edit_batch(&mut self) {
        self.edits.begin_batch();
    }

    /// Close the batch started by [`Engine::begin_edit_batch`].
    pub fn end_edit_batch(&mut self) {
        self.edits.end_batch();
    }

    /// Insert `bytes` as a NEW logical line right after `line_no`. Handles both
    /// original physical lines and already-inserted lines (mid-block).
    pub fn insert_logical_line_after(&mut self, line_no: u64, bytes: Vec<u8>) -> bool {
        self.edits.clear_redo();
        let (phys, blk) = match self.edits.mapping.resolve(
            &self.edits.inserted,
            line_no,
            self.index.line_count(),
        ) {
            Some(t) => t,
            None => return false,
        };
        let mut editor = LineEditor::new(&self.mmap, &self.index, &mut self.edits);
        match blk {
            Some((anchor, idx)) => editor.insert_line_in_block(anchor, idx, bytes),
            None => editor.insert_lines(phys.unwrap_or(u64::MAX), vec![bytes]),
        }
        self.cache.invalidate_raw();
        true
    }

    pub fn replace_logical_line(&mut self, line_no: u64, new_bytes: Vec<u8>) -> bool {
        self.edits.clear_redo();
        let (phys, blk) = match self.edits.mapping.resolve(
            &self.edits.inserted,
            line_no,
            self.index.line_count(),
        ) {
            Some(t) => t,
            None => return false,
        };
        // Line inside an inserted block (created by Enter / paste): update the
        // block entry directly, undoable via ReplaceBlock.
        if let Some((anchor, idx)) = blk {
            let old = match self.edits.inserted.get(&anchor).and_then(|v| v.get(idx)) {
                Some(b) => b.clone(),
                None => return false,
            };
            if old != new_bytes {
                if let Some(entry) = self.edits.inserted.get_mut(&anchor) {
                    entry[idx] = new_bytes.clone();
                }
                self.edits.push_undo(EditOp::ReplaceBlock {
                    anchor,
                    index: idx,
                    old,
                    new: new_bytes,
                });
                self.edits.dirty = true;
            }
            self.cache.invalidate_raw();
            return true;
        }
        let phys = match phys {
            Some(p) => p,
            None => return false,
        };
        let mut editor = LineEditor::new(&self.mmap, &self.index, &mut self.edits);
        let ok = editor.replace_line(phys, new_bytes).is_some();
        self.cache.invalidate_raw();
        ok
    }

    pub fn delete_logical_line_and_return(&mut self, line_no: u64) -> Option<Vec<u8>> {
        self.edits.clear_redo();
        let (phys, blk) = match self.edits.mapping.resolve(
            &self.edits.inserted,
            line_no,
            self.index.line_count(),
        ) {
            Some(t) => t,
            None => return None,
        };
        let bytes = if let Some((anchor, idx)) = blk {
            let entry = self.edits.inserted.get_mut(&anchor)?;
            if idx >= entry.len() {
                return None;
            }
            let bytes = entry.remove(idx);
            if self.edits.inserted.get(&anchor).map_or(true, |e| e.is_empty()) {
                self.edits.inserted.remove(&anchor);
            }
            self.edits.push_undo(EditOp::DeleteBlock {
                anchor,
                index: idx,
                bytes: bytes.clone(),
            });
            self.edits.dirty = true;
            self.edits.rebuild_mapping();
            bytes
        } else {
            let phys = phys?;
            let mut editor = LineEditor::new(&self.mmap, &self.index, &mut self.edits);
            editor.delete_line(phys)?
        };
        self.edits.yank_line(bytes.clone());
        self.cache.invalidate_raw();
        Some(bytes)
    }

    // ------------------------------------------------------------------
    // Search
    // ------------------------------------------------------------------

    pub fn submit_search(&mut self, query: String, opts: SearchOptions) -> Result<()> {
        if query.is_empty() {
            self.search.clear();
            self.search_query.clear();
            self.search_hash = 0;
            self.search_progress = None;
            self.bg_search = None;
            self.cache.invalidate_display();
            return Ok(());
        }
        self.search_query = query.clone();
        self.search_hash = xxhash_query(&query);
        self.cache.invalidate_display();
        // Drop any prior BlockIndex immediately. The worker thread holds no
        // reference yet, so this lets the previous samples Arc deallocate as
        // soon as the worker spins up — preventing the brief memory spike
        // during overlapping searches.
        self.search.clear();
        self.bg_search = None;

        let mmap = self.mmap.clone();
        self.bg_search = Some(BackgroundSearch::spawn(
            mmap,
            query.clone(),
            opts,
            self.scan_window,
            self.search_config.sample_interval,
            self.search_config.max_samples,
        ));
        self.search_progress = Some("searching...".to_string());
        Ok(())
    }

    /// Spawn an **independent** background search whose lifecycle and results
    /// are owned by the caller (agent / tool searches).
    ///
    /// Unlike [`Self::submit_search`], this never touches the interactive
    /// search slot (`bg_search` / `search` / `search_query` / `search_hash` /
    /// `search_progress`) nor the display cache, so it can't disturb the UI's
    /// search state, and concurrent calls are fully isolated — each scans the
    /// file with its own worker and returns its own `BlockIndex`. Callers
    /// should still serialize concurrent searches (the file is read once per
    /// search; parallel scans of a huge file just thrash the disk).
    pub fn spawn_search(&self, query: String, opts: SearchOptions) -> Result<BackgroundSearch> {
        if query.is_empty() {
            anyhow::bail!("empty query");
        }
        Ok(BackgroundSearch::spawn(
            self.mmap.clone(),
            query,
            opts,
            self.scan_window,
            self.search_config.sample_interval,
            self.search_config.max_samples,
        ))
    }

    pub fn poll_bg_search(&mut self) -> (bool, Option<String>) {
        let mut done = false;
        let mut result_msg: Option<String> = None;
        let mut messages: Vec<crate::search::SearchProgress> = Vec::new();
        let mut elapsed = std::time::Duration::ZERO;
        if let Some(ref bg) = self.bg_search {
            while let Some(p) = bg.poll() {
                messages.push(p);
            }
            elapsed = bg.elapsed();
        }
        for p in messages {
            match p {
                crate::search::SearchProgress::Started(q) => {
                    // Ignore progress after a terminal state (same race as the
                    // indexer — the poller can emit a ping after Done).
                    if done {
                        continue;
                    }
                    self.search_progress = Some(format!("searching '{}'...", q));
                }
                crate::search::SearchProgress::Percent(pct) => {
                    if done {
                        continue;
                    }
                    self.search_progress = Some(format!("searching... {}%", pct));
                }
                crate::search::SearchProgress::Done(index) => {
                    let n = index.total_count();
                    let elapsed_str = if elapsed.as_secs() >= 1 {
                        format!("{:.2}s", elapsed.as_secs_f64())
                    } else {
                        format!("{}ms", elapsed.as_millis())
                    };
                    self.search.set_results(index, self.search_query.clone());
                    self.search_progress = None;
                    result_msg = Some(format!(
                        "{} hits in {}", n, elapsed_str
                    ));
                    done = true;
                }
                crate::search::SearchProgress::Failed(e) => {
                    self.search_progress = None;
                    result_msg = Some(format!("search failed: {}", e));
                    done = true;
                }
                crate::search::SearchProgress::Cancelled => {
                    self.search_progress = None;
                    result_msg = Some("search cancelled".to_string());
                    done = true;
                }
            }
        }
        if done {
            self.bg_search = None;
        }
        (done, result_msg)
    }

    /// Cancel a running background search. The worker will stop at the next
    /// check-point and partial results are discarded.
    pub fn cancel_search(&mut self) {
        if let Some(ref bg) = self.bg_search {
            bg.cancel();
        }
    }

    /// Whether the file predominantly uses CRLF (`\r\n`) line endings.
    /// Samples the first 64 KiB — a cheap one-time scan used to normalise
    /// multi-line literal search queries to the file's actual line ending.
    pub fn uses_crlf(&self) -> bool {
        let size = self.mmap.size();
        if size == 0 {
            return false;
        }
        let slice = self.mmap.slice(0, size.min(65536) as usize);
        let mut crlf = 0u32;
        let mut lf = 0u32;
        for i in memchr::memchr_iter(b'\n', slice) {
            lf += 1;
            if i > 0 && slice[i - 1] == b'\r' {
                crlf += 1;
            }
        }
        lf > 0 && crlf >= lf / 2
    }

    pub fn substitute_current(&mut self, line_no: u64, pat: &str, repl: &str, global: bool) -> Option<String> {
        let raw = self.read_line(line_no).text.into_bytes();
        if raw.is_empty() {
            return Some("empty line".to_string());
        }
        let new = if global {
            if raw.windows(pat.len()).any(|w| w == pat.as_bytes()) {
                let mut s = raw.clone();
                while let Some(pos) = s.windows(pat.len()).position(|w| w == pat.as_bytes()) {
                    let mut after = s.split_off(pos + pat.len());
                    s.truncate(pos);
                    s.extend_from_slice(repl.as_bytes());
                    s.append(&mut after);
                }
                Some(s)
            } else {
                None
            }
        } else {
            raw.windows(pat.len())
                .position(|w| w == pat.as_bytes())
                .map(|pos| {
                    let mut s = raw.clone();
                    let mut tail = s.split_off(pos + pat.len());
                    s.truncate(pos);
                    s.extend_from_slice(repl.as_bytes());
                    s.append(&mut tail);
                    s
                })
        };
        match new {
            Some(bytes) => {
                if bytes == raw {
                    Some("pattern not found".to_string())
                } else {
                    self.replace_logical_line(line_no, bytes);
                    Some("substituted".to_string())
                }
            }
            None => Some("pattern not found".to_string()),
        }
    }
}

fn xxhash_query(s: &str) -> u64 {
    xxhash_rust::xxh3::xxh3_64(s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `.qli` 缓存键必须稳定：`\?\` 扩展路径前缀不能改变 xxhash，
    /// 否则 GUI（明文路径）与 Agent（canonicalize 后带前缀）会对同一文件
    /// 算出两个缓存键 → Agent 侧错过缓存、整份大文件重建索引。
    #[cfg(windows)]
    #[test]
    fn normalize_cache_path_strips_extended_prefix() {
        assert_eq!(
            normalize_cache_path(PathBuf::from(r"\\?\D:\data\test_xxl.log")),
            PathBuf::from(r"D:\data\test_xxl.log")
        );
        assert_eq!(
            normalize_cache_path(PathBuf::from(r"\\?\UNC\server\share\a.log")),
            PathBuf::from(r"\\server\share\a.log")
        );
        // 无前缀路径原样保留
        let plain = PathBuf::from(r"D:\data\test_xxl.log");
        assert_eq!(normalize_cache_path(plain.clone()), plain);
    }

    /// 与 `with_config` 相同的组合方式（先 `normalize_cache_path` 再取缓存路径）：
    /// 带前缀与不带前缀的路径必须命中同一个 `.qli` 键。
    #[cfg(windows)]
    #[test]
    fn cache_key_stable_across_path_styles() {
        let cfg = EngineConfig {
            index_dir: Some(PathBuf::from(r"D:\idx")),
            index_cache_enabled: true,
            ..EngineConfig::default()
        };
        let prefixed = normalize_cache_path(PathBuf::from(r"\\?\D:\data\test_xxl.log"));
        let plain = normalize_cache_path(PathBuf::from(r"D:\data\test_xxl.log"));
        assert_eq!(
            cfg.index_path(&prefixed),
            cfg.index_path(&plain),
            "归一化后两种路径应命中同一 .qli 缓存"
        );
    }
}
