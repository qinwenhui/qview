//! Persistent configuration — read/write JSON config to the platform
//! app-data directory so user preferences survive restarts.
//!
//! The config file is split into three logical groups:
//! * `gui`    — display, theme, font, and search-option preferences
//! * `engine` — engine-level parameters (thresholds, cache, …)
//! * top-level — recent files and search history
//!
//! Old flat-format configs (≤ v2.0.0) are automatically migrated on first load.

use std::path::PathBuf;

use qview_core::config::EngineConfig;
use serde::{Deserialize, Serialize};

use crate::{log_info, log_warn};

// ---------------------------------------------------------------------------
// GuiConfig — all display / user-preference fields
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiConfig {
    #[serde(default = "default_theme")]
    pub theme: String,

    #[serde(default = "default_font_family")]
    pub font_family: String,

    #[serde(default = "default_font_size")]
    pub font_size: f32,

    #[serde(default = "default_row_height")]
    pub row_height: f64,

    #[serde(default = "default_true")]
    pub show_line_numbers: bool,

    #[serde(default)]
    pub word_wrap: bool,

    #[serde(default)]
    pub show_whitespace: bool,

    #[serde(default = "default_true")]
    pub level_coloring: bool,

    #[serde(default)]
    pub show_indent_guides: bool,

    #[serde(default)]
    pub case_sensitive: bool,

    #[serde(default)]
    pub use_regex: bool,

    #[serde(default)]
    pub whole_word: bool,

    #[serde(default = "default_window_size")]
    pub window_size: [f32; 2],

    #[serde(default)]
    pub window_maximized: bool,

    /// Files at or below this size may be edited and saved back IN PLACE.
    /// Larger files can still be edited but only "另存为" (the original is
    /// never overwritten). 0/disabled would mean no in-place saving at all.
    #[serde(default = "default_max_editable_bytes")]
    pub max_editable_bytes: u64,
}

impl Default for GuiConfig {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            font_family: default_font_family(),
            font_size: default_font_size(),
            row_height: default_row_height(),
            show_line_numbers: true,
            word_wrap: false,
            show_whitespace: false,
            level_coloring: true,
            show_indent_guides: false,
            case_sensitive: false,
            use_regex: false,
            whole_word: false,
            window_size: default_window_size(),
            window_maximized: false,
            max_editable_bytes: default_max_editable_bytes(),
        }
    }
}

fn default_max_editable_bytes() -> u64 {
    256 * 1024 * 1024 // 256 MiB
}

// ---------------------------------------------------------------------------
// AppConfig — top-level container
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_version")]
    pub version: String,

    #[serde(default)]
    pub gui: GuiConfig,

    #[serde(default)]
    pub engine: EngineConfig,

    /// 器灵 Agent 配置（UI 用自身的 JSON 配置文件承载 AgentConfig；
    /// 类比 engine 承载 EngineConfig）。缺省用 Mock provider（离线可跑）。
    #[serde(default)]
    pub agent: qview_agent::AgentConfig,

    /// 本地结构化存储配置（会话历史 / 文件元数据 / 搜索历史）。缺省 redb + `{config_dir}/qview.db`。
    #[serde(default)]
    pub store: StoreConfig,

    /// 记录 LLM 原始请求 / 响应到 `{config_dir}/llm_raw.log`（诊断用，可能含敏感
    /// 数据）。默认关；设置面板 → AI 可开关，实时生效（contexa-llm 每次调用读
    /// `QVIEW_LLM_RAW_LOG`，无需重启）。
    #[serde(default)]
    pub llm_raw_log: bool,

    /// 【已迁移到 store】遗留字段，仅用于启动时一次性迁移到 `files` 表后清空。
    /// 菜单「最近打开」不再读这里（读 `QLogApp.recent_files` 缓存，来源 = store）。
    #[serde(default)]
    pub recent_files: Vec<PathBuf>,

    /// 【已迁移到 store】遗留字段，仅用于启动时一次性迁移到 `search_history` 表后清空。
    #[serde(default)]
    pub search_history: Vec<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: default_version(),
            gui: GuiConfig::default(),
            engine: EngineConfig::default(),
            agent: qview_agent::AgentConfig::default(),
            store: StoreConfig::default(),
            llm_raw_log: false,
            recent_files: Vec::new(),
            search_history: Vec::new(),
        }
    }
}

/// 本地存储后端。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreBackend {
    Redb,
}

impl Default for StoreBackend {
    fn default() -> Self {
        StoreBackend::Redb
    }
}

