//! 主视图可见范围快照：UI 每帧发布 → `get_viewport` 工具读取。
//!
//! 这是唯一一条「UI → Agent」的数据通道：UI 把当前可见行 / 选区
//! 写进共享 `Arc<Mutex<Option<ViewportSnapshot>>>`，工具侧只读。

use serde::{Deserialize, Serialize};

/// 当前主视图快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewportSnapshot {
    /// 当前可见的第一行（0 基，逻辑行）。
    pub first_visible_line: u64,
    /// 当前可见的最后一行（0 基，逻辑行）。
    pub last_visible_line: u64,
    /// 当前选区（start_line, end_line，0 基）；无选区为 None。
    pub selection: Option<(u64, u64)>,
}
