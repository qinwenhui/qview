//! `get_document_info` 工具：返回当前文档的元信息（行数 / 字节 / 编码等）。

use std::sync::Arc;

use futures::future::FutureExt;
use serde_json::{json, Value};

use contexa_tools::{boxed_invoke, LocalTool, ToolResult};

use crate::protocol::{DocumentId, SideEffect};
use crate::service::document::DocumentService;
use crate::tool::metadata::{ToolGroup, ToolMetadata};

/// 工具元数据。
pub fn info_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "get_document_info",
        "返回当前文档的元信息（行数 / 字节 / 编码 / 是否已索引 / modified）。line_count_estimated=true 时 total_lines 是估算值（后台索引未完成，按字节/80 粗估），汇报行数应说明是估算",
        SideEffect::ReadOnly,
        ToolGroup::Document,
    )
}

/// 工具入参 JSON Schema。
pub fn info_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "document_id": {
                "type": "integer",
                "minimum": 1,
                "description": "DocumentService 打开文档时返回的 id"
            }
        },
        "required": ["document_id"],
        "additionalProperties": false
    })
}

/// 构造工具（捕获 `Arc<DocumentService>`）。
pub fn info_tool(docs: Arc<DocumentService>) -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "get_document_info",
        "返回当前文档的元信息（行数 / 字节 / 编码 / 是否已索引 / modified）。line_count_estimated=true 时 total_lines 是估算值（后台索引未完成，按字节/80 粗估），汇报行数应说明是估算",
        info_parameters(),
        boxed_invoke(move |args| {
            let docs = docs.clone();
            async move {
                let Some(id) = parse_doc_id(&args) else {
                    return Ok(ToolResult::err(json!({
                        "error": "missing_argument",
                        "argument": "document_id"
                    })));
                };
                match docs.info(id).await {
                    Some(info) => Ok(ToolResult::ok(serde_json::to_value(&info).unwrap_or(json!({})))),
                    None => Ok(ToolResult::err(json!({
                        "error": "unknown_document",
                        "document_id": id.get(),
                        "message": "DocumentService 中不存在该 id（可能被关闭）"
                    }))),
                }
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}

pub(crate) fn parse_doc_id(args: &Value) -> Option<DocumentId> {
    args.get("document_id")
        .and_then(|v| v.as_u64())
        .map(DocumentId)
}
