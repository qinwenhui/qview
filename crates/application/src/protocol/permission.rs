//! 权限策略（架构文档 §6.1 / §11）。
//!
//! 把 qview 业务层的"允许/拒绝"映射到 contexa `WorkerConfig` 字段。
//! qview 端二次过滤（在 `ToolRegistry::call_tool` 中生效）保证 LLM 即使绕过
//! `effective_tools` 输出也无法调到不在白名单里的工具。

use serde::{Deserialize, Serialize};

use super::side_effect::SideEffect;

use contexa::prelude::*;

/// 权限策略：白名单 + 副作用分级 + 资源上限 + 脱敏模式。
///
/// 字段一一对应 `contexa_core::WorkerConfig`（见 §11.4 映射表）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionPolicy {
    /// 允许的工具白名单（与 `ToolSpec.name` 对齐）。
    /// 空列表 = 拒绝所有工具（必须显式 opt-in）。
    pub allow_tools: Vec<String>,

    /// 哪些副作用级别需要用户确认才执行。
    /// 默认 = `[Reversible, Mutating, Destructive]`。
    pub require_approval: Vec<SideEffect>,

    /// 单次工具调用最多读多少行（超限截断 + `truncated: true`）。
    pub max_read_lines: u64,

    /// 累计业务工具调用上限 → `WorkerConfig::max_total_tool_calls`。
    pub max_tool_calls: u32,

    /// 累计 token 上限 → `WorkerConfig::max_total_tokens`。
    pub max_token_budget: u32,

    /// ReAct 循环轮数 → `WorkerConfig::max_tool_rounds`。
    pub max_tool_rounds: u32,

    /// 总耗时秒数 → `WorkerConfig::max_wall_seconds`。
    pub max_wall_seconds: f64,

    /// 单条工具结果最大字符数 → `WorkerConfig::tool_result_max_chars`。
    pub tool_result_max_chars: usize,

    /// 单轮并发工具上限 → `WorkerConfig::max_tool_workers`。
    pub max_tool_workers: u32,

    /// 工具单次调用超时（qview 端 `tokio::time::timeout`）。
    pub tool_timeout_secs: u64,

    /// 脱敏正则列表（在结果中把匹配替换为 `***`）。
    pub redact_patterns: Vec<String>,
}

impl Default for PermissionPolicy {
    fn default() -> Self {
        Self {
            allow_tools: Vec::new(),
            require_approval: vec![
                SideEffect::Reversible,
                SideEffect::Mutating,
                SideEffect::Destructive,
            ],
            max_read_lines: crate::DEFAULT_MAX_READ_LINES,
            max_tool_calls: 20,
            max_token_budget: 200_000,
            max_tool_rounds: 20,
            max_wall_seconds: 300.0,
            tool_result_max_chars: 8_000,
            max_tool_workers: 8,
            tool_timeout_secs: crate::DEFAULT_TOOL_TIMEOUT_SECS,
            redact_patterns: Vec::new(),
        }
    }
}

impl PermissionPolicy {
    /// 默认策略 + 显式 allowlist（首期最常用）。
    pub fn with_allowlist(allow: Vec<String>) -> Self {
        Self {
            allow_tools: allow,
            ..Self::default()
        }
    }

    /// 工具是否被允许。
    pub fn allows(&self, tool_name: &str) -> bool {
        // 框架保留名总是允许（worker_finish）。
        if tool_name == FINISH_TOOL_NAME {
            return true;
        }
        self.allow_tools.iter().any(|n| n == tool_name)
    }

    /// 该副作用级别是否需要审批。
    pub fn needs_approval(&self, side: SideEffect) -> bool {
        self.require_approval.contains(&side)
    }

    /// 翻译为 `contexa_core::WorkerConfig`。
    ///
    /// 注意：contexa 的 `ToolRegistry` 负责"按实例/任务/finish 合并"，
    /// qview 端 `allow_tools` 在 `ToolRegistry::call_tool` 里二次过滤（架构 §11.1）。
    pub fn to_worker_config(&self) -> WorkerConfig {
        WorkerConfig::builder()
            .max_tool_rounds(self.max_tool_rounds)
            .max_total_tool_calls(self.max_tool_calls)
            .max_total_tokens(self.max_token_budget)
            .max_wall_seconds(self.max_wall_seconds)
            .max_tool_workers(self.max_tool_workers)
            .tool_result_max_chars(self.tool_result_max_chars)
            // 压缩 / 预算默认关（P2 由 qview-agent 决定是否开）。
            .context_compress_enabled(false)
            .context_budget_enabled(false)
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_arch_table() {
        let p = PermissionPolicy::default();
        assert_eq!(p.max_read_lines, 200);
        assert_eq!(p.max_tool_calls, 20);
        assert_eq!(p.max_token_budget, 200_000);
        assert_eq!(p.max_tool_rounds, 20);
        assert!((p.max_wall_seconds - 300.0).abs() < 1e-6);
        assert_eq!(p.max_tool_workers, 8); // 架构文档 §11.4 与默认值一致
        assert_eq!(p.tool_result_max_chars, 8_000);
        assert_eq!(p.tool_timeout_secs, 30);
        assert!(p.needs_approval(SideEffect::Mutating));
        assert!(!p.needs_approval(SideEffect::ReadOnly));
    }

    #[test]
    fn allows_uses_allowlist_and_finish() {
        let mut p = PermissionPolicy::default();
        p.allow_tools = vec!["search_text".into(), "read_context".into()];
        assert!(p.allows("search_text"));
        assert!(p.allows("read_context"));
        assert!(!p.allows("annotate_create"));
        assert!(p.allows(FINISH_TOOL_NAME));
    }

    #[test]
    fn to_worker_config_maps_all_fields() {
        let p = PermissionPolicy {
            allow_tools: vec!["x".into()],
            require_approval: vec![SideEffect::Mutating],
            max_read_lines: 2,
            max_tool_calls: 3,
            max_token_budget: 4,
            max_tool_rounds: 5,
            max_wall_seconds: 6.0,
            tool_result_max_chars: 7,
            max_tool_workers: 9,
            tool_timeout_secs: 10,
            redact_patterns: vec!["secret".into()],
        };
        let cfg = p.to_worker_config();
        assert_eq!(cfg.max_tool_rounds, 5);
        assert_eq!(cfg.max_total_tool_calls, 3);
        assert_eq!(cfg.max_total_tokens, 4);
        assert!((cfg.max_wall_seconds - 6.0).abs() < 1e-6);
        assert_eq!(cfg.max_tool_workers, 9);
        assert_eq!(cfg.tool_result_max_chars, 7);
    }
}
