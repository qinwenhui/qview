//! `read_context` 工具：读取指定行号附近的若干行（前后各 N 行）。

use std::sync::Arc;

use futures::future::FutureExt;
use serde_json::{json, Value};

use contexa_tools::{boxed_invoke, LocalTool, ToolResult};

use crate::protocol::SideEffect;
use crate::service::document::DocumentService;
use crate::tool::metadata::{ToolGroup, ToolMetadata};
use crate::DEFAULT_MAX_READ_LINES;

use super::info::parse_doc_id;

/// 工具元数据。
pub fn read_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "read_context",
        "读取指定行号附近的若干行（before / after 默认各 20 行；每行返回 byte_start/byte_end 字节偏移，供 annotate_create 填 start_byte/end_byte）",
        SideEffect::ReadOnly,
        ToolGroup::Search,
    )
}

/// 工具入参 JSON Schema。
pub fn read_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "document_id": {"type": "integer", "minimum": 1},
            "line": {"type": "integer", "minimum": 0, "description": "中心行号（0-based）"},
            "before": {"type": "integer", "minimum": 0, "maximum": 1000, "default": 20},
            "after": {"type": "integer", "minimum": 0, "maximum": 1000, "default": 20},
            "max_lines": {"type": "integer", "minimum": 1, "maximum": 5000, "default": 200}
        },
        "required": ["document_id", "line"],
        "additionalProperties": false
    })
}

/// 构造工具。
pub fn read_tool(docs: Arc<DocumentService>) -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "read_context",
        "读取指定行号附近的若干行（before / after 默认各 20 行；每行返回 byte_start/byte_end 字节偏移，供 annotate_create 填 start_byte/end_byte）",
        read_parameters(),
        boxed_invoke(move |args| {
            let docs = docs.clone();
            async move {
                let Some(id) = parse_doc_id(&args) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"document_id"})));
                };
                let Some(line) = args.get("line").and_then(|v| v.as_u64()) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"line"})));
                };
                let before = args.get("before").and_then(|v| v.as_u64()).unwrap_or(20);
                let after = args.get("after").and_then(|v| v.as_u64()).unwrap_or(20);
                let max_lines = args
                    .get("max_lines")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(DEFAULT_MAX_READ_LINES);

                let engine = match docs.engine(id) {
                    Some(e) => e,
                    None => {
                        return Ok(ToolResult::err(json!({
                            "error": "unknown_document",
                            "document_id": id.get()
                        })));
                    }
                };

                let total = {
                    let e = engine.lock();
                    e.effective_line_count()
                };
                let start = line.saturating_sub(before);
                let mut end = line.saturating_add(after).saturating_add(1).min(total);
                if end <= start {
                    return Ok(ToolResult::ok(json!({
                        "line": line,
                        "line_range": [start, start],
                        "lines": [],
                        "truncated": false,
                        "total_lines": total,
                    })));
                }
                let truncated_by_limit = (end - start) > max_lines;
                if truncated_by_limit {
                    end = start + max_lines;
                }

                let mut lines = Vec::with_capacity((end - start) as usize);
                {
                    let e = engine.lock();
                    // 深行读取护栏：后台索引未完成时线性扫描极慢，先按估算代价拦截。
                    // 索引已完成 → estimate_read_cost_bytes 返回 None，不拦。
                    if let Some(cost) = e.estimate_read_cost_bytes(end.saturating_sub(1)) {
                        if cost > crate::MAX_INDEXING_SCAN_BYTES {
                            return Ok(ToolResult::err(json!({
                                "error": "index_building",
                                "line": line,
                                "line_range": [start, end],
                                "total_lines_estimated": total,
                                "message": format!(
                                    "文件的行索引仍在后台构建中（is_indexed=false），目标区域过深（估算扫描 {} MiB），逐行线性读取会非常慢。请稍候重试——索引完成后读取为秒级；或先读取文件头部 / 较浅行号的区域。",
                                    cost / (1024 * 1024)
                                ),
                            })));
                        }
                    }
                    for l in start..end {
                        let raw = e.read_line(l);
                        if raw.text.is_empty() && l >= total {
                            break;
                        }
                        // 字节偏移（annotate_create 的 start_byte / end_byte 用）。
                        // 直接复用 RawLine 自带的 start_byte / byte_len —— 它就是「不含
                        // 行尾换行 / \r」的干净区间，与 line_byte_range 等价，且省掉一次
                        // 后台索引未完成时的重复线性扫描。
                        let byte_start = raw.start_byte;
                        let byte_end = raw.start_byte + raw.byte_len as u64;
                        lines.push(json!({
                            "line": l,
                            "byte_start": byte_start,
                            "byte_end": byte_end,
                            "text": raw.text,
                            "modified": raw.modified,
                        }));
                    }
                }

                Ok(ToolResult::ok(json!({
                    "line": line,
                    "line_range": [start, end],
                    "lines": lines,
                    "truncated": truncated_by_limit,
                    "total_lines": total,
                })))
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}
