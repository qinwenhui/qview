//! TUI configuration loader.
//!
//! Reads an optional TOML file from `--config <path>` or `~/.config/qview/config.toml`,
//! deserializes it into `qview_core::config::EngineConfig`, and exposes it for
//! construction of the core `Engine`.
//!
//! Unknown fields are tolerated (`#[serde(default)]` on every field); missing
//! fields fall back to `EngineConfig::default()`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub use qview_core::config::{EngineConfig, SearchConfig};

/// On-disk representation of the TUI config. Mirrors `EngineConfig` directly;
/// future TUI-only knobs (key bindings, theme, etc.) can extend this.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AppConfig(pub EngineConfig);

impl AppConfig {
    /// Path to the user-level config file (`~/.config/qview/config.toml`).
    /// Returns `None` if the home directory cannot be determined.
    pub fn default_path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))?;
        let mut p = PathBuf::from(home);
        if cfg!(windows) {
            p.push("AppData");
            p.push("Roaming");
            p.push("qview");
        } else {
            p.push(".config");
            p.push("qview");
        }
        p.push("config.toml");
        Some(p)
    }

    /// Load config from the given path. Missing file → `Default::default()`.
    /// Invalid file → error with the underlying parse message.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read config {}", path.display()))?;
        let cfg: Self = toml::from_str(&text)
            .with_context(|| format!("parse config {}", path.display()))?;
        Ok(cfg)
    }

    /// Load from `--config <path>` if provided, else the user-level default,
    /// else `Default::default()`. Logs the source for diagnostics.
    pub fn load_with_override(explicit: Option<&Path>) -> Result<(Self, PathBuf)> {
        if let Some(p) = explicit {
            let cfg = Self::load(p)?;
            return Ok((cfg, p.to_path_buf()));
        }
        if let Some(p) = Self::default_path() {
            if p.exists() {
                let cfg = Self::load(&p)?;
                return Ok((cfg, p));
            }
        }
        Ok((Self::default(), PathBuf::from("<default>")))
    }

    /// Borrow the inner engine config.
    #[inline]
    pub fn engine_config(&self) -> &EngineConfig { &self.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal() {
        let s = "";
        let cfg: AppConfig = toml::from_str(s).unwrap();
        // All defaults — verify a couple of well-known fields.
        assert_eq!(cfg.0.search.sample_interval, SearchConfig::default().sample_interval);
        assert_eq!(cfg.0.small_file_threshold, 10 * 1024 * 1024);
    }

    #[test]
    fn parses_search_block() {
        let s = r#"
            small_file_threshold = 5242880
            [search]
            sample_interval = 50
            max_samples = 1000000
        "#;
        let cfg: AppConfig = toml::from_str(s).unwrap();
        assert_eq!(cfg.0.small_file_threshold, 5242880);
        assert_eq!(cfg.0.search.sample_interval, 50);
        assert_eq!(cfg.0.search.max_samples, 1000000);
    }

    #[test]
    fn load_missing_returns_default() {
        let cfg = AppConfig::load(Path::new("nonexistent-config-file.toml")).unwrap();
        assert_eq!(cfg.0.search.sample_interval, 100);
    }
}