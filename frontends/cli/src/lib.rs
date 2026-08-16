//! qview: terminal UI for log browsing.
//!
//! Exports `app` (application state wrapping qview-core's Engine),
//! `tui` (terminal rendering and input handling), and `config`
//! (TOML config loader producing a `qview_core::config::EngineConfig`).

pub mod app;
pub mod config;
pub mod tui;

// Re-export core modules for convenience (tests, examples).
pub use qview_core::{file, search};