/// 本地结构化存储配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreConfig {
    pub backend: StoreBackend,
    /// DB 文件路径；`None` → `{config_dir}/qview.db`。
    pub path: Option<PathBuf>,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            backend: StoreBackend::Redb,
            path: None,
        }
    }
}

// ---------------------------------------------------------------------------
// serde default helpers
// ---------------------------------------------------------------------------

fn default_version() -> String {
    env!("CARGO_PKG_VERSION").into()
}
fn default_theme() -> String {
    "Dark Pro".into()
}
fn default_font_family() -> String {
    "内置等宽".into()
}
fn default_font_size() -> f32 {
    13.0
}
fn default_row_height() -> f64 {
    18.0
}
fn default_true() -> bool {
    true
}
fn default_window_size() -> [f32; 2] {
    [1280.0, 860.0]
}

// ---------------------------------------------------------------------------
// IO helpers
// ---------------------------------------------------------------------------

impl AppConfig {
    /// 数据目录 = exe 同目录下的 data/
    pub fn config_dir() -> Option<PathBuf> {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        Some(exe_dir.join("data"))
    }

    fn config_path() -> Option<PathBuf> {
        Self::config_dir().map(|d| d.join("config.json"))
    }

    /// Load config from disk, falling back to defaults if the file is missing
    /// or corrupt.  Old flat-format configs (≤ v2.0.0) are automatically
    /// migrated to the grouped format.
    ///
    /// On first run, the engine's `index_dir` is set to
    /// `{config_dir}/qview-gui/index/`.
    pub fn load() -> Self {
        let path = match Self::config_path() {
            Some(p) => p,
            None => return Self::default(),
        };
        let json = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => {
                log_info!("config", "首次运行, 使用默认配置 (config_path={})", path.display());
                return Self::with_defaults();
            }
        };

        // Try new grouped format first.
        if let Ok(mut cfg) = serde_json::from_str::<Self>(&json) {
            let current_ver = default_version();
            if !cfg.gui.theme.is_empty()
                || cfg.version == "2.1.0"      // existed before version unification
                || cfg.version == current_ver
            {
                cfg.ensure_index_dir();
                log_info!("config", "加载配置: {} (theme={}, font_size={}, encoding={})",
                    path.display(), cfg.gui.theme, cfg.gui.font_size, cfg.engine.encoding);
                return cfg;
            }
        }

        // Try old flat format and migrate.
        if let Ok(old) = serde_json::from_str::<OldFlatConfig>(&json) {
            log_info!("config", "迁移旧版配置: {}", path.display());
            let mut migrated = old.migrate();
            migrated.ensure_index_dir();
            migrated.save();
            return migrated;
        }

        // Corrupt — keep a backup and start fresh.
        log_warn!("config", "配置损坏, 备份为 .bak 并使用默认配置");
        let _ = std::fs::rename(&path, path.with_extension("json.bak"));
        Self::with_defaults()
    }

    /// Return defaults with the GUI-appropriate index directory set.
    fn with_defaults() -> Self {
        let mut cfg = Self::default();
        cfg.ensure_index_dir();
        cfg
    }

    /// Set `engine.index_dir` to the default index directory if it is `None`.
    fn ensure_index_dir(&mut self) {
        if self.engine.index_dir.is_none() {
            if let Some(dir) = Self::config_dir() {
                self.engine.index_dir = Some(dir.join("index"));
            }
        }
    }

    /// Persist current config to disk. Creates the config directory if it
    /// doesn't exist yet.
    pub fn save(&self) {
        let path = match Self::config_path() {
            Some(p) => p,
            None => return,
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            if let Err(e) = std::fs::write(&path, json) {
                log_warn!("config", "保存配置失败: {}", e);
            }
        }
    }

    /// Record current window state.
    pub fn set_window_state(&mut self, size: [f32; 2], maximized: bool) {
        if !maximized {
            self.gui.window_size = size;
        }
        self.gui.window_maximized = maximized;
    }

    /// 解析本地存储 DB 路径：显式配置优先，否则 `{config_dir}/qview.db`。
    pub fn store_path(&self) -> Option<PathBuf> {
        if let Some(p) = &self.store.path {
            return Some(p.clone());
        }
        Self::config_dir().map(|d| d.join("qview.db"))
    }

    /// 按 `llm_raw_log` 配置应用 `QVIEW_LLM_RAW_LOG` 环境变量。contexa-llm 每次
    /// LLM 调用实时读取该变量，所以**开关即时生效，无需重启**：
    /// - 开 → 指向 `{config_dir}/llm_raw.log`
    /// - 关 → 移除变量（raw_log 静默，零开销）
    pub fn apply_llm_raw_log(&self) {
        if self.llm_raw_log {
            if let Some(dir) = Self::config_dir() {
                std::env::set_var("QVIEW_LLM_RAW_LOG", dir.join("llm_raw.log"));
            }
        } else {
            std::env::remove_var("QVIEW_LLM_RAW_LOG");
        }
    }
}

