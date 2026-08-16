//! 协议层 ID 类型（架构文档 §6.1）。
//!
//! 这些是稳定的 wire-format 类型，所有 crate 都用同一份。
//! `SessionId` 不在 qview 端重定义 — 直接用 `contexa_core::Task::task_id`（String）。

use serde::{Deserialize, Serialize};

/// 文档 ID。在 qview 端把"当前打开的文档"实例化为单调递增的 id，
/// 工具的输入参数里全部使用 `DocumentId`，**禁止**让 LLM 直接传 path。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DocumentId(pub u64);

impl DocumentId {
    /// 构造一个新的 DocumentId（测试 / fixture 用）。
    pub const fn new(v: u64) -> Self {
        Self(v)
    }

    /// 取内部数值。
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for DocumentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "doc#{}", self.0)
    }
}

/// 单次工具调用 ID（uuid v4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolCallId(pub uuid::Uuid);

impl ToolCallId {
    /// 生成一个新的 ToolCallId。
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for ToolCallId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ToolCallId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "call#{}", self.0)
    }
}

/// 提案 ID（一次需要用户确认的写操作）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProposalId(pub uuid::Uuid);

impl ProposalId {
    /// 生成一个新的 ProposalId。
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for ProposalId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ProposalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "prop#{}", self.0)
    }
}

// 直接引用顶层 uuid crate（依赖在 Cargo.toml）。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_id_display() {
        assert_eq!(DocumentId(42).to_string(), "doc#42");
    }

    #[test]
    fn tool_call_id_default_is_unique() {
        let a = ToolCallId::default();
        let b = ToolCallId::default();
        assert_ne!(a, b);
    }

    #[test]
    fn json_round_trip() {
        let id = DocumentId(7);
        let s = serde_json::to_string(&id).unwrap();
        assert_eq!(s, "7");
        let back: DocumentId = serde_json::from_str(&s).unwrap();
        assert_eq!(back, id);
    }
}
