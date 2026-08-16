//! MCP stdio JSON-RPC server（架构 §12.3）。
//!
//! 实现 MCP 协议 2024-11-25 的最小子集：
//! - `initialize` → 返回 server capabilities
//! - `tools/list` → 返回工具列表（SideEffect → annotations）
//! - `tools/call` → 转发到 `ToolRegistry::call_tool`
//!
//! **不**暴露 `worker_finish`（架构 §12.4）。

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use qview_application::tool::ToolRegistry;

use crate::map::to_mcp_tool;
use crate::McpError;

/// MCP server 配置（server name / version）。
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub name: String,
    pub version: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: "qview-mcp".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}

/// MCP server handle。
pub struct McpServer {
    config: ServerConfig,
    registry: Arc<ToolRegistry>,
    /// 写入器（stdout）独占锁 — JSON-RPC 响应单线程串行。
    writer: Mutex<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>,
}

impl McpServer {
    /// 构造 server。
    ///
    /// `writer`：通常是 `tokio::io::stdout()`（stdio transport）。
    pub fn new(
        config: ServerConfig,
        registry: Arc<ToolRegistry>,
        writer: Box<dyn tokio::io::AsyncWrite + Send + Unpin>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            registry,
            writer: Mutex::new(writer),
        })
    }

    /// 启动服务循环：从 `reader` 读 JSON-RPC 请求，逐一处理并把响应写回 `writer`。
    ///
    /// 该循环**不**主动退出；调用方负责把 reader EOF / 错误信号转成取消。
    pub async fn serve(
        self: Arc<Self>,
        reader: Box<dyn tokio::io::AsyncBufRead + Send + Unpin>,
    ) -> Result<(), McpError> {
        let mut lines = reader.lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let response = self.handle_request(&line).await;
                    if let Some(resp) = response {
                        let mut w = self.writer.lock().await;
                        w.write_all(resp.as_bytes()).await?;
                        w.write_all(b"\n").await?;
                        w.flush().await?;
                    }
                }
                Ok(None) => return Ok(()), // EOF
                Err(e) => return Err(McpError::Io(e)),
            }
        }
    }

    /// 单条请求的处理（也可被 e2e 测试直接调）。
    pub async fn handle_request(&self, raw: &str) -> Option<String> {
        let req: Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(e) => {
                return Some(jsonrpc_error(None, -32700, format!("parse error: {e}")));
            }
        };
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(json!({}));

        match method {
            "initialize" => Some(jsonrpc_ok(id, initialize_response(&self.config))),
            "tools/list" => {
                let list = self.list_tools().await;
                Some(jsonrpc_ok(id, json!({"tools": list})))
            }
            "tools/call" => {
                let r = self.call_tool(params).await;
                Some(match r {
                    Ok(v) => jsonrpc_ok(id, v),
                    Err(e) => jsonrpc_error(id, -32603, format!("{e}")),
                })
            }
            "notifications/initialized" => None, // 通知不响应
            "ping" => Some(jsonrpc_ok(id, json!({}))),
            other => Some(jsonrpc_error(id, -32601, format!("method not found: {other}"))),
        }
    }

    async fn list_tools(&self) -> Vec<Value> {
        let specs = self.registry.effective_specs().await;
        let mut out = Vec::with_capacity(specs.len());
        for spec in specs {
            // 过滤 worker_finish（架构 §12.4：保留名不暴露为 MCP tool）
            if spec.name == contexa_context::FINISH_TOOL_NAME {
                continue;
            }
            // 找 metadata（缺则用默认 ReadOnly）
            let meta = self.registry.metadata_of(&spec.name).unwrap_or_else(|| {
                qview_application::tool::metadata::ToolMetadata::new(
                    spec.name.clone(),
                    spec.description.clone(),
                    qview_application::protocol::SideEffect::ReadOnly,
                    qview_application::tool::metadata::ToolGroup::Document,
                )
            });
            out.push(to_mcp_tool(&spec, &meta));
        }
        out
    }

    async fn call_tool(&self, params: Value) -> Result<Value, McpError> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::Protocol("missing 'name'".into()))?;
        if name == contexa_context::FINISH_TOOL_NAME {
            return Err(McpError::Protocol("worker_finish 不暴露为 MCP tool".into()));
        }
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

        let result = self.registry.call_tool(name, arguments).await;
        Ok(json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&result.content)?,
            }],
            "isError": result.is_error,
        }))
    }
}

