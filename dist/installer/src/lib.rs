//! qview-installer shared library.
//!
//! `manifest` compiles for every binary (serde-only). `qpak` and `install`
//! are gated behind the `installer` feature so the tiny uninstaller binary
//! doesn't link the egui / zstd tree.

pub mod manifest;

#[cfg(feature = "installer")]
pub mod install;

#[cfg(feature = "installer")]
pub mod qpak;
