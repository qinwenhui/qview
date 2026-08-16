//! 持久化设置。所有数据保存在 exe 同目录下的 `data/`：
//!
//!   {exe_dir}/data/config.json
//!   {exe_dir}/data/index/*.qli
//!
//! 可移植：复制整个文件夹即带走全部配置和缓存。安装程序可指定安装目录。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FontSetting {
    pub name: String,
    pub pixel: i32,
}
impl Default for FontSetting {
    fn default() -> Self {
        Self { name: "Consolas".into(), pixel: 14 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub font: FontSetting,
    pub recent_files: Vec<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            font: FontSetting::default(),
            recent_files: Vec::new(),
        }
    }
}

/// 数据根目录 = exe 所在目录 / data
pub fn data_dir() -> PathBuf {
    exe_dir().join("data")
}

/// 索引缓存目录 = data/index
pub fn index_dir() -> PathBuf {
    data_dir().join("index")
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn load() -> AppSettings {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => AppSettings::default(),
    }
}

pub fn save(s: &AppSettings) {
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::create_dir_all(&index_dir());
    if let Ok(json) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(config_path(), json);
    }
}