// ---------------------------------------------------------------------------
// Old flat format (≤ v2.0.0) for migration
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OldFlatConfig {
    #[serde(default)]
    version: String,
    #[serde(default = "default_theme")]
    theme: String,
    #[serde(default = "default_font_family")]
    font_family: String,
    #[serde(default = "default_font_size")]
    font_size: f32,
    #[serde(default = "default_row_height")]
    row_height: f64,
    #[serde(default = "default_true")]
    show_line_numbers: bool,
    #[serde(default)]
    alternate_rows: bool, // deprecated, ignored
    #[serde(default)]
    word_wrap: bool,
    #[serde(default)]
    show_whitespace: bool,
    #[serde(default = "default_true")]
    level_coloring: bool,
    #[serde(default)]
    case_sensitive: bool,
    #[serde(default)]
    use_regex: bool,
    #[serde(default)]
    whole_word: bool,
    #[serde(default)]
    recent_files: Vec<PathBuf>,
    #[serde(default = "default_window_size")]
    window_size: [f32; 2],
    #[serde(default)]
    window_maximized: bool,
    #[serde(default)]
    search_history: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The installer writes a minimal config.json containing only the wizard's
    /// choices (theme / font_family / index_build_mode / scan_threads). Every
    /// other field must be filled by serde defaults so the app can parse it.
    #[test]
    fn minimal_config_from_installer_parses_with_defaults() {
        let json = format!(
            r#"{{"version": "{}", "gui": {{"theme": "Dark Pro", "font_family": "NotoSansSC-VF"}}, "engine": {{"index_build_mode": "sparse", "scan_threads": 4}}}}"#,
            env!("CARGO_PKG_VERSION")
        );
        let cfg: AppConfig = serde_json::from_str(&json).expect("最小配置可解析");
        assert_eq!(cfg.gui.theme, "Dark Pro");
        assert_eq!(cfg.gui.font_family, "NotoSansSC-VF");
        assert_eq!(cfg.gui.font_size, 13.0, "未填写字段应取默认值");
        assert_eq!(cfg.gui.window_size, [1280.0, 860.0], "未填写字段应取默认值");
        assert_eq!(cfg.engine.index_build_mode, qview_core::config::IndexBuildMode::Sparse);
        assert_eq!(cfg.engine.scan_threads, 4, "安装器写入的扫描线程数");
        assert_eq!(cfg.engine.scan_window_mb, 64, "未填写字段应取默认值（64 MB）");
        assert_eq!(cfg.engine.encoding, "UTF-8", "未填写字段应取默认值");
        assert!(cfg.recent_files.is_empty());
    }

    #[test]
    fn missing_scan_threads_defaults_to_auto() {
        let json = r#"{"engine": {"index_build_mode": "full"}}"#;
        let cfg: AppConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.engine.scan_threads, 0, "旧配置缺 scan_threads 时应为 0（自动）");
    }

    #[test]
    fn minimal_config_index_mode_full() {
        let json = r#"{"engine": {"index_build_mode": "full"}}"#;
        let cfg: AppConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.engine.index_build_mode, qview_core::config::IndexBuildMode::Full);
    }
}

impl OldFlatConfig {
    fn migrate(self) -> AppConfig {
        AppConfig {
            version: env!("CARGO_PKG_VERSION").into(),
            agent: qview_agent::AgentConfig::default(),
            gui: GuiConfig {
                theme: self.theme,
                font_family: self.font_family,
                font_size: self.font_size,
                row_height: self.row_height,
                show_line_numbers: self.show_line_numbers,
                word_wrap: self.word_wrap,
                show_whitespace: self.show_whitespace,
                level_coloring: self.level_coloring,
                show_indent_guides: false,
                case_sensitive: self.case_sensitive,
                use_regex: self.use_regex,
                whole_word: self.whole_word,
                window_size: self.window_size,
                window_maximized: self.window_maximized,
                max_editable_bytes: default_max_editable_bytes(),
            },
            engine: EngineConfig::default(),
            store: StoreConfig::default(),
            llm_raw_log: false,
            recent_files: self.recent_files,
            search_history: self.search_history,
        }
    }
}
