//! 把 qview_core::Engine 包装为本前端用的 Bridge（镜像 gui/native 的
//! engine_bridge.rs），并额外提供基于 `parse_query` 的精确行内高亮。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use qview_core::config::EngineConfig;
use qview_core::engine::Engine;
use qview_core::search::{parse_query, SearchOptions};

/// 一行文本 + 行内匹配区间（UTF-8 字节区间，与 qview-core 的 hits 一致）
pub struct DisplayLine {
    pub text: String,
    /// 行内匹配的 UTF-8 字节区间
    pub matches: Vec<(usize, usize)>,
    /// 该行是否包含“当前命中”光标所在的命中
    pub is_current: bool,
}

pub struct Bridge {
    pub path: PathBuf,
    pub size: u64,
    pub line_count: u64,
    pub engine: Arc<Mutex<Engine>>,
    pub last_query: String,
    pub hits: Vec<u64>,
    pub cursor: usize,
    pub bg_indexing: bool,
}

impl Bridge {
    pub fn open(path: &Path, cfg: &EngineConfig) -> Result<Self> {
        let mut engine = Engine::with_config(path.to_path_buf(), cfg.clone())?;
        let size = engine.mmap.size();

        if engine.index.is_complete() {
            eprintln!("[engine] 索引缓存命中: {}", path.display());
        }

        // 大于小文件阈值且索引未就绪 → 后台建索引
        let bg_indexing = size > cfg.small_file_threshold && !engine.index.is_complete();
        if bg_indexing {
            engine.submit_build_index();
        }

        let line_count = engine.effective_line_count();
        Ok(Self {
            path: path.to_path_buf(),
            size,
            line_count,
            engine: Arc::new(Mutex::new(engine)),
            last_query: String::new(),
            hits: Vec::new(),
            cursor: 0,
            bg_indexing,
        })
    }

    /// Whether the open file predominantly uses CRLF (`\r\n`) line endings.
    pub fn uses_crlf(&self) -> bool {
        self.engine.lock().map_or(false, |e| e.uses_crlf())
    }

    pub fn total_lines(&self) -> u64 {
        self.line_count
    }

    /// 读取第 n 行的原始文本。
    pub fn read_line(&self, n: u64) -> String {
        let engine = self.engine.lock().unwrap();
        engine.read_line(n).text
    }

    /// 读取某行并自算行内匹配（比 egui 端更准确：直接用 parse_query）。
    pub fn read_display_line(
        &self,
        n: u64,
        query: &str,
        opts: &SearchOptions,
        hit_byte: Option<u64>,
    ) -> DisplayLine {
        let engine = self.engine.lock().unwrap();
        let raw = engine.read_line(n);
        let text = raw.text;
        let line_start = raw.start_byte;

        let mut matches: Vec<(usize, usize)> = Vec::new();
        let mut is_current = false;
        if !query.is_empty() {
            if let Ok(q) = parse_query(query, opts) {
                let cb = text.as_bytes();
                match &q {
                    qview_core::search::Query::Literal(p) => {
                        if !p.is_empty() {
                            for m in memchr::memmem::find_iter(cb, p) {
                                let end = (m + p.len()).min(cb.len());
                                if end > m {
                                    matches.push((m, end));
                                }
                                if let Some(hb) = hit_byte {
                                    let abs = line_start + m as u64;
                                    if hb >= abs && hb < abs + p.len() as u64 {
                                        is_current = true;
                                    }
                                }
                            }
                        }
                    }
                    qview_core::search::Query::Regex(re) => {
                        for m in re.find_iter(cb) {
                            let s = m.start();
                            let e = m.end();
                            if e > s {
                                matches.push((s, e));
                            }
                            if let Some(hb) = hit_byte {
                                let abs = line_start + s as u64;
                                if hb >= abs && hb < abs + (e - s) as u64 {
                                    is_current = true;
                                }
                            }
                        }
                    }
                }
            }
        }

        DisplayLine {
            text,
            matches,
            is_current,
        }
    }

    pub fn submit_search(&mut self, q: String, opts: SearchOptions) -> Result<()> {
        let mut engine = self.engine.lock().unwrap();
        engine.submit_search(q.clone(), opts)?;
        self.last_query = q;
        self.cursor = 0;
        Ok(())
    }

    /// 轮询后台搜索。返回 true 表示搜索已完成（hits 已更新）。
    pub fn poll_search(&mut self) -> bool {
        let mut engine = self.engine.lock().unwrap();
        let (done, _msg) = engine.poll_bg_search();
        if done {
            self.hits = engine.search.snapshot_hits();
            self.cursor = 0;
        }
        done
    }

