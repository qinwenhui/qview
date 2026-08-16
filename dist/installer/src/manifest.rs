//! Uninstall manifest types — serde-only so the tiny uninstaller binary can
//! read them without pulling in the egui / zstd dependency tree.

use serde::{Deserialize, Serialize};

/// A registry VALUE the uninstaller must remove without deleting the parent
/// key (OpenWithProgIds entries are shared with other apps).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValueToDelete {
    pub parent: String,
    pub name: String,
}

/// Everything the uninstaller needs to reverse an install.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UninstallManifest {
    pub install_dir: String,
    pub shortcut: Option<String>,
    pub keys_to_delete: Vec<String>,
    pub values_to_delete: Vec<ValueToDelete>,
    pub uninstall_key: String,
}
