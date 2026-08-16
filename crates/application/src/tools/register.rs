//! 一站式注册：把所有工具塞进 `ToolRegistry`。
//!
//! 调用方：UI / CLI / 测试 fixture。

use std::sync::Arc;

use crate::service::annotation::AnnotationService;
use crate::service::document::DocumentService;
use crate::service::search::SearchService;
use crate::tool::registry::ToolRegistry;
use crate::tools::viewport::SharedViewport;
use crate::tools::{
    annotate_delete_metadata, annotate_delete_tool, annotate_list_metadata, annotate_list_tool,
    annotate_metadata, annotate_tool, annotate_update_metadata, annotate_update_tool,
    clear_filter_metadata, clear_filter_tool, create_filter_metadata, create_filter_tool,
    export_metadata, export_tool, get_viewport_metadata, get_viewport_tool, highlight_metadata,
    highlight_tool, info_metadata, info_tool, inspect_metadata, inspect_tool,
    list_directory_metadata, list_directory_tool, list_documents_metadata, list_documents_tool,
    navigate_metadata, navigate_tool, new_document_metadata, new_document_tool,
    open_document_metadata, open_document_tool, open_panel_metadata, open_panel_tool, read_metadata,
    read_tool, report_progress_metadata, report_progress_tool, search_metadata, search_tool,
    summarize_metadata, summarize_tool, switch_theme_metadata, switch_theme_tool,
    system_info_metadata, system_info_tool, toggle_word_wrap_metadata, toggle_word_wrap_tool,
    write_document_metadata, write_document_tool,
};

/// 把全部工具 + 元数据注册进 `ToolRegistry`。
///
/// - `viewport`：UI 发布的共享视口快照（`get_viewport` 用）。
/// - `ann` 可选：提供则注册批注工具 + 导出工具。
/// - `skip`：跳过不注册的工具名（典型用法：需要审批的写工具已由 GuardedTool
///   单独注入 `worker.instance_sources`，如 `export_report` / `write_document`）。
pub fn register_defaults(
    reg: &mut ToolRegistry,
    docs: Arc<DocumentService>,
    search: Arc<SearchService>,
    ann: Option<Arc<AnnotationService>>,
    viewport: SharedViewport,
    skip: &[&str],
) -> anyhow::Result<()> {
    macro_rules! reg_one {
        ($reg:expr, $ctor:expr, $meta:expr) => {
            if !skip.contains(&$meta.name.as_str()) {
                $reg.register($ctor?, $meta);
            }
        };
    }
    reg_one!(reg, info_tool(docs.clone()), info_metadata());
    reg_one!(reg, search_tool(search.clone()), search_metadata());
    reg_one!(reg, read_tool(docs.clone()), read_metadata());
    reg_one!(reg, inspect_tool(search.clone()), inspect_metadata());
    reg_one!(reg, summarize_tool(docs.clone()), summarize_metadata());
    reg_one!(reg, navigate_tool(), navigate_metadata());
    reg_one!(reg, highlight_tool(), highlight_metadata());
    reg_one!(reg, create_filter_tool(), create_filter_metadata());
    // ── 纯 UI / 设置操作（Control）──
    reg_one!(reg, clear_filter_tool(), clear_filter_metadata());
    reg_one!(reg, open_panel_tool(), open_panel_metadata());
    reg_one!(reg, toggle_word_wrap_tool(), toggle_word_wrap_metadata());
    reg_one!(reg, switch_theme_tool(), switch_theme_metadata());
    // ── 文档管理 ──
    reg_one!(reg, new_document_tool(), new_document_metadata());
    reg_one!(reg, list_documents_tool(docs.clone()), list_documents_metadata());
    reg_one!(reg, list_directory_tool(docs.clone()), list_directory_metadata());
    reg_one!(reg, open_document_tool(docs.clone()), open_document_metadata());
    reg_one!(reg, get_viewport_tool(viewport), get_viewport_metadata());
    // ── 项目经理控制（进度汇报，no-op；广播由 QviewSinkHook 拦截完成）──
    reg_one!(reg, report_progress_tool(), report_progress_metadata());
    // ── 系统信息（只读）──
    reg_one!(reg, system_info_tool(), system_info_metadata());
    reg_one!(reg, write_document_tool(docs.clone()), write_document_metadata());
    if let Some(ann) = ann {
        reg_one!(reg, annotate_tool(ann.clone()), annotate_metadata());
        reg_one!(reg, annotate_update_tool(ann.clone()), annotate_update_metadata());
        reg_one!(reg, annotate_delete_tool(ann.clone()), annotate_delete_metadata());
        reg_one!(reg, annotate_list_tool(ann.clone()), annotate_list_metadata());
        reg_one!(reg, export_tool(ann), export_metadata());
    }
    Ok(())
}

/// 只读 / 视图工具名（不含写工具）。用于构造 `PermissionPolicy::allow_tools`。
///
/// 写工具（annotate_* / export_report / write_document）单独列在
/// `ALL_TOOL_NAMES_WITH_WRITES`。
pub const ALL_TOOL_NAMES: &[&str] = &[
    "get_document_info",
    "search_text",
    "read_context",
    "inspect_matches",
    "summarize_range",
    "navigate_to_line",
    "highlight_range",
    "create_filter",
    "clear_filter",
    "open_panel",
    "toggle_word_wrap",
    "switch_theme",
    "new_document",
    "list_documents",
    "list_directory",
    "open_document",
    "get_viewport",
    "annotate_list",
    "report_progress",
    "system_info",
];

/// 全部工具名（含写工具）。调用方如果同时注册 GuardedTool 包装的写工具，
/// 可以用这个常量构造 allowlist。
pub const ALL_TOOL_NAMES_WITH_WRITES: &[&str] = &[
    "get_document_info",
    "search_text",
    "read_context",
    "inspect_matches",
    "summarize_range",
    "navigate_to_line",
    "highlight_range",
    "create_filter",
    "clear_filter",
    "open_panel",
    "toggle_word_wrap",
    "switch_theme",
    "new_document",
    "list_documents",
    "list_directory",
    "open_document",
    "get_viewport",
    "annotate_list",
    "report_progress",
    "system_info",
    "annotate_create",
    "annotate_update",
    "annotate_delete",
    "export_report",
    "write_document",
];
