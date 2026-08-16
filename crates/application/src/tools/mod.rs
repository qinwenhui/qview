//! Agent 工具实现（架构文档 §7）。
//!
//! 每个工具通过 `LocalTool::from_async_fn` 注册到 `ToolRegistry`；
//! 工具的副作用级别由 `ToolMetadata` 标注。
//!
//! ## 公共规则（架构 §6.2）
//! 1. 稳定 `name`（≤ 64 字符，不与 `worker_finish` 重名）
//! 2. JSON Schema `parameters`（在构造时给出）
//! 3. 显式声明 `SideEffect`（`ToolMetadata.side_effect`）
//! 4. **不**直接接受 path；必须通过 `DocumentId` 走 `DocumentService`
//! 5. 输出分页 / 截断（按 `PermissionPolicy::max_read_lines`）
//! 6. 写工具 → GuardedTool（P4 落地）

pub mod annotation;
pub mod directory;
pub mod document;
pub mod export;
pub mod info;
pub mod intent_groups;
pub mod inspect;
pub mod progress;
pub mod read;
pub mod register;
pub mod search;
pub mod summarize;
pub mod system_info;
pub mod view;
pub mod viewport;

pub use annotation::{
    annotate_delete_metadata, annotate_delete_parameters, annotate_delete_tool,
    annotate_list_metadata, annotate_list_parameters, annotate_list_tool, annotate_metadata,
    annotate_parameters, annotate_tool, annotate_update_metadata, annotate_update_parameters,
    annotate_update_tool,
};
pub use directory::{
    list_directory_metadata, list_directory_parameters, list_directory_tool,
};
pub use document::{
    list_documents_metadata, list_documents_parameters, list_documents_tool, open_document_metadata,
    open_document_parameters, open_document_tool, write_document_metadata,
    write_document_parameters, write_document_tool,
};
pub use export::{export_metadata, export_parameters, export_tool};
pub use info::{info_metadata, info_parameters, info_tool};
pub use inspect::{inspect_metadata, inspect_parameters, inspect_tool};
pub use progress::{report_progress_metadata, report_progress_parameters, report_progress_tool};
pub use read::{read_metadata, read_parameters, read_tool};
pub use register::{register_defaults, ALL_TOOL_NAMES, ALL_TOOL_NAMES_WITH_WRITES};
pub use intent_groups::{tools_for, IntentKindTag, INTENT_TOOL_GROUPS};
pub use search::{search_metadata, search_parameters, search_tool};
pub use summarize::{summarize_metadata, summarize_parameters, summarize_tool};
pub use system_info::{system_info_metadata, system_info_parameters, system_info_tool};
pub use view::{
    clear_filter_metadata, clear_filter_parameters, clear_filter_tool, create_filter_metadata,
    create_filter_parameters, create_filter_tool, highlight_metadata, highlight_parameters,
    highlight_tool, navigate_metadata, navigate_parameters, navigate_tool, new_document_metadata,
    new_document_parameters, new_document_tool, open_panel_metadata, open_panel_parameters,
    open_panel_tool, switch_theme_metadata, switch_theme_parameters, switch_theme_tool,
    toggle_word_wrap_metadata, toggle_word_wrap_parameters, toggle_word_wrap_tool,
};
pub use viewport::{
    get_viewport_metadata, get_viewport_parameters, get_viewport_tool, SharedViewport,
};
