//! `search_text` 工具：在当前文档里做字面量 / 正则搜索。

use std::sync::Arc;

use futures::future::FutureExt;
use serde_json::{json, Value};

use contexa_tools::{boxed_invoke, LocalTool, ToolResult};

use qview_core::search::SearchOptions;

use crate::protocol::SideEffect;
use crate::service::search::SearchService;
use crate::tool::metadata::{ToolGroup, ToolMetadata};

use super::info::parse_doc_id;

/// 工具元数据。
pub fn search_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "search_text",
        "在当前文档里做字面量或正则搜索，返回分页后的命中列表。注意：这是全文件扫描，大文件（数 GB）可能耗时几十秒；一次只搜最关键的一两个词",
        SideEffect::ReadOnly,
        ToolGroup::Search,
    )
}

/// 工具入参 JSON Schema。
pub fn search_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "document_id": {"type": "integer", "minimum": 1},
            "query": {"type": "string", "minLength": 1, "description": "搜索字符串"},
            "regex": {"type": "boolean", "default": false, "description": "是否按正则解析"},
            "case_sensitive": {"type": "boolean", "default": false},
            "limit": {"type": "integer", "minimum": 1, "maximum": 5000, "default": 200},
            "offset": {"type": "integer", "minimum": 0, "default": 0}
        },
        "required": ["document_id", "query"],
        "additionalProperties": false
    })
}

/// 构造工具。
pub fn search_tool(search: Arc<SearchService>) -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "search_text",
        "在当前文档里做字面量或正则搜索，返回分页后的命中列表。注意：这是全文件扫描，大文件（数 GB）可能耗时几十秒；一次只搜最关键的一两个词",
        search_parameters(),
        boxed_invoke(move |args| {
            let search = search.clone();
            async move {
                let Some(id) = parse_doc_id(&args) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"document_id"})));
                };
                let Some(query) = args.get("query").and_then(|v| v.as_str()) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"query"})));
                };
                if query.is_empty() {
                    return Ok(ToolResult::err(json!({"error":"empty_query"})));
                }
                let regex_flag = args.get("regex").and_then(|v| v.as_bool()).unwrap_or(false);
                let case = args
                    .get("case_sensitive")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(200) as usize;
                let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

                let opts = SearchOptions {
                    case_sensitive: case,
                    use_regex: regex_flag,
                    whole_word: false,
                    crlf: false,
                };

                match search.search(id, query, opts, limit, offset).await {
                    Ok(summary) => Ok(ToolResult::ok(
                        serde_json::to_value(&summary).unwrap_or(json!({})),
                    )),
                    Err(e) => Ok(ToolResult::err(json!({
                        "error": "search_failed",
                        "message": format!("{e}")
                    }))),
                }
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}
