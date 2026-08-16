//! `qview-application` — application capability layer.
//!
//! 把 `qview-core::Engine` 包装为 Agent 可调用的"语义化工具"。
//! 自身**不**实现 Worker / Runtime，只负责"工具 → 服务 → 引擎"这条链路。
//!

#![forbid(unsafe_code)]
// 内部 crate 不对外发布，工具注册/事件枚举等样板不强制文档，避免编译噪音。
#![allow(missing_docs)]

pub mod protocol;
pub mod service;
pub mod tool;
pub mod tools;

pub use protocol::{
    DocumentId, FilterSpec, HighlightKind, MessageLevel, PanelKind, PermissionPolicy, ProposalId,
    SideEffect, ToolCallId, ViewIntent,
};
pub use service::{DocumentService, SearchService};
pub use tool::{ToolMetadata, ToolRegistry};

/// 默认工具一次读取的最大行数（与架构文档 §11.4 对齐）。
pub const DEFAULT_MAX_READ_LINES: u64 = 200;

/// 工具单次调用的默认超时（架构文档 §11.4）。
pub const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 30;

/// 后台索引未完成时，深行读取护栏阈值（估算线性扫描字节数）。
/// 索引未完成时 `read_line` 走从头线性扫描（O(字节偏移)），大文件深行读取
/// 会卡住几十秒到几分钟。工具层在 `estimate_read_cost_bytes` 超过此值时
/// 拒绝扫描并返回清晰错误，引导稍后重试（索引完成后即秒级）。
pub const MAX_INDEXING_SCAN_BYTES: u64 = 32 * 1024 * 1024; // 32 MiB
