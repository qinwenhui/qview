//! `summarize_range` 工具：把一段行范围的内容返回给 LLM（让模型侧做总结）。
//!
//! 名字里的 "summarize" 是给 LLM 的提示 —— 工具本身只负责**取原文**。
//! 输出截断由 `max_tokens`（粗估 4 字符 = 1 token）控制。

use std::sync::Arc;

use futures::future::FutureExt;
use serde_json::{json, Value};

use contexa_tools::{boxed_invoke, LocalTool, ToolResult};

use crate::protocol::SideEffect;
use crate::service::document::DocumentService;
use crate::tool::metadata::{ToolGroup, ToolMetadata};

use super::info::parse_doc_id;

/// 工具元数据。
pub fn summarize_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "summarize_range",
        "读取 [start, end] 行号区间的内容（受 max_tokens 限制），供模型侧总结",
        SideEffect::ReadOnly,
        ToolGroup::Search,
    )
}

/// 工具入参 JSON Schema。
pub fn summarize_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "document_id": {"type": "integer", "minimum": 1},
            "start": {"type": "integer", "minimum": 0},
            "end":   {"type": "integer", "minimum": 0},
            "max_tokens": {"type": "integer", "minimum": 100, "maximum": 200000, "default": 4000}
        },
        "required": ["document_id", "start", "end"],
        "additionalProperties": false
    })
}

/// 构造工具。
pub fn summarize_tool(docs: Arc<DocumentService>) -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "summarize_range",
        "读取 [start, end] 行号区间的内容（受 max_tokens 限制），供模型侧总结",
        summarize_parameters(),
        boxed_invoke(move |args| {
            let docs = docs.clone();
            async move {
                let Some(id) = parse_doc_id(&args) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"document_id"})));
                };
                let Some(start) = args.get("start").and_then(|v| v.as_u64()) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"start"})));
                };
                let Some(end) = args.get("end").and_then(|v| v.as_u64()) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"end"})));
                };
                let max_tokens = args
                    .get("max_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(4000);

                if end < start {
                    return Ok(ToolResult::err(json!({
                        "error": "invalid_range",
                        "message": "end 必须 >= start"
                    })));
                }

                let engine = match docs.engine(id) {
                    Some(e) => e,
                    None => {
                        return Ok(ToolResult::err(json!({
                            "error": "unknown_document",
                            "document_id": id.get()
                        })));
                    }
                };

                let mut lines = Vec::with_capacity((end - start) as usize);
                let max_chars = (max_tokens as usize).saturating_mul(4);
                let mut used = 0usize;
                let mut truncated = false;
                {
                    let e = engine.lock();
                    let total = e.effective_line_count();
                    let stop = end.min(total);
                    // 深行读取护栏：后台索引未完成时线性扫描极慢，先按估算代价拦截。
                    if let Some(cost) = e.estimate_read_cost_bytes(stop.saturating_sub(1)) {
                        if cost > crate::MAX_INDEXING_SCAN_BYTES {
                            return Ok(ToolResult::err(json!({
                                "error": "index_building",
                                "start": start,
                                "end": stop,
                                "total_lines_estimated": total,
                                "message": format!(
                                    "文件的行索引仍在后台构建中（is_indexed=false），目标区域过深（估算扫描 {} MiB），逐行线性读取会非常慢。请稍候重试——索引完成后读取为秒级；或先读取文件头部 / 较浅行号的区域。",
                                    cost / (1024 * 1024)
                                ),
                            })));
                        }
                    }
                    for l in start..stop {
                        let raw = e.read_line(l);
                        let line_chars = raw.text.len() + 32;
                        if used + line_chars > max_chars {
                            truncated = true;
                            break;
                        }
                        used += line_chars;
                        lines.push(json!({"line": l, "text": raw.text}));
                    }
                }

                Ok(ToolResult::ok(json!({
                    "start": start,
                    "end": start + lines.len() as u64,
                    "truncated": truncated,
                    "used_chars": used,
                    "lines": lines,
                })))
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}
