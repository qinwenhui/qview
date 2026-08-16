//! View 类工具：navigate_to_line / highlight_range / create_filter。
//!
//! 这三个工具**只**改变 Agent 视图的状态，不影响主文档。
//! 它们通过 `ToolResult.content["view_intents"]` 字段发出 ViewIntent，
//! 由 `qview-agent::QviewSinkHook::post_tool_call` 解析并广播为
//! `AgentEvent::ViewIntentEmitted`。
//!
//! 设计选择：工具不接收 DocumentService 句柄，因为它们**不**读文档内容；
//! 只接受行号 / pattern 等纯参数。失败的 ViewIntent 由 UI 忽略（架构 §9.1）。

use futures::future::FutureExt;
use serde_json::{json, Value};

use contexa_tools::{boxed_invoke, LocalTool, ToolResult};

use crate::protocol::view_intent::{FilterSpec, HighlightKind, PanelKind, ViewIntent};
use crate::protocol::SideEffect;
use crate::tool::metadata::{ToolGroup, ToolMetadata};

// ─────────────── navigate_to_line ───────────────

/// 工具元数据。
pub fn navigate_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "navigate_to_line",
        "跳转到指定行（发出 ViewIntent::FocusLine）",
        SideEffect::ViewOnly,
        ToolGroup::View,
    )
}

pub fn navigate_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "line": {"type": "integer", "minimum": 0}
        },
        "required": ["line"],
        "additionalProperties": false
    })
}

pub fn navigate_tool() -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "navigate_to_line",
        "跳转到指定行（发出 ViewIntent::FocusLine）",
        navigate_parameters(),
        boxed_invoke(|args| {
            async move {
                let Some(line) = args.get("line").and_then(|v| v.as_u64()) else {
                    return Ok(ToolResult::err(json!({
                        "error": "missing_argument",
                        "argument": "line"
                    })));
                };
                Ok(view_intent_result(ViewIntent::FocusLine { line }))
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}

// ─────────────── highlight_range ───────────────

pub fn highlight_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "highlight_range",
        "在 [start, end] 行号范围内加高亮（ViewIntent::HighlightRange）",
        SideEffect::ViewOnly,
        ToolGroup::View,
    )
}

pub fn highlight_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "start": {"type": "integer", "minimum": 0},
            "end":   {"type": "integer", "minimum": 0},
            "kind": {
                "type": "string",
                "enum": ["agent_focus", "agent_match", "agent_warning", "annotation"],
                "default": "agent_match"
            }
        },
        "required": ["start", "end"],
        "additionalProperties": false
    })
}

pub fn highlight_tool() -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "highlight_range",
        "在 [start, end] 行号范围内加高亮（ViewIntent::HighlightRange）",
        highlight_parameters(),
        boxed_invoke(|args| {
            async move {
                let Some(start) = args.get("start").and_then(|v| v.as_u64()) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"start"})));
                };
                let Some(end) = args.get("end").and_then(|v| v.as_u64()) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"end"})));
                };
                let kind_str = args
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("agent_match");
                let kind = match kind_str {
                    "agent_focus" => HighlightKind::AgentFocus,
                    "agent_warning" => HighlightKind::AgentWarning,
                    "annotation" => HighlightKind::Annotation,
                    _ => HighlightKind::AgentMatch,
                };
                if end < start {
                    return Ok(ToolResult::err(json!({"error":"invalid_range"})));
                }
                Ok(view_intent_result(ViewIntent::HighlightRange { start, end, kind }))
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}

// ─────────────── create_filter ───────────────

pub fn create_filter_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "create_filter",
        "为 Agent 视图应用一个临时过滤器（ViewIntent::ApplyFilter）",
        SideEffect::ViewOnly,
        ToolGroup::View,
    )
}

pub fn create_filter_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "type": {
                "type": "string",
                "enum": ["literal", "error_level", "contains"],
                "description": "过滤器类型"
            },
            "pattern": {"type": "string", "description": "literal / contains 用"},
            "case_sensitive": {"type": "boolean", "default": false},
            "min": {"type": "integer", "minimum": 0, "maximum": 599, "description": "error_level 用"},
            "max": {"type": "integer", "minimum": 0, "maximum": 599, "description": "error_level 用"},
            "needle": {"type": "string", "description": "contains 用"}
        },
        "required": ["type"],
        "additionalProperties": false
    })
}

pub fn create_filter_tool() -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "create_filter",
        "为 Agent 视图应用一个临时过滤器（ViewIntent::ApplyFilter）",
        create_filter_parameters(),
        boxed_invoke(|args| {
            async move {
                let Some(kind) = args.get("type").and_then(|v| v.as_str()) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"type"})));
                };
                let spec = match kind {
                    "literal" => {
                        let pattern = match args.get("pattern").and_then(|v| v.as_str()) {
                            Some(p) => p.to_string(),
                            None => {
                                return Ok(ToolResult::err(json!({
                                    "error": "missing_argument",
                                    "argument": "pattern"
                                })));
                            }
                        };
                        let case = args
                            .get("case_sensitive")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        FilterSpec::Literal { pattern, case_sensitive: case }
                    }
                    "error_level" => {
                        let min = args.get("min").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                        let max = args.get("max").and_then(|v| v.as_u64()).unwrap_or(599) as u16;
                        if min > max {
                            return Ok(ToolResult::err(json!({"error":"invalid_error_level"})));
                        }
                        FilterSpec::ErrorLevel { min, max }
                    }
                    "contains" => {
                        let needle = match args.get("needle").and_then(|v| v.as_str()) {
                            Some(n) => n.to_string(),
                            None => {
                                return Ok(ToolResult::err(json!({
                                    "error": "missing_argument",
                                    "argument": "needle"
                                })));
                            }
                        };
                        FilterSpec::Contains { needle }
                    }
                    other => {
                        return Ok(ToolResult::err(json!({
                            "error": "unknown_filter_type",
                            "type": other
                        })));
                    }
                };
                Ok(view_intent_result(ViewIntent::ApplyFilter { filter: spec }))
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}

