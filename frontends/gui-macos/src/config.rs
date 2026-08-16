//! 持久化配置 — 与 Windows egui 前端共享同一套 AppConfig v2.1 JSON schema，
//! 但存储目录改为 macOS 平台的应用数据目录：
//!
//!   ~/Library/Application Support/qview/config.json
//!   ~/Library/Application Support/qview/index/*.qli
//!
//! 旧 flat-format 配置（≤ v2.0.0）自动迁移到分组格式。

use std::path::PathBuf;

use qview_core::config::EngineConfig;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// GuiConfig — 所有显示 / 用户偏好字段
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
        }
    }
}

// ---------------------------------------------------------------------------
// AppConfig — 顶层容器
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_version")]
    pub version: String,

    #[serde(default)]
    pub gui: GuiConfig,

    #[serde(default)]
    pub engine: EngineConfig,

    #[serde(default)]
    pub recent_files: Vec<PathBuf>,

    #[serde(default)]
    pub search_history: Vec<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: default_version(),
            gui: GuiConfig::default(),
            engine: EngineConfig::default(),
            recent_files: Vec::new(),
            search_history: Vec::new(),
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
    "Menlo".into()
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
    /// macOS 应用数据目录：~/Library/Application Support/qview
    pub fn config_dir() -> Option<PathBuf> {
        home_dir().map(|h| h.join("Library/Application Support/qview"))
    }

    /// 最近文件（字符串形式，供菜单显示 / 打开）。
    pub fn recent_files(&self) -> Vec<String> {
        self.recent_files
            .iter()
            .map(|p| p.display().to_string())
            .collect()
    }

    fn config_path() -> Option<PathBuf> {
        Self::config_dir().map(|d| d.join("config.json"))
    }

    /// 从磁盘加载配置；缺失或损坏则回退默认值。
    /// 旧 flat-format 配置（≤ v2.0.0）自动迁移。
    pub fn load() -> Self {
        let path = match Self::config_path() {
            Some(p) => p,
            None => return Self::default(),
        };
        let json = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => return Self::with_defaults(),
        };

        // 优先尝试新的分组格式。
        if let Ok(mut cfg) = serde_json::from_str::<Self>(&json) {
            let current_ver = default_version();
            if !cfg.gui.theme.is_empty()
                || cfg.version == "2.1.0"
                || cfg.version == current_ver
            {
                cfg.ensure_index_dir();
                return cfg;
            }
        }

        // 尝试旧 flat 格式并迁移。
        if let Ok(old) = serde_json::from_str::<OldFlatConfig>(&json) {
            let mut migrated = old.migrate();
            migrated.ensure_index_dir();
            migrated.save();
            return migrated;
        }

        // 损坏 — 备份后从默认值重新开始。
        let _ = std::fs::rename(&path, path.with_extension("json.bak"));
        Self::with_defaults()
    }

    /// 返回设置好默认索引目录的默认配置。
    fn with_defaults() -> Self {
        let mut cfg = Self::default();
        cfg.ensure_index_dir();
        cfg
    }

    /// 若 `engine.index_dir` 为 None，则设为默认索引目录。
    fn ensure_index_dir(&mut self) {
        if self.engine.index_dir.is_none() {
            if let Some(dir) = Self::config_dir() {
                self.engine.index_dir = Some(dir.join("index"));
            }
        }
    }

    /// 持久化到磁盘。
    pub fn save(&self) {
        let path = match Self::config_path() {
            Some(p) => p,
            None => return,
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&path, json);
        }
    }

    /// 把文件加入最近文件列表最前，保留至多 10 条并去重。
    pub fn add_recent(&mut self, path: PathBuf) {
        self.recent_files.retain(|p| p != &path);
        self.recent_files.insert(0, path);
        self.recent_files.truncate(10);
    }

    /// 把查询加入搜索历史，保留至多 20 条。
    pub fn add_search_history(&mut self, query: String) {
        self.search_history.retain(|q| q != &query);
        self.search_history.insert(0, query);
        self.search_history.truncate(20);
    }

    /// 记录窗口状态。
    pub fn set_window_state(&mut self, size: [f32; 2], maximized: bool) {
        if !maximized {
            self.gui.window_size = size;
        }
        self.gui.window_maximized = maximized;
    }
}

/// 返回用户主目录（$HOME）。macOS 下总是可用。
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
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

impl OldFlatConfig {
    fn migrate(self) -> AppConfig {
        AppConfig {
            version: env!("CARGO_PKG_VERSION").into(),
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
            },
            engine: EngineConfig::default(),
            recent_files: self.recent_files,
            search_history: self.search_history,
        }
    }
}
