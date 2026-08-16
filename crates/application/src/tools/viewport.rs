//! `get_viewport` 工具：读取主视图当前可见行 / 选区。
//!
//! 数据源是 UI 每帧发布的共享 `Arc<parking_lot::Mutex<Option<ViewportSnapshot>>>`
//! （`application::protocol::ViewportSnapshot`）。这是唯一「UI → Agent」通道。

use std::sync::Arc;

use futures::future::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Value};

use contexa_tools::{boxed_invoke, LocalTool, ToolResult};

use crate::protocol::ViewportSnapshot;
use crate::protocol::SideEffect;
use crate::tool::metadata::{ToolGroup, ToolMetadata};

/// 共享视口状态（UI 写、工具读）。
pub type SharedViewport = Arc<Mutex<Option<ViewportSnapshot>>>;

pub fn get_viewport_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "get_viewport",
        "读取主视图当前可见行范围与选中范围（感知用户在看哪）",
        SideEffect::ReadOnly,
        ToolGroup::Search,
    )
}

pub fn get_viewport_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "document_id": {"type": "integer", "minimum": 1, "description": "可选；不校验，返回当前全局视口"}
        },
        "required": [],
        "additionalProperties": false
    })
}

pub fn get_viewport_tool(shared: SharedViewport) -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "get_viewport",
        "读取主视图当前可见行范围与选中范围（感知用户在看哪）",
        get_viewport_parameters(),
        boxed_invoke(move |_args| {
            let shared = shared.clone();
            async move {
                let snap = shared.lock().clone();
                match snap {
                    Some(v) => Ok(ToolResult::ok(json!({
                        "has_viewport": true,
                        "first_visible_line": v.first_visible_line,
                        "last_visible_line": v.last_visible_line,
                        "visible_lines": v.last_visible_line.saturating_sub(v.first_visible_line) + 1,
                        "selection": v.selection.map(|(s, e)| json!({
                            "start_line": s,
                            "end_line": e,
                        })).unwrap_or(json!(null)),
                    }))),
                    None => Ok(ToolResult::ok(json!({
                        "has_viewport": false,
                        "message": "主视图尚未发布视口信息（可能未打开文件）",
                    }))),
                }
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}
