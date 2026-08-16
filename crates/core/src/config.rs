//! Engine-level configuration.
//!
//! `EngineConfig` controls how the engine opens and indexes files.
//! It lives in `qview-core` so all frontends (GUI, TUI, future) can
//! share the same configuration model.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Configuration for the log-viewer engine.
///
/// The GUI reads this from its own config file and passes it to
/// `Engine::with_config()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    /// Files smaller than this many bytes are indexed **synchronously in
    /// memory** on open — no background thread, no disk cache.
    /// Larger files use the background indexer and may be cached to disk
    /// (see [`index_cache_enabled`]).
    #[serde(default = "default_small_file_threshold")]
    pub small_file_threshold: u64,

    /// When `true` AND [`index_dir`] is `Some`, large-file line indices are
    /// persisted to disk so subsequent opens are instant.
    ///
    /// Small files (≤ [`small_file_threshold`]) are **never** cached to disk.
    #[serde(default = "default_true")]
    pub index_cache_enabled: bool,

    /// Directory for persistent `.qli` index files.
    ///
    /// * `None` — legacy mode: `.qli` files are stored next to the log file.
    /// * `Some(dir)` — all `.qli` files go into `dir/`, named by xxhash of
    ///   the log file's absolute path (no collisions, no littering).
    ///
    /// The GUI defaults to `{app_data}/qview-gui/index/`.
    #[serde(default)]
    pub index_dir: Option<PathBuf>,

    /// Maximum number of raw-line reads to keep in the LRU cache.
    #[serde(default = "default_line_cache_capacity")]
    pub line_cache_capacity: usize,

    /// Text encoding label, e.g. "UTF-8", "GBK", "Shift_JIS".
    /// Resolved to an [`encoding_rs::Encoding`] at engine creation time.
    #[serde(default = "default_encoding")]
    pub encoding: String,

    /// How to build the large-file line index.
    ///
    /// * `Sparse` (default) — inline sampling: never materialises the full
    ///   offsets array (800 MB for 100M lines), lowest peak memory.  The file
    ///   is still read from disk once, but each streamed window is scanned
    ///   twice in RAM (count, then sample), so it can be slower on cold-cache
    ///   first opens.
    /// * `Full` — legacy single-pass build that materialises every line start
    ///   then downsamples.  Uses more peak memory (~8 bytes/line) but only
    ///   scans the file once.
    #[serde(default = "default_index_build_mode")]
    pub index_build_mode: IndexBuildMode,

    /// Number of worker threads used for index building and search.
    ///
    /// * `0` (default) — auto: `available_parallelism − 1`, leaving one core
    ///   free for the UI thread so huge-file scans don't freeze the window.
    /// * `≥ 1` — force an exact thread count (capped at the CPU core count).
    ///
    /// Read once when the shared scan pool is first built (engine startup);
    /// changing it on a live process requires a restart.
    #[serde(default = "default_scan_threads")]
    pub scan_threads: u32,

    /// Streaming scan window, in MiB. Index builds and searches read the file
    /// in windows of this size (two are buffered at once; Windows opens the
    /// scan handle with `FILE_FLAG_NO_BUFFERING`, so this does NOT grow the
    /// system file cache — only ~2× the window is resident).
    ///
    /// Larger window → fewer boundary handoffs (slightly faster on huge
    /// files) but ~2× window bytes of extra process memory. Default 64 MiB.
    #[serde(default = "default_scan_window_mb")]
    pub scan_window_mb: u32,

    /// Search / hit-navigation tuning. Controls memory vs navigation-speed
    /// tradeoff for the BlockIndex.
    #[serde(default)]
    pub search: SearchConfig,
}

/// How the large-file line index is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexBuildMode {
    /// New default: inline sparse sampling — low memory, two passes.
    #[default]
    Sparse,
    /// Legacy: full offsets array then downsample — one pass, more memory.
    Full,
}

fn default_index_build_mode() -> IndexBuildMode {
    IndexBuildMode::Sparse
}

fn default_scan_threads() -> u32 {
    0 // auto: leave one core for the UI
}

fn default_scan_window_mb() -> u32 {
    64
}

