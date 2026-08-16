//! qview 端 Tool 注册表（架构文档 §5.2.1 / §6.2 / §7）。
//!
//! 包装 `contexa_tools::ToolRegistry`，叠加：
//! - 每条工具的 `ToolMetadata`（副作用 / 描述）供 UI 展示
//! - `PermissionPolicy::allow_tools` 二次过滤（即使 LLM 绕过 `effective_tools` 也拦得住）
//! - 脱敏管道（在 `call_tool` 结果出来时正则替换为 `***`）

pub mod metadata;
pub mod registry;

pub use metadata::ToolMetadata;
pub use registry::ToolRegistry;