    /// 轮询后台索引进度。返回 true 表示索引已完成。
    pub fn poll_index(&mut self) -> bool {
        let mut engine = self.engine.lock().unwrap();
        let (done, _msg) = engine.poll_bg_index();
        if done {
            self.line_count = engine.effective_line_count();
            self.bg_indexing = false;
        }
        done
    }

    /// 将命中索引转为行号（供视图跳转）
    pub fn hit_line(&self, idx: usize) -> Option<u64> {
        if idx >= self.hits.len() {
            return None;
        }
        let byte = self.hits[idx];
        let engine = self.engine.lock().unwrap();
        Some(engine.index.line_of_byte(byte))
    }

    pub fn indexing_active(&self) -> bool {
        self.bg_indexing
    }

    /// 当前后台任务进度消息（索引进度优先），用于状态栏确定/不确定进度条。
    pub fn progress_message(&self) -> Option<String> {
        let engine = self.engine.lock().unwrap();
        engine
            .index_progress
            .clone()
            .or_else(|| engine.search_progress.clone())
    }
}

// ---------------------------------------------------------------------------
// 集成测试：验证 Bridge 的打开 / 搜索 / 行内匹配（不依赖 GUI，可在 CI 跑）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const LOG: &str = "2024-01-01 INFO starting app\n\
                       2024-01-01 DEBUG some debug info\n\
                       2024-01-01 ERROR something went wrong\n\
                       2024-01-01 ERROR another error here\n\
                       2024-01-01 INFO finishing\n";

    fn write_temp_log() -> PathBuf {
        let path = std::env::temp_dir().join(format!("qlog_bridge_test_{}.log", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(LOG.as_bytes()).unwrap();
        path
    }

    fn opts(case: bool, regex: bool, whole: bool) -> SearchOptions {
        SearchOptions {
            case_sensitive: case,
            use_regex: regex,
            whole_word: whole,
            crlf: false,
        }
    }

    #[test]
    fn open_read_and_line_count() {
        let p = write_temp_log();
        let b = Bridge::open(&p, &EngineConfig::default()).unwrap();
        assert_eq!(b.total_lines(), 5, "small file index must be synchronous");
        assert_eq!(b.read_line(0), "2024-01-01 INFO starting app");
        assert_eq!(b.read_line(4), "2024-01-01 INFO finishing");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn case_sensitive_search_maps_hits_to_lines() {
        let p = write_temp_log();
        let mut b = Bridge::open(&p, &EngineConfig::default()).unwrap();
        b.submit_search("ERROR".into(), opts(true, false, false)).unwrap();
        // 后台线程搜索：轮询直到完成
        for _ in 0..100 {
            b.poll_search();
            if b.hits.len() >= 2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(b.hits.len(), 2, "case-sensitive 'ERROR' → 2 hits");
        assert_eq!(b.hit_line(0), Some(2), "first hit on line 3 (0-based 2)");
        assert_eq!(b.hit_line(1), Some(3), "second hit on line 4 (0-based 3)");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_display_line_highlights_matches() {
        let p = write_temp_log();
        let mut b = Bridge::open(&p, &EngineConfig::default()).unwrap();
        b.submit_search("ERROR".into(), opts(true, false, false)).unwrap();
        for _ in 0..100 {
            b.poll_search();
            if !b.hits.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!b.hits.is_empty());

        let hit = b.hits[0];
        let line = b.hit_line(0).unwrap();
        let dl = b.read_display_line(line, "ERROR", &opts(true, false, false), Some(hit));
        assert_eq!(dl.matches.len(), 1, "line 3 has exactly one 'ERROR'");
        let (s, e) = dl.matches[0];
        assert_eq!(&dl.text[s..e], "ERROR");
        assert!(dl.is_current, "the hit at cursor must mark line as current");

        // 非命中行：无高亮
        let dl2 = b.read_display_line(0, "ERROR", &opts(true, false, false), None);
        assert!(dl2.matches.is_empty());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn regex_search_highlights_regex_matches() {
        let p = write_temp_log();
        let mut b = Bridge::open(&p, &EngineConfig::default()).unwrap();
        b.submit_search("err\\w+".into(), opts(false, true, false)).unwrap();
        for _ in 0..100 {
            b.poll_search();
            if !b.hits.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(b.hits.len() >= 3, "case-insensitive regex 'err\\w+' hits many");
        // 第 4 行（0-based 3）含 "ERROR" 和 "error" 两个匹配
        let dl = b.read_display_line(3, "err\\w+", &opts(false, true, false), None);
        assert_eq!(dl.matches.len(), 2, "line 4 has 'ERROR' + 'error'");
        let _ = std::fs::remove_file(&p);
    }
}