// ─────────────── clear_filter ───────────────

pub fn clear_filter_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "clear_filter",
        "清除 Agent 视图的临时过滤器（ViewIntent::ClearFilter）",
        SideEffect::ViewOnly,
        ToolGroup::Control,
    )
}

pub fn clear_filter_parameters() -> Value {
    json!({"type":"object","properties":{},"additionalProperties":false})
}

pub fn clear_filter_tool() -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "clear_filter",
        "清除 Agent 视图的临时过滤器（ViewIntent::ClearFilter）",
        clear_filter_parameters(),
        boxed_invoke(|_| {
            async move { Ok(view_intent_result(ViewIntent::ClearFilter)) }.boxed()
        }),
    )?;
    Ok(tool)
}

// ─────────────── open_panel ───────────────

pub fn open_panel_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "open_panel",
        "打开一个面板（批注列表 / 过滤器）（ViewIntent::OpenPanel）",
        SideEffect::ViewOnly,
        ToolGroup::Control,
    )
}

pub fn open_panel_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "panel": {
                "type": "string",
                "enum": ["agent", "annotation", "filter"],
                "description": "要打开的面板"
            }
        },
        "required": ["panel"],
        "additionalProperties": false
    })
}

pub fn open_panel_tool() -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "open_panel",
        "打开一个面板（批注列表 / 过滤器）（ViewIntent::OpenPanel）",
        open_panel_parameters(),
        boxed_invoke(|args| {
            async move {
                let Some(kind) = args.get("panel").and_then(|v| v.as_str()) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"panel"})));
                };
                let panel = match kind {
                    "annotation" => PanelKind::Annotation,
                    "filter" => PanelKind::Filter,
                    _ => PanelKind::Agent,
                };
                Ok(view_intent_result(ViewIntent::OpenPanel { panel }))
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}

// ─────────────── toggle_word_wrap ───────────────

pub fn toggle_word_wrap_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "toggle_word_wrap",
        "开启 / 关闭自动换行（ViewIntent::ToggleWordWrap）",
        SideEffect::ViewOnly,
        ToolGroup::Control,
    )
}

pub fn toggle_word_wrap_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "enabled": {"type": "boolean", "description": "true=开换行, false=关"}
        },
        "required": ["enabled"],
        "additionalProperties": false
    })
}

pub fn toggle_word_wrap_tool() -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "toggle_word_wrap",
        "开启 / 关闭自动换行（ViewIntent::ToggleWordWrap）",
        toggle_word_wrap_parameters(),
        boxed_invoke(|args| {
            async move {
                let Some(enabled) = args.get("enabled").and_then(|v| v.as_bool()) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"enabled"})));
                };
                Ok(view_intent_result(ViewIntent::ToggleWordWrap { enabled }))
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}

// ─────────────── switch_theme ───────────────

pub fn switch_theme_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "switch_theme",
        "切换主题（按名称前缀匹配，如 dracula / dark pro）（ViewIntent::SwitchTheme）",
        SideEffect::ViewOnly,
        ToolGroup::Control,
    )
}

pub fn switch_theme_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "theme": {"type": "string", "description": "主题名（前缀匹配，不区分大小写）"}
        },
        "required": ["theme"],
        "additionalProperties": false
    })
}

pub fn switch_theme_tool() -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "switch_theme",
        "切换主题（按名称前缀匹配，如 dracula / dark pro）（ViewIntent::SwitchTheme）",
        switch_theme_parameters(),
        boxed_invoke(|args| {
            async move {
                let Some(theme) = args.get("theme").and_then(|v| v.as_str()) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"theme"})));
                };
                Ok(view_intent_result(ViewIntent::SwitchTheme { theme: theme.to_string() }))
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}

// ─────────────── new_document ───────────────

pub fn new_document_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "new_document",
        "新建空白文档（ViewIntent::NewDocument，UI 点击后创建）",
        SideEffect::ViewOnly,
        ToolGroup::Document,
    )
}

pub fn new_document_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {"type": "string", "default": "未命名.txt", "description": "建议的文件名"}
        },
        "required": [],
        "additionalProperties": false
    })
}

pub fn new_document_tool() -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "new_document",
        "新建空白文档（ViewIntent::NewDocument，UI 点击后创建）",
        new_document_parameters(),
        boxed_invoke(|args| {
            async move {
                let name = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("未命名.txt")
                    .to_string();
                Ok(view_intent_result(ViewIntent::NewDocument { name }))
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}

// ─────────────── 公共 helper ───────────────

/// 把单个 ViewIntent 包成 ToolResult，content 里附带 `view_intents` 数组
/// 供 `QviewSinkHook::post_tool_call` 解析。
fn view_intent_result(intent: ViewIntent) -> ToolResult {
    let intent_json = serde_json::to_value(&intent).unwrap_or(json!({}));
    ToolResult::ok(json!({
        "applied": true,
        "view_intents": [intent_json],
    }))
}
