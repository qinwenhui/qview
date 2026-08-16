//! 按意图分组的"最小可用工具集"（架构 §22.x — P1「意图层」落地）。
//!
//! ## 为什么按意图分组
//!
//! qview 总共 23 个工具，全量塞给 LLM 的 `tools_specs` 约 6-8K tokens。
//! 多数场景（打开文件、查日志、出报告）只需要 3-5 个工具，其余是噪声：
//!
//! - LLM 决策时间变长（schema 多 → 思考路径多）
//! - 误调用概率上升（"批注列表"和"批注创建"schema 接近，容易混淆）
//! - 提示词更可能被工具描述干扰
//!
//! v1 router 命中意图后只推本组的工具 schema，省 token、提速度。
//!
//! ## 维护规则
//!
//! - **`Unknown` 走全集**：`tools_for(IntentKindTag::Unknown)` 返回所有工具名。
//! - **`Chat` 不推任何工具**：闲聊不进 ReAct，工具集必须空。
//! - **每组工具必须自包含**：能独立完成该意图对应的最小任务流。
//! - **不要把写工具和读工具混进一组**：写工具（annotate_* / export_report /
//!   `write_document`）走 GuardedTool 单独注入；router 推名字 OK（schema 还是要
//!   告诉 LLM 这个能力存在），但运行时调用会被 GuardedTool 拦截审批。
//!
//! ## 与 `qview_agent::intent::IntentKind` 的关系
//!
//! 本模块定义了**本地** `IntentKindTag`（application crate 是 agent 的依赖，
//! 不能反向引用）。`qview_agent::intent::IntentRouter` 在调 [`tools_for`] 之前
//! 把自己的 `IntentKind` 映射到 `IntentKindTag`（见 `agent/src/intent.rs`）。

#[allow(unused_imports)]
use crate::tools::register::{ALL_TOOL_NAMES, ALL_TOOL_NAMES_WITH_WRITES};

/// 意图标签（与 `qview_agent::intent::IntentKind` 一一对应，但独立定义以避免循环依赖）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntentKindTag {
    Chat,
    OpenFile,
    SearchLog,
    ReadContext,
    AnnotateFile,
    EditFile,
    ExportReport,
    ListDir,
    ListAnnotations,
    NavigateToLine,
    ConfigureAgent,
    SystemInfo,
    Unknown,
}

/// 意图 → 工具 schema 子集。
///
/// `&'static [&'static str]` 写法让 router 可以零拷贝地构造 `Intent::suggested_tools`。
pub const INTENT_TOOL_GROUPS: &[(IntentKindTag, &[&str])] = &[
    (
        IntentKindTag::Chat,
        &[], // 闲聊不进 ReAct，必须空
    ),
    (
        IntentKindTag::OpenFile,
        &[
            "open_document",
            "get_document_info",
            "list_documents",
        ],
    ),
    (
        IntentKindTag::ListDir,
        &[
            "list_directory",
            "list_documents",
            "open_document",
        ],
    ),
    (
        IntentKindTag::SearchLog,
        &[
            // 查日志 + 出报告的最小闭环
            "search_text",
            "read_context",
            "inspect_matches",
            "summarize_range",
            "get_document_info",
            "export_report", // 写工具走 GuardedTool，router 推名字 OK
        ],
    ),
    (
        IntentKindTag::ReadContext,
        &[
            "read_context",
            "get_document_info",
            "navigate_to_line",
            "get_viewport",
        ],
    ),
    (
        IntentKindTag::AnnotateFile,
        &[
            "read_context",
            "get_document_info",
            "annotate_list",
            "annotate_create", // 写工具
        ],
    ),
    (
        IntentKindTag::EditFile,
        &[
            "read_context",
            "get_document_info",
            "write_document", // 写工具
        ],
    ),
    (
        IntentKindTag::ExportReport,
        &[
            "get_document_info",
            "read_context",
            "summarize_range",
            "export_report", // 写工具
        ],
    ),
    (
        IntentKindTag::ListAnnotations,
        &[
            "annotate_list",
            "get_document_info",
        ],
    ),
    (
        IntentKindTag::NavigateToLine,
        &[
            "navigate_to_line",
            "read_context",
            "annotate_list",
            "get_document_info",
            "get_viewport",
        ],
    ),
    (
        IntentKindTag::ConfigureAgent,
        &[], // 配置类由 GUI 处理，不调 LLM
    ),
    (
        IntentKindTag::SystemInfo,
        &["system_info"],
    ),
    (
        IntentKindTag::Unknown,
        ALL_TOOL_NAMES_WITH_WRITES, // 兜底全集
    ),
];

/// 便捷查询：`IntentKindTag` → 工具名切片。
pub fn tools_for(kind: IntentKindTag) -> Vec<&'static str> {
    INTENT_TOOL_GROUPS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, tools)| tools.to_vec())
        .unwrap_or_else(|| ALL_TOOL_NAMES_WITH_WRITES.to_vec())
}

/// 反向：`IntentKindTag` → 该意图对应的工具名数量（调试用）。
pub fn tool_count(kind: IntentKindTag) -> usize {
    tools_for(kind).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_group_is_empty() {
        assert!(tools_for(IntentKindTag::Chat).is_empty());
    }

    #[test]
    fn configure_group_is_empty() {
        assert!(tools_for(IntentKindTag::ConfigureAgent).is_empty());
    }

    #[test]
    fn unknown_group_is_full_set() {
        let tools = tools_for(IntentKindTag::Unknown);
        assert_eq!(tools.len(), ALL_TOOL_NAMES_WITH_WRITES.len());
    }

    #[test]
    fn search_log_includes_export() {
        let tools = tools_for(IntentKindTag::SearchLog);
        assert!(tools.contains(&"search_text"));
        assert!(tools.contains(&"export_report"));
    }

    #[test]
    fn navigate_to_line_group_has_jump_tools() {
        let tools = tools_for(IntentKindTag::NavigateToLine);
        assert!(tools.contains(&"navigate_to_line"));
        assert!(tools.contains(&"read_context"));
        assert!(tools.contains(&"annotate_list"));
        // 跳转是读操作，不推写工具
        assert!(!tools.contains(&"write_document"));
    }

    #[test]
    fn all_intents_have_a_group() {
        let all_kinds = [
            IntentKindTag::Chat,
            IntentKindTag::OpenFile,
            IntentKindTag::SearchLog,
            IntentKindTag::ReadContext,
            IntentKindTag::AnnotateFile,
            IntentKindTag::EditFile,
            IntentKindTag::ExportReport,
            IntentKindTag::ListDir,
            IntentKindTag::ListAnnotations,
            IntentKindTag::NavigateToLine,
            IntentKindTag::ConfigureAgent,
            IntentKindTag::SystemInfo,
            IntentKindTag::Unknown,
        ];
        for k in all_kinds {
            let _ = tools_for(k); // 不 panic = 有映射
        }
    }

    #[test]
    fn no_group_includes_invalid_tool_names() {
        for (kind, tools) in INTENT_TOOL_GROUPS {
            for t in *tools {
                assert!(
                    ALL_TOOL_NAMES_WITH_WRITES.contains(t) || ALL_TOOL_NAMES.contains(t),
                    "{:?} 引用了不存在的工具 {}",
                    kind,
                    t
                );
            }
        }
    }
}
