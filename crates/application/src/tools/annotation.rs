//! 批注工具：`annotate_create` / `annotate_update` / `annotate_delete` / `annotate_list`。
//!
//! 写操作（create / update / delete）默认 Reversible —— 按 `require_approval`
//! 决定是否包 GuardedTool（默认自动放行）。

use std::sync::Arc;

use futures::future::FutureExt;
use serde_json::{json, Value};

use contexa_tools::{boxed_invoke, LocalTool, ToolResult};

use crate::protocol::SideEffect;
use crate::service::annotation::AnnotationService;
use crate::tool::metadata::{ToolGroup, ToolMetadata};

use super::info::parse_doc_id;

/// 工具元数据。
pub fn annotate_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "annotate_create",
        "在 [start_byte, end_byte] 范围创建批注（需用户审批；写入 AnnotationStore）",
        SideEffect::Reversible,
        ToolGroup::Annotation,
    )
}

/// 工具入参 JSON Schema。
pub fn annotate_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "document_id": {"type": "integer", "minimum": 1},
            "start_byte": {"type": "integer", "minimum": 0},
            "end_byte": {"type": "integer", "minimum": 0},
            "start_line": {"type": "integer", "minimum": 0},
            "end_line": {"type": "integer", "minimum": 0},
            "start_col": {"type": "integer", "minimum": 0},
            "end_col": {"type": "integer", "minimum": 0},
            "selected_text": {"type": "string", "description": "选区原文（≤ 4 KiB）"},
            "text": {"type": "string", "minLength": 1, "description": "批注内容"}
        },
        "required": [
            "document_id", "start_byte", "end_byte", "start_line", "end_line",
            "start_col", "end_col", "selected_text", "text"
        ],
        "additionalProperties": false
    })
}

/// 构造工具。
///
/// 注意：该工具**必须**经 GuardedTool 包装（P4 见 `qview-agent::guarded_tool`），
/// 裸 `LocalTool` 注册会被 PermissionPolicy 拒绝（架构 §6.3）。
pub fn annotate_tool(ann: Arc<AnnotationService>) -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "annotate_create",
        "在 [start_byte, end_byte] 范围创建批注（需用户审批；写入 AnnotationStore）",
        annotate_parameters(),
        boxed_invoke(move |args| {
            let ann = ann.clone();
            async move {
                let Some(doc_id) = parse_doc_id(&args) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"document_id"})));
                };
                let start_byte = args.get("start_byte").and_then(|v| v.as_u64()).unwrap_or(0);
                let end_byte = args.get("end_byte").and_then(|v| v.as_u64()).unwrap_or(0);
                let start_line = args.get("start_line").and_then(|v| v.as_u64()).unwrap_or(0);
                let end_line = args.get("end_line").and_then(|v| v.as_u64()).unwrap_or(0);
                let start_col = args.get("start_col").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let end_col = args.get("end_col").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let selected_text = args
                    .get("selected_text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let text = args
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if text.is_empty() {
                    return Ok(ToolResult::err(json!({"error":"empty_text"})));
                }
                if end_byte < start_byte {
                    return Ok(ToolResult::err(json!({"error":"invalid_range"})));
                }

                match ann
                    .create(
                        doc_id,
                        start_byte,
                        end_byte,
                        start_line,
                        end_line,
                        start_col,
                        end_col,
                        selected_text,
                        text,
                    )
                    .await
                {
                    Ok(id) => Ok(ToolResult::ok(json!({
                        "annotation_id": id,
                        "created": true,
                    }))),
                    Err(e) => Ok(ToolResult::err(json!({
                        "error": "create_failed",
                        "message": e,
                    }))),
                }
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}

// ─────────────── annotate_list ───────────────

pub fn annotate_list_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "annotate_list",
        "列出指定文档的全部批注（id / 行范围 / 内容 / 时间）",
        SideEffect::ReadOnly,
        ToolGroup::Annotation,
    )
}

pub fn annotate_list_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "document_id": {"type": "integer", "minimum": 1}
        },
        "required": ["document_id"],
        "additionalProperties": false
    })
}

