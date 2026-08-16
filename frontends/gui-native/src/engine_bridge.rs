//! 把 qview_core::Engine 包装为本前端用的 Bridge。
//!
//! 搜索导航**精确委托**给 `engine.search`（SearchResults 走 BlockIndex，任意
//! 第 n 个命中都精确），不再用 `snapshot_hits()` 复制采样偏移 —— 消除大结果
//! 集的巨额分配，总数恒精确。

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use qview_core::config::EngineConfig;
use qview_core::engine::Engine;
use qview_core::search::{Match, SearchOptions};

/// 顶层应用持有的搜索 UI 状态（导航状态本身在 engine.search 上）
#[derive(Default)]
pub struct SearchState {
    pub query: String,
    pub case_sensitive: bool,
    pub use_regex: bool,
    pub whole_word: bool,
    /// 命中总数（engine.search.len()，精确）
    pub total: usize,
    /// 当前命中序号（0 基）
    pub cursor: usize,
    /// 后台搜索进行中（状态栏进度条）
    pub searching: bool,
    /// 搜索状态文本（"N/M 条匹配"）
    pub status: String,
}

impl SearchState {
    pub fn cancel(&mut self) {
        self.query.clear();
        self.total = 0;
        self.cursor = 0;
        self.searching = false;
        self.status.clear();
    }
}

pub struct Bridge {
    pub path: std::path::PathBuf,
    pub size: u64,
    pub line_count: u64,
    pub engine: Arc<Mutex<Engine>>,
    pub bg_indexing: std::sync::atomic::AtomicBool,
}

impl Bridge {
    pub fn open(path: &Path, cfg: &EngineConfig) -> Result<Self> {
        let mut engine = Engine::with_config(path.to_path_buf(), cfg.clone())?;
        let size = engine.mmap.size();

        let cache_hit = engine.index.is_complete();
        if cache_hit {
            eprintln!("[engine] 索引缓存命中: {}", path.display());
        }

        // 仅当索引未完成时才后台建索引（避免重新索引已有缓存的文件）
        let threshold = cfg.small_file_threshold;
        let bg_indexing = if size > threshold && !engine.index.is_complete() {
            engine.submit_build_index();
            std::sync::atomic::AtomicBool::new(true)
        } else {
            std::sync::atomic::AtomicBool::new(false)
        };

        let line_count = engine.effective_line_count();
        Ok(Self {
            path: path.to_path_buf(),
            size,
            line_count,
            engine: Arc::new(Mutex::new(engine)),
            bg_indexing,
        })
    }

    pub fn read_line(&self, n: u64) -> String {
        let engine = self.engine.lock().unwrap();
        engine.read_line(n).text
    }

    /// 返回 RawLine（含 start_byte，批注定位用）
    pub fn read_raw(&self, n: u64) -> qview_core::cache::RawLine {
        let engine = self.engine.lock().unwrap();
        engine.read_line(n)
    }

    /// 行起始字节偏移（jump_hit 视口锚定用）
    pub fn line_start_byte(&self, n: u64) -> u64 {
        let engine = self.engine.lock().unwrap();
        engine.read_line(n).start_byte
    }

    /// Whether the open file predominantly uses CRLF (`\r\n`) line endings.
    pub fn uses_crlf(&self) -> bool {
        self.engine.lock().map_or(false, |e| e.uses_crlf())
    }

    /// 返回最近一次已知的行数（后台索引完成后更新）
    pub fn total_lines(&self) -> u64 {
        self.line_count
    }

    pub fn submit_search(&mut self, q: String, opts: SearchOptions) -> Result<()> {
        let mut engine = self.engine.lock().unwrap();
        engine.submit_search(q, opts)
    }

    /// 轮询后台搜索。返回 (done, 消息)。完成后 `search_len()` 为精确总数。
    pub fn poll_search(&mut self) -> (bool, Option<String>) {
        let mut engine = self.engine.lock().unwrap();
        engine.poll_bg_search()
    }

    /// 轮询后台索引进度。返回是否完成。
    pub fn poll_index(&mut self) -> bool {
        let mut engine = self.engine.lock().unwrap();
        let (done, _msg) = engine.poll_bg_index();
        if done {
            self.line_count = engine.effective_line_count();
            self.bg_indexing = std::sync::atomic::AtomicBool::new(false);
        }
        done
    }

    pub fn cancel_search(&self) {
        if let Ok(mut engine) = self.engine.lock() {
            engine.cancel_search();
        }
    }

    pub fn cancel_index(&self) {
        if let Ok(mut engine) = self.engine.lock() {
            engine.cancel_index();
        }
    }

    // ── 精确搜索导航（委托 engine.search）──

    pub fn search_len(&self) -> usize {
        self.engine.lock().map_or(0, |e| e.search.len())
    }
    pub fn search_cursor(&self) -> usize {
        self.engine.lock().map_or(0, |e| e.search.cursor())
    }
    pub fn search_current(&self) -> Option<Match> {
        self.engine.lock().ok().and_then(|e| e.search.current())
    }
    pub fn search_jump_by(&self, delta: i64) -> Option<Match> {
        self.engine.lock().ok().and_then(|e| e.search.jump_by(delta))
    }
    pub fn search_next(&self) -> Option<Match> {
        self.engine.lock().ok().and_then(|e| e.search.next())
    }
    pub fn search_prev(&self) -> Option<Match> {
        self.engine.lock().ok().and_then(|e| e.search.prev())
    }
    pub fn search_jump(&self, n: usize) -> Option<Match> {
        self.engine.lock().ok().and_then(|e| e.search.jump(n))
    }
    pub fn search_first(&self) -> Option<Match> {
        self.engine.lock().ok().and_then(|e| e.search.first())
    }
    pub fn search_last(&self) -> Option<Match> {
        self.engine.lock().ok().and_then(|e| e.search.last())
    }
    pub fn search_seek_to_byte(&self, byte: u64) -> bool {
        self.engine.lock().map_or(false, |e| e.search.seek_to_byte(byte))
    }

    /// 命中 → 行号（0 基）
    pub fn hit_line_of(&self, m: &Match) -> u64 {
        let engine = self.engine.lock().unwrap();
        engine.line_of_byte(m.byte)
    }

    pub fn indexing_active(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.bg_indexing.load(Ordering::Relaxed)
    }
}
