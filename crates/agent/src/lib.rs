//! `qview-agent` — 器灵 Agent 运行时（架构文档 §5.2.2 / §6 / §8）。
//!
//! ## 职责
//! - 包装 `contexa::ReActWorker`，不重新实现 ReAct 循环
//! - 通过 `QviewSinkHook` 把 `contexa::Hook` 7 个钩子点翻译为 `AgentEvent`
//! - 维护 `ApprovalRegistry`（oneshot-based proposal 等待）
//! - 暴露 `AgentRuntimeHandle` 给 UI（仅 4 类 API：start/cancel/subscribe/proposal_decision）
//!
//! ## 不做
//! - 不实现 Worker / Task / 限额 / 压缩 / 记忆（contexa 已经做了）
//! - 不实现 UI（egui / TUI 由 ui crate 自己接入 `AgentSink`）

#![forbid(unsafe_code)]
// 内部 crate 不对外发布，工具注册/事件枚举等样板不强制文档，避免编译噪音。
#![allow(missing_docs)]

pub mod approval;
pub mod audit;
pub mod builder;
pub mod config;
pub mod event;
pub mod guarded_tool;
pub mod handle;
pub mod intent;
pub mod reasoning_effort;
pub mod runtime;
pub mod sink;
pub mod sink_hook;
pub mod proposal;

pub use approval::ApprovalRegistry;
pub use audit::{AuditHook, AuditRecord, AuditSink, FileAuditSink, InMemoryAuditSink};
pub use builder::{allow_all_with_writes, attach_sources, make_annotate_guarded, make_export_guarded, make_guarded_sources};
pub use config::{AgentConfig, AgentDeps, LlmProvider, ProviderConfig};
pub use event::{AgentEvent, AgentSink, Phase, Role, SessionId, SubscriptionGuard};
pub use guarded_tool::{into_source, GuardedTool, GuardedToolMeta, InnerInvokeFn};
pub use handle::{AgentGoal, AgentRuntimeHandle, ProposalDecision};
pub use runtime::AgentRuntime;
