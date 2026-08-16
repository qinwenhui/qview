//! `report_progress` 工具：项目经理向用户**实时**交代中间进度。
//!
//! 执行中 LLM 的普通文本不会实时显示，只有调本工具才能让用户看到进度。
//! 工具本身是 no-op：真正的动作发生在 `QviewSinkHook::on_tool_call` —— 它拦到
//! `report_progress` 后从 `args["message"]` 读一句话，广播 `AgentEvent::ProgressNote`，
//! 并**提前 return**（不进 in_flight 队列 / 不产生工具气泡）。

use futures::future::FutureExt;
use serde_json::{json, Value};

use contexa_tools::{boxed_invoke, LocalTool, ToolResult};

use crate::protocol::SideEffect;
use crate::tool::metadata::{ToolGroup, ToolMetadata};

/// 工具元数据。
pub fn report_progress_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "report_progress",
        "向用户实时交代中间进度（message 填一句话）。执行中普通文本不实时显示，必须调本工具才能让用户看到进度。",
        SideEffect::ReadOnly,
        ToolGroup::Control,
    )
}

/// 工具入参 JSON Schema。
pub fn report_progress_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "message": {
                "type": "string",
                "description": "一句话交代当前进度（如「正在读 prod.log 前 200 行」）"
            }
        },
        "required": ["message"],
        "additionalProperties": false
    })
}

/// 构造工具（no-op；进度广播由 QviewSinkHook 拦截完成）。
pub fn report_progress_tool() -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "report_progress",
        "向用户实时交代中间进度（message 填一句话）。执行中普通文本不实时显示，必须调本工具才能让用户看到进度。",
        report_progress_parameters(),
        boxed_invoke(|_args| {
            async move { Ok(ToolResult::ok(json!({"status": "ok"}))) }.boxed()
        }),
    )?;
    Ok(tool)
}
