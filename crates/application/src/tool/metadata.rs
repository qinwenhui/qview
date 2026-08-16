//! 工具元数据（架构文档 §6.2）。

use serde::{Deserialize, Serialize};

use crate::protocol::SideEffect;

/// 单条工具的元数据（不进入 `ToolSpec`，仅在 qview 内部使用）。
///
/// - `name` 与 `ToolSpec.name` 对齐。
/// - `side_effect` 用于 UI 分级显示 + 权限策略。
/// - `group` 用于 UI 分组（如 `document` / `search` / `view`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMetadata {
    /// 工具名（与 `LocalTool::name()` 对齐）。
    pub name: String,
    /// 一句话简介（UI 列表展示）。
    pub summary: String,
    /// 副作用级别。
    pub side_effect: SideEffect,
    /// 工具分组（UI 折叠 / 过滤）。
    pub group: ToolGroup,
}

/// 工具分组。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolGroup {
    /// 文档元信息 / 列出。
    Document,
    /// 搜索 / 上下文 / 检视 / 总结。
    Search,
    /// 视图跳转 / 高亮 / 过滤（不影响数据）。
    View,
    /// 纯 UI / 设置操作（切主题 / 换行 / 开面板 / 清过滤）。
    Control,
    /// 批注 / 标记（可撤销）。
    Annotation,
    /// 导出 / 报告。
    Export,
    /// 系统信息（OS / 内存 / CPU / 磁盘 / 网络）。
    System,
}

impl ToolGroup {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolGroup::Document => "document",
            ToolGroup::Search => "search",
            ToolGroup::View => "view",
            ToolGroup::Control => "control",
            ToolGroup::Annotation => "annotation",
            ToolGroup::Export => "export",
            ToolGroup::System => "system",
        }
    }
}

impl ToolMetadata {
    /// 构造元数据。`name` 必须与对应 `LocalTool::name()` 保持一致。
    pub fn new(
        name: impl Into<String>,
        summary: impl Into<String>,
        side_effect: SideEffect,
        group: ToolGroup,
    ) -> Self {
        Self {
            name: name.into(),
            summary: summary.into(),
            side_effect,
            group,
        }
    }
}