pub fn annotate_list_tool(ann: Arc<AnnotationService>) -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "annotate_list",
        "列出指定文档的全部批注（id / 行范围 / 内容 / 时间）",
        annotate_list_parameters(),
        boxed_invoke(move |args| {
            let ann = ann.clone();
            async move {
                let Some(doc_id) = parse_doc_id(&args) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"document_id"})));
                };
                let list = ann.list(doc_id).await;
                // 序列化时截断 selected_text 长快照，避免巨量回包
                let items: Vec<Value> = list
                    .iter()
                    .map(|a| {
                        let selected: String = a.selected_text.chars().take(200).collect();
                        let note: String = a.text.chars().take(500).collect();
                        json!({
                            "annotation_id": a.id,
                            "start_line": a.start_line,
                            "end_line": a.end_line,
                            "start_byte": a.start_byte,
                            "end_byte": a.end_byte,
                            "selected_text": selected,
                            "text": note,
                            "stale": a.stale,
                        })
                    })
                    .collect();
                Ok(ToolResult::ok(json!({
                    "count": items.len(),
                    "annotations": items,
                })))
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}

// ─────────────── annotate_update ───────────────

pub fn annotate_update_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "annotate_update",
        "修改指定批注的文本内容（写入 AnnotationStore）",
        SideEffect::Reversible,
        ToolGroup::Annotation,
    )
}

pub fn annotate_update_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "document_id": {"type": "integer", "minimum": 1},
            "annotation_id": {"type": "integer", "minimum": 1, "description": "批注 id（annotate_list 返回）"},
            "text": {"type": "string", "minLength": 1, "description": "新的批注内容"}
        },
        "required": ["document_id", "annotation_id", "text"],
        "additionalProperties": false
    })
}

pub fn annotate_update_tool(ann: Arc<AnnotationService>) -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "annotate_update",
        "修改指定批注的文本内容（写入 AnnotationStore）",
        annotate_update_parameters(),
        boxed_invoke(move |args| {
            let ann = ann.clone();
            async move {
                let Some(doc_id) = parse_doc_id(&args) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"document_id"})));
                };
                let Some(id) = args.get("annotation_id").and_then(|v| v.as_u64()) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"annotation_id"})));
                };
                let Some(text) = args.get("text").and_then(|v| v.as_str()) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"text"})));
                };
                if text.is_empty() {
                    return Ok(ToolResult::err(json!({"error":"empty_text"})));
                }
                match ann.set_text(doc_id, id, text.to_string()).await {
                    Ok(true) => Ok(ToolResult::ok(json!({"annotation_id": id, "updated": true}))),
                    Ok(false) => Ok(ToolResult::err(json!({
                        "error": "not_found",
                        "annotation_id": id,
                    }))),
                    Err(e) => Ok(ToolResult::err(json!({"error": "update_failed", "message": e}))),
                }
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}

// ─────────────── annotate_delete ───────────────

pub fn annotate_delete_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "annotate_delete",
        "删除指定批注（从 AnnotationStore 移除）",
        SideEffect::Reversible,
        ToolGroup::Annotation,
    )
}

pub fn annotate_delete_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "document_id": {"type": "integer", "minimum": 1},
            "annotation_id": {"type": "integer", "minimum": 1, "description": "批注 id（annotate_list 返回）"}
        },
        "required": ["document_id", "annotation_id"],
        "additionalProperties": false
    })
}

pub fn annotate_delete_tool(ann: Arc<AnnotationService>) -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "annotate_delete",
        "删除指定批注（从 AnnotationStore 移除）",
        annotate_delete_parameters(),
        boxed_invoke(move |args| {
            let ann = ann.clone();
            async move {
                let Some(doc_id) = parse_doc_id(&args) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"document_id"})));
                };
                let Some(id) = args.get("annotation_id").and_then(|v| v.as_u64()) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"annotation_id"})));
                };
                match ann.remove(doc_id, id).await {
                    Ok(true) => Ok(ToolResult::ok(json!({"annotation_id": id, "deleted": true}))),
                    Ok(false) => Ok(ToolResult::err(json!({
                        "error": "not_found",
                        "annotation_id": id,
                    }))),
                    Err(e) => Ok(ToolResult::err(json!({"error": "delete_failed", "message": e}))),
                }
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}
