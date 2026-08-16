//! MCP client 包装（架构 §12.3）。
//!
//! 把外部 MCP server 暴露的工具桥接到 qview Agent：
//! 1. 启动 `contexa_tools::McpClient::initialize()`
//! 2. 用 `McpToolSource::list_tools()` → 转换 SideEffect
//! 3. 注入到 `ReActWorker::instance_sources`
//!
//! 权限仍由 qview::PermissionPolicy::allow_tools 控制（架构 §11.1）。

use std::sync::Arc;

use anyhow::Context as _;
use serde_json::Value;

use contexa_tools::{McpClient, McpToolSource, ToolSource};

use crate::map::from_mcp_tool;
use crate::McpError;

/// MCP client 桥接器（连接 + 持有 ToolSource）。
pub struct McpClientBridge {
    source: McpToolSource,
}

impl std::fmt::Debug for McpClientBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClientBridge").finish_non_exhaustive()
    }
}

impl McpClientBridge {
    /// 构造并初始化（handshake）。
    pub async fn connect(client: McpClient) -> Result<Arc<Self>, McpError> {
        client
            .initialize()
            .await
            .map_err(|e| McpError::Protocol(format!("initialize failed: {e}")))?;
        Ok(Arc::new(Self {
            source: McpToolSource::new(client),
        }))
    }

    /// 暴露为 `Arc<dyn ToolSource>` 注入到 `ReActWorker::instance_sources`。
    pub fn as_source(self: &Arc<Self>) -> Arc<dyn ToolSource> {
        // McpToolSource 是具体类型；clone 出 Arc<dyn ToolSource>。
        let inner = Arc::new(McpToolSource::new(self.source.inner().clone()));
        inner as Arc<dyn ToolSource>
    }
}

/// 便捷函数：连接并包装成 ToolSource。
pub async fn connect_and_wrap(client: McpClient) -> anyhow::Result<Arc<dyn ToolSource>> {
    let bridge = McpClientBridge::connect(client)
        .await
        .context("connect")?;
    Ok(bridge.as_source())
}

/// 工具列表（仅 spec.name + side 元组）。
pub type ToolSummary = (contexa_context::ToolSpec, qview_application::protocol::SideEffect);

/// 用于 e2e / UI 展示的 helper：把 `tools/list` 原始 JSON 转成 summary。
pub fn parse_tools_list(value: &Value) -> Vec<ToolSummary> {
    value
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(from_mcp_tool).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_tools_list_extracts_names() {
        let v = json!({
            "tools": [
                {"name": "a", "description": "x", "inputSchema": {"type": "object"}, "annotations": {"readOnlyHint": true}},
                {"name": "b", "description": "y", "inputSchema": {"type": "object"}, "annotations": {"destructiveHint": true}},
            ]
        });
        let list = parse_tools_list(&v);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].0.name, "a");
        assert_eq!(list[0].1, qview_application::protocol::SideEffect::ReadOnly);
        assert_eq!(list[1].1, qview_application::protocol::SideEffect::Mutating);
    }
}
