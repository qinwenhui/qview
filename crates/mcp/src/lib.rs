//! `qview-mcp` — 器灵 MCP 边界（架构文档 §12）。
//!
//! ## 职责
//! - **server feature**：把 qview 的 `ToolRegistry` 通过 stdio JSON-RPC 暴露为 MCP server
//!   （供 Claude Desktop / 其他 Agent 连接）。
//! - **client feature**：把外部 MCP server 通过 `contexa_tools::McpClient` 桥接为
//!   `Arc<dyn ToolSource>`，注入到 qview Agent 的 `ReActWorker::instance_sources`。
//!
//! ## 协议映射（架构 §12.4）
//! - `ToolSpec::name` → MCP `name`
//! - `ToolSpec::description` → MCP `description`
//! - `ToolSpec::parameters` → MCP `inputSchema`
//! - `ToolResult::content` → MCP `content`
//! - `ToolResult::is_error` → MCP `isError`
//! - `SideEffect::ReadOnly` → `annotations.readOnlyHint = true`
//! - `SideEffect::Mutating/Destructive` → `annotations.destructiveHint = true`
//! - `ViewIntent` → **不**映射到 MCP
//! - `worker_finish` → **不**暴露为 MCP tool（架构 §12.4）
//!
//! ## 设计选择
//! - server 端独立实现 stdio JSON-RPC 子集（contexa-rs 0.1.0 仅提供 client）；
//!   复用 `ToolSpec` / `ToolResult`，不重写类型层。

#![forbid(unsafe_code)]
#![allow(missing_docs)]

#[cfg(feature = "server")]
pub mod server;

#[cfg(feature = "client")]
pub mod client;

pub mod map;

/// MCP server / client 共享的错误类型。
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON 错误: {0}")]
    Json(#[from] serde_json::Error),
    #[error("MCP 协议错误: {0}")]
    Protocol(String),
    #[error("工具未找到: {0}")]
    ToolNotFound(String),
    #[error("内部错误: {0}")]
    Internal(String),
}
