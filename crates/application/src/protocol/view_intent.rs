//! 视图意图（架构文档 §6.1 / §9）。
//!
//! UI 投影层动作；不影响数据。Agent 通过 ViewIntent 告诉 UI 该怎么做，
//! 但 UI 可以忽略（失败的 ViewIntent 不影响 Agent 任务）。

use serde::{Deserialize, Serialize};

/// 单条 ViewIntent。变体稳定；新增变体必须同步更新架构文档。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "intent", rename_all = "snake_case")]
pub enum ViewIntent {
    /// 跳转到指定行（不滚到中心，仅在屏外则滚到屏内）。
    FocusLine { line: u64 },

    /// 高亮一段行范围 + 语义类别。
    HighlightRange {
        start: u64,
        end: u64,
        kind: HighlightKind,
    },

    /// 打开一个面板（Agent / Annotation / Filter）。
    OpenPanel { panel: PanelKind },

    /// 显示一次性消息（气泡 / Toast）。
    ShowMessage { level: MessageLevel, text: String },

    /// 应用一个临时过滤器（Agent 视图专用，不影响人类视图）。
    ApplyFilter { filter: FilterSpec },

    /// 打开一个文件（切到主视图；**点击应用**，不自动抢用户视图）。
    OpenDocument { path: String },

    /// 新建空白文档（**点击应用**）。
    NewDocument { name: String },

    /// 清除 Agent 视图过滤器。
    ClearFilter,

    /// 切换自动换行。
    ToggleWordWrap { enabled: bool },

    /// 切换主题（按名称前缀匹配，如 "dracula" / "dark pro"）。
    SwitchTheme { theme: String },
}

/// 高亮语义类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HighlightKind {
    /// Agent 当前正在关注的范围（动态）。
    AgentFocus,
    /// Agent 搜索 / 匹配命中的范围（静态参考）。
    AgentMatch,
    /// Agent 标记的"疑似问题"范围。
    AgentWarning,
    /// 已有批注（与 AnnotationStore 同步）。
    Annotation,
}

/// 面板种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelKind {
    /// Agent 主面板（时间线 / 输入框）。
    Agent,
    /// 批注列表面板。
    Annotation,
    /// 过滤器列表面板。
    Filter,
}

/// 消息级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageLevel {
    Info,
    Success,
    Warning,
    Error,
}

/// 临时过滤器规格（不写入 AnnotationStore）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FilterSpec {
    /// 正则 / 字面量匹配某行。
    Literal { pattern: String, case_sensitive: bool },
    /// 匹配错误码（如 `5xx` / `4xx`）。
    ErrorLevel { min: u16, max: u16 },
    /// 包含子串的行。
    Contains { needle: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_line_serializes() {
        let v = ViewIntent::FocusLine { line: 42 };
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("\"focus_line\""));
        assert!(s.contains("\"line\":42"));
    }

    #[test]
    fn highlight_range_carries_kind() {
        let v = ViewIntent::HighlightRange {
            start: 1,
            end: 10,
            kind: HighlightKind::AgentMatch,
        };
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("\"agent_match\""));
    }

    #[test]
    fn filter_spec_variants() {
        let v = FilterSpec::ErrorLevel { min: 500, max: 599 };
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("\"error_level\""));
    }
}