/// Search-specific tuning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    /// Store one sample every N hits. Total hit count is always exact;
    /// `get(n)` rescans at most this many matches to locate any hit.
    ///
    /// Memory: samples ≈ total_hits / interval × 8 bytes.
    /// Lookup cost: O(interval) bytes scanned.
    ///
    /// Sensible values: 50 (faster navigation, more memory) … 1000+
    /// (slower navigation, less memory). Default 100 keeps memory around
    /// 32MB for 402M hits while staying sub-millisecond per navigation.
    #[serde(default = "default_search_sample_interval")]
    pub sample_interval: u32,

    /// Hard cap on the number of samples kept. Bounds memory regardless
    /// of `sample_interval`. With default interval=100 this allows up to
    /// 10M samples = 80MB, supporting up to 1 billion hits. Beyond that,
    /// navigation past the sampled range falls back to full rescans.
    #[serde(default = "default_search_max_samples")]
    pub max_samples: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            sample_interval: default_search_sample_interval(),
            max_samples: default_search_max_samples(),
        }
    }
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            small_file_threshold: default_small_file_threshold(),
            index_cache_enabled: true,
            index_dir: None,
            line_cache_capacity: default_line_cache_capacity(),
            encoding: default_encoding(),
            index_build_mode: default_index_build_mode(),
            scan_threads: default_scan_threads(),
            scan_window_mb: default_scan_window_mb(),
            search: SearchConfig::default(),
        }
    }
}

impl EngineConfig {
    /// Compute the `.qli` path for a given log file.
    ///
    /// * If `index_dir` is `Some(dir)`, the filename is
    ///   `{xxhash64_of_absolute_path:016x}.qli` inside `dir`.
    /// * If `index_dir` is `None`, falls back to `{log_path}.qli` (legacy).
    pub fn index_path(&self, log_path: &Path) -> PathBuf {
        match &self.index_dir {
            Some(dir) => {
                let hash =
                    xxhash_rust::xxh3::xxh3_64(log_path.to_string_lossy().as_bytes());
                dir.join(format!("{:016x}.qli", hash))
            }
            None => log_path.with_extension("qli"),
        }
    }

    /// The index path to use for caching, if caching is enabled.
    /// Returns `None` for small files or when caching is disabled.
    pub fn cache_path(&self, log_path: &Path) -> Option<PathBuf> {
        if self.index_cache_enabled {
            Some(self.index_path(log_path))
        } else {
            None
        }
    }
}

// ---- serde default helpers ----

fn default_small_file_threshold() -> u64 {
    10 * 1024 * 1024 // 10 MiB
}

fn default_true() -> bool {
    true
}

fn default_line_cache_capacity() -> usize {
    10_000
}

fn default_encoding() -> String {
    "UTF-8".into()
}

fn default_search_sample_interval() -> u32 {
    100
}

fn default_search_max_samples() -> usize {
    10_000_000 // 80MB at 8 bytes per sample
}

/// Resolve an encoding label (e.g. "GBK", "Shift_JIS") to an
/// [`encoding_rs::Encoding`] reference. Falls back to UTF-8 for unknown labels.
pub fn resolve_encoding(label: &str) -> &'static encoding_rs::Encoding {
    if label.eq_ignore_ascii_case("utf-8") || label.eq_ignore_ascii_case("utf8") {
        return encoding_rs::UTF_8;
    }
    encoding_rs::Encoding::for_label_no_replacement(label.as_bytes())
        .unwrap_or(encoding_rs::UTF_8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_threads_defaults_to_zero_and_roundtrips() {
        // Default = 0 (auto). This is what a missing config field deserializes to.
        assert_eq!(EngineConfig::default().scan_threads, 0);

        // Explicit values survive a serde round-trip, including `0`.
        for n in [0u32, 1, 4, 16] {
            let json = serde_json::to_string(&EngineConfig {
                scan_threads: n,
                ..EngineConfig::default()
            })
            .unwrap();
            let back: EngineConfig = serde_json::from_str(&json).unwrap();
            assert_eq!(back.scan_threads, n, "scan_threads round-trip for {n}");
        }

        // Missing field → 0 (auto), so old configs keep auto behaviour.
        let cfg: EngineConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.scan_threads, 0);
    }

    #[test]
    fn scan_window_defaults_to_64_and_roundtrips() {
        // Default = 64 MiB; missing field in old configs must not change behaviour.
        assert_eq!(EngineConfig::default().scan_window_mb, 64);
        let cfg: EngineConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.scan_window_mb, 64);

        // Explicit values survive a round-trip.
        for mb in [16u32, 32, 64, 128, 256] {
            let json = serde_json::to_string(&EngineConfig {
                scan_window_mb: mb,
                ..EngineConfig::default()
            })
            .unwrap();
            let back: EngineConfig = serde_json::from_str(&json).unwrap();
            assert_eq!(back.scan_window_mb, mb, "scan_window_mb round-trip for {mb}");
        }
    }
}
