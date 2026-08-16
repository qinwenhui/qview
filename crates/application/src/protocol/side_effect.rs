//! 工具副作用分级（架构文档 §6.1）。
//!
//! 五级分类决定 UI 是否弹审批、审计如何记录、权限如何过滤。
//! 与架构文档 §6.2 命名规则绑定：`mut_*` / `destruct_*` 前缀。

use serde::{Deserialize, Serialize};

/// 工具副作用分级。
///
/// 级别按"严重程度"递增；UI / 权限层据此分流。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffect {
    /// 只读：不会改变任何状态，可随时执行。
    ReadOnly,
    /// 仅影响视图：跳行 / 高亮 / 过滤 — UI 可忽略失败。
    ViewOnly,
    /// 可撤销：批注 / 选区 — 写入后有"撤销"语义。
    Reversible,
    /// 变更：编辑 / 替换 — 默认走"提案-确认-执行"。
    Mutating,
    /// 破坏性：删除文件 / 清空批注等不可逆操作。
    Destructive,
}

impl SideEffect {
    /// 当前级别需要的工具名前缀。
    ///
    /// - `Mutating` → `mut_`
    /// - `Destructive` → `destruct_`
    /// - 其他级别不加前缀（保持可读）
    ///
    /// 用于把工具名直接挂在 `ToolSpec.description` 前部供 UI 识别。
    pub fn tool_name_prefix(self) -> &'static str {
        match self {
            SideEffect::ReadOnly | SideEffect::ViewOnly | SideEffect::Reversible => "",
            SideEffect::Mutating => "mut_",
            SideEffect::Destructive => "destruct_",
        }
    }

    /// 是否需要走"提案-确认-执行"流程。
    ///
    /// `Reversible` / `Mutating` / `Destructive` 三者都触发审批；
    /// `Reversible` 仍需 UI 知情（批注一旦写入会出现在 AnnotationStore 里）。
    pub fn requires_approval(self) -> bool {
        matches!(
            self,
            SideEffect::Reversible | SideEffect::Mutating | SideEffect::Destructive
        )
    }

    /// 是否会让文档内容发生变化。
    pub fn changes_document(self) -> bool {
        matches!(self, SideEffect::Mutating | SideEffect::Destructive)
    }
}

impl Default for SideEffect {
    fn default() -> Self {
        SideEffect::ReadOnly
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_rules() {
        assert_eq!(SideEffect::ReadOnly.tool_name_prefix(), "");
        assert_eq!(SideEffect::ViewOnly.tool_name_prefix(), "");
        assert_eq!(SideEffect::Reversible.tool_name_prefix(), "");
        assert_eq!(SideEffect::Mutating.tool_name_prefix(), "mut_");
        assert_eq!(SideEffect::Destructive.tool_name_prefix(), "destruct_");
    }

    #[test]
    fn approval_required_for_writes() {
        assert!(!SideEffect::ReadOnly.requires_approval());
        assert!(!SideEffect::ViewOnly.requires_approval());
        assert!(SideEffect::Reversible.requires_approval());
        assert!(SideEffect::Mutating.requires_approval());
        assert!(SideEffect::Destructive.requires_approval());
    }

    #[test]
    fn serde_round_trip() {
        for v in [
            SideEffect::ReadOnly,
            SideEffect::ViewOnly,
            SideEffect::Reversible,
            SideEffect::Mutating,
            SideEffect::Destructive,
        ] {
            let s = serde_json::to_string(&v).unwrap();
            let back: SideEffect = serde_json::from_str(&s).unwrap();
            assert_eq!(v, back);
        }
    }
}
