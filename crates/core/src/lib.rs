//! qview-core: UI-agnostic engine for log file browsing.
//!
//! Provides mmap file access, line indexing, search, edit buffer, and caching.
//! Used by the terminal UI (`qview`) and future GUI frontends.

pub mod annotation;
pub mod cache;
pub mod config;
pub mod parallel;
pub mod edit;
pub mod engine;
pub mod file;
pub mod search;
