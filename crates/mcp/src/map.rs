//! MCP 协议映射（架构 §12.4）。
//!
//! - SideEffect ↔ MCP annotations
//! - ToolSpec → MCP `Tool` 描述
//! - ToolResult → MCP `CallToolResult`

use serde_json::{json, Value};

use qview_application::protocol::SideEffect;
use qview_application::tool::metadata::ToolMetadata;

use contexa_context::ToolSpec;

/// SideEffect → MCP annotations。
pub fn annotations(side: SideEffect) -> Value {
    match side {
        SideEffect::ReadOnly => json!({"readOnlyHint": true}),
        SideEffect::ViewOnly => json!({"readOnlyHint": true}),
        SideEffect::Reversible => json!({"readOnlyHint": false}),
        SideEffect::Mutating => json!({"destructiveHint": true}),
        SideEffect::Destructive => json!({"destructiveHint": true}),
    }
}

/// ToolSpec + ToolMetadata → MCP `Tool`。
///
/// MCP `Tool` 结构（来自 MCP 协议 2024-11-25）：
/// ```json
/// {
///   "name": "...",
///   "description": "...",
///   "inputSchema": { ... JSON Schema ... },
///   "annotations": { ... }
/// }
/// ```
pub fn to_mcp_tool(spec: &ToolSpec, meta: &ToolMetadata) -> Value {
    json!({
        "name": spec.name,
        "description": spec.description,
        "inputSchema": spec.parameters,
        "annotations": annotations(meta.side_effect),
    })
}

/// MCP `Tool` → ToolSpec（反向，client 端用）。
///
/// MCP 工具可能携带 annotations；qview 端根据 annotations 决定 SideEffect：
/// - readOnlyHint = true → ReadOnly
/// - destructiveHint = true → Mutating
/// - 其他 → Reversible（保守估计）
pub fn from_mcp_tool(value: &Value) -> Option<(ToolSpec, SideEffect)> {
    let name = value.get("name")?.as_str()?.to_string();
    let description = value.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let input_schema = value.get("inputSchema").cloned().unwrap_or(json!({"type": "object"}));
    let annotations = value.get("annotations").cloned().unwrap_or(json!({}));
    let side = if annotations.get("readOnlyHint").and_then(|v| v.as_bool()).unwrap_or(false) {
        SideEffect::ReadOnly
    } else if annotations.get("destructiveHint").and_then(|v| v.as_bool()).unwrap_or(false) {
        SideEffect::Mutating
    } else {
        SideEffect::Reversible
    };
    Some((
        ToolSpec::new_unchecked(name, description, input_schema),
        side,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use qview_application::tool::metadata::ToolGroup;
    use serde_json::json;

    #[test]
    fn annotations_round_trip() {
        for v in [
            SideEffect::ReadOnly,
            SideEffect::ViewOnly,
            SideEffect::Reversible,
            SideEffect::Mutating,
            SideEffect::Destructive,
        ] {
            let a = annotations(v);
            // 至少有一个 hint
            assert!(a.get("readOnlyHint").is_some() || a.get("destructiveHint").is_some());
        }
    }

    #[test]
    fn to_mcp_tool_includes_annotations() {
        let spec = ToolSpec::new_unchecked("search_text", "x", json!({"type": "object"}));
        let meta = ToolMetadata::new(
            "search_text",
            "x",
            SideEffect::ReadOnly,
            ToolGroup::Search,
        );
        let mcp = to_mcp_tool(&spec, &meta);
        assert_eq!(mcp["name"], "search_text");
        assert_eq!(mcp["annotations"]["readOnlyHint"], true);
    }

    #[test]
    fn from_mcp_tool_readonly() {
        let v = json!({
            "name": "x",
            "description": "d",
            "inputSchema": {"type": "object"},
            "annotations": {"readOnlyHint": true}
        });
        let (spec, side) = from_mcp_tool(&v).unwrap();
        assert_eq!(spec.name, "x");
        assert_eq!(side, SideEffect::ReadOnly);
    }

    #[test]
    fn from_mcp_tool_destructive() {
        let v = json!({
            "name": "delete",
            "annotations": {"destructiveHint": true}
        });
        let (_, side) = from_mcp_tool(&v).unwrap();
        assert_eq!(side, SideEffect::Mutating);
    }
}
