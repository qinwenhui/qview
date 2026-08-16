//! 文档管理工具：`list_documents` / `open_document` / `write_document`。
//!
//! - `list_documents`：列出已注册文档（读 `DocumentService::list_paths`）。
//! - `open_document`：把文件注册进 DocumentService（幂等），返回 doc_id，
//!   并发 `ViewIntent::OpenDocument`（UI 收到后**自动切换主视图**）。
//! - `write_document`：写文本到文件（新建 / 覆写，**需审批**），写后注册 +
//!   发 `ViewIntent::OpenDocument`。这是「保存文件」类操作，走 GuardedTool。

use std::sync::Arc;

use futures::future::FutureExt;
use serde_json::{json, Value};

use contexa_tools::{boxed_invoke, LocalTool, ToolResult};

use crate::protocol::view_intent::ViewIntent;
use crate::protocol::SideEffect;
use crate::service::document::DocumentService;
use crate::tool::metadata::{ToolGroup, ToolMetadata};

// ─────────────── list_documents ───────────────

pub fn list_documents_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "list_documents",
        "列出当前已注册（已打开）的文档：id + 路径",
        SideEffect::ReadOnly,
        ToolGroup::Document,
    )
}

pub fn list_documents_parameters() -> Value {
    json!({"type":"object","properties":{},"additionalProperties":false})
}

pub fn list_documents_tool(docs: Arc<DocumentService>) -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "list_documents",
        "列出当前已注册（已打开）的文档：id + 路径",
        list_documents_parameters(),
        boxed_invoke(move |_| {
            let docs = docs.clone();
            async move {
                let items: Vec<Value> = docs
                    .list_paths()
                    .into_iter()
                    .map(|(id, p)| json!({"document_id": id.get(), "path": p.display().to_string()}))
                    .collect();
                Ok(ToolResult::ok(json!({
                    "count": items.len(),
                    "documents": items,
                })))
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}

// ─────────────── open_document ───────────────

pub fn open_document_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "open_document",
        "打开一个文件：注册进 DocumentService 返回 document_id，并发 ViewIntent 让 UI 切换到该文件（同路径重复调用幂等，返回相同 document_id）",
        SideEffect::ViewOnly,
        ToolGroup::Document,
    )
}

pub fn open_document_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "要打开的文件的绝对路径"}
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

pub fn open_document_tool(docs: Arc<DocumentService>) -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "open_document",
        "打开一个文件：注册进 DocumentService 返回 document_id，并发 ViewIntent 让 UI 切换到该文件（同路径重复调用幂等，返回相同 document_id）",
        open_document_parameters(),
        boxed_invoke(move |args| {
            let docs = docs.clone();
            async move {
                let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"path"})));
                };
                if let Some(rule) = docs.is_blocked(std::path::Path::new(path)) {
                    return Ok(ToolResult::err(json!({
                        "error": "path_blocked",
                        "path": path,
                        "rule": rule,
                        "message": format!("系统目录黑名单：{path}（命中规则 {rule}），器灵不允许打开"),
                    })));
                }
                // 幂等：同路径已在文档列表 → already_open=true，不重复创建 Engine / 索引。
                let already_open = docs.lookup(std::path::Path::new(path)).is_some();
                match docs.open(std::path::PathBuf::from(path)) {
                    Ok(id) => Ok(tool_result_with_intent(json!({
                        "document_id": id.get(),
                        "path": path,
                        "opened": true,
                        "already_open": already_open,
                    }), ViewIntent::OpenDocument { path: path.to_string() })),
                    Err(e) => Ok(ToolResult::err(json!({
                        "error": "open_failed",
                        "path": path,
                        "message": format!("{e:#}"),
                    }))),
                }
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}

// ─────────────── write_document（需审批）───────────────

pub fn write_document_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "write_document",
        "写文本到文件（新建 / 覆写，**需用户审批**）；写后注册并让 UI 打开",
        SideEffect::Mutating,
        ToolGroup::Document,
    )
}

pub fn write_document_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "目标文件绝对路径（不存在则创建）"},
            "text": {"type": "string", "description": "要写入的完整文本内容"}
        },
        "required": ["path", "text"],
        "additionalProperties": false
    })
}

pub fn write_document_tool(docs: Arc<DocumentService>) -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "write_document",
        "写文本到文件（新建 / 覆写，**需用户审批**）；写后注册并让 UI 打开",
        write_document_parameters(),
        boxed_invoke(move |args| {
            let docs = docs.clone();
            async move {
                let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"path"})));
                };
                let Some(text) = args.get("text").and_then(|v| v.as_str()) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"text"})));
                };
                let pb = std::path::PathBuf::from(path);
                // 系统目录黑名单：写工具同样禁止落在系统关键路径（防御纵深，审批之外再加一道）。
                if let Some(rule) = docs.is_blocked(&pb) {
                    return Ok(ToolResult::err(json!({
                        "error": "path_blocked",
                        "path": path,
                        "rule": rule,
                        "message": format!("系统目录黑名单：{path}（命中规则 {rule}），器灵不允许写入"),
                    })));
                }
                // 父目录不存在则创建
                if let Some(parent) = pb.parent() {
                    if !parent.as_os_str().is_empty() && !parent.exists() {
                        if let Err(e) = std::fs::create_dir_all(parent) {
                            return Ok(ToolResult::err(json!({
                                "error": "write_failed",
                                "path": path,
                                "message": format!("创建目录失败: {e}"),
                            })));
                        }
                    }
                }
                if let Err(e) = std::fs::write(&pb, text) {
                    return Ok(ToolResult::err(json!({
                        "error": "write_failed",
                        "path": path,
                        "message": format!("{e}"),
                    })));
                }
                let bytes = text.len() as u64;
                let id = docs.open(pb).map_err(|e| e.to_string()).ok();
                let mut payload = json!({
                    "path": path,
                    "written": true,
                    "bytes": bytes,
                });
                if let Some(id) = id {
                    payload["document_id"] = json!(id.get());
                }
                Ok(tool_result_with_intent(payload, ViewIntent::OpenDocument { path: path.to_string() }))
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}

/// 复用 view.rs 的 helper 语义：内容 + `view_intents`。
fn tool_result_with_intent(payload: Value, intent: ViewIntent) -> ToolResult {
    let intent_json = serde_json::to_value(&intent).unwrap_or(json!({}));
    let mut content = payload;
    if let Some(obj) = content.as_object_mut() {
        obj.insert("view_intents".into(), json!([intent_json]));
    }
    ToolResult::ok(content)
}