/// 启动 stdio server（标准 stdin/stdout transport）。
pub async fn run_stdio(registry: Arc<ToolRegistry>, config: ServerConfig) -> Result<(), McpError> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let reader = BufReader::new(stdin);
    let writer: Box<dyn tokio::io::AsyncWrite + Send + Unpin> = Box::new(stdout);
    let server = McpServer::new(config, registry, writer);
    server.serve(Box::new(reader)).await
}

fn initialize_response(cfg: &ServerConfig) -> Value {
    json!({
        "protocolVersion": "2024-11-25",
        "capabilities": {
            "tools": {"listChanged": false}
        },
        "serverInfo": {
            "name": cfg.name,
            "version": cfg.version,
        }
    })
}

fn jsonrpc_ok(id: Option<Value>, result: Value) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
    .unwrap_or_default()
}

fn jsonrpc_error(id: Option<Value>, code: i32, message: String) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message},
    }))
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use qview_application::protocol::PermissionPolicy;
    use qview_application::service::annotation::AnnotationService;
    use qview_application::service::{DocumentService, SearchService};
    use qview_application::tools::{register_defaults, ALL_TOOL_NAMES_WITH_WRITES};
    use std::path::PathBuf;

    fn fixture_log() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("qview-mcp-{}.log", uuid::Uuid::new_v4()));
        std::fs::write(&p, b"line1\nline2\n").unwrap();
        p
    }

    fn make_registry() -> Arc<ToolRegistry> {
        let path = fixture_log();
        let docs = Arc::new(DocumentService::default());
        docs.open(path.clone()).unwrap();
        let search = Arc::new(SearchService::new(docs.clone()));
        let ann = Arc::new(AnnotationService::new(docs.clone()));
        let mut reg = ToolRegistry::new(PermissionPolicy::with_allowlist(
            ALL_TOOL_NAMES_WITH_WRITES.iter().map(|s| s.to_string()).collect(),
        ));
        register_defaults(
            &mut reg,
            docs,
            search,
            Some(ann),
            qview_application::tools::SharedViewport::default(),
            &["annotate_create", "export_report", "write_document"],
        )
        .unwrap();
        Arc::new(reg)
    }

    #[tokio::test]
    async fn initialize_returns_server_info() {
        let reg = make_registry();
        let writer = Box::new(Vec::<u8>::new());
        let server = McpServer::new(ServerConfig::default(), reg, writer);
        let resp = server
            .handle_request(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"]["serverInfo"]["name"], "qview-mcp");
    }

    #[tokio::test]
    async fn tools_list_excludes_worker_finish() {
        let reg = make_registry();
        let writer = Box::new(Vec::<u8>::new());
        let server = McpServer::new(ServerConfig::default(), reg, writer);
        let resp = server
            .handle_request(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#)
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        let tools = v["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(!names.contains(&"worker_finish"), "worker_finish 不应暴露");
        assert!(names.contains(&"search_text"));
        assert!(names.contains(&"get_document_info"));
        // annotations 存在
        let search = tools.iter().find(|t| t["name"] == "search_text").unwrap();
        assert_eq!(search["annotations"]["readOnlyHint"], true);
    }

    #[tokio::test]
    async fn tools_call_unknown_returns_error() {
        let reg = make_registry();
        let writer = Box::new(Vec::<u8>::new());
        let server = McpServer::new(ServerConfig::default(), reg, writer);
        let resp = server
            .handle_request(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"nonexistent","arguments":{}}}"#,
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"]["isError"], true);
    }

    #[tokio::test]
    async fn tools_call_worker_finish_rejected() {
        let reg = make_registry();
        let writer = Box::new(Vec::<u8>::new());
        let server = McpServer::new(ServerConfig::default(), reg, writer);
        let resp = server
            .handle_request(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"worker_finish","arguments":{}}}"#,
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(v.get("error").is_some());
        assert!(v["error"]["message"].as_str().unwrap().contains("worker_finish"));
    }
}
