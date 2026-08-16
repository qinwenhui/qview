//! Flow 抽象层（架构 §22.x — P2「Flow」落地）。
//!
//! ## 定位
//!
//! 把"打开文件 → 搜索 → 读上下文 → 总结 → 出报告"这种典型链子抽成可复用、可
//! 断点续跑的 Flow。`AgentRuntimeInner::start_session_with` 在 router 命中
//! `suggested_flow` 时跳过 ReAct 多轮、直接进 FlowRunner。
//!
//! ## 与 ReAct 的协作（v1）
//!
//! - **Flow-only 模式**：简单场景（OpenFile / ListDir）跑完 Flow 就 SessionFinished。
//! - **Flow-then-ReAct**（v1 不实现，留接口）：Flow 跑完后把结果塞 context_hints，
//!   再让 LLM 做总结。
//! - **ReAct-with-Flow**（v1 不实现）：ReAct 过程中 LLM 选择调 `flow.run(flow_id)`
//!   工具，runtime 把控制权转给 FlowRegistry。
//!
//! ## v1 范围
//!
//! - FlowId 枚举：5 个内置 Flow
//! - Flow trait：`plan()` + `name()`
//! - FlowRegistry：`register()` / `get()` / `find_for()`
//! - 不实现断点续跑（先做最简链路）
//!
//! ## 模块结构
//!
//! - [`mod@work`]：Work 单元（独立超时、重试、审计）
//! - [`flows`]：5 个内置 Flow 的实现
//! - [`runner`]：FlowRunner（执行 Flow 的主循环，串行 step + 并行 work）

pub mod flows;
pub mod runner;
pub mod work;

use std::collections::HashMap;
use std::sync::Arc;

use crate::intent::{Intent, IntentKind};
use crate::sink_hook::WeakSinks;

/// Flow 的执行步骤。
///
/// ## v1 范围
///
/// - [`Step::Work`]：单 Work 单元
/// - [`Step::Parallel`]：并行多个 Work
/// - [`Step::LlmDecision`]：转交 ReAct 做一次 LLM 决策（v1 占位；先跑 Work 序列）
/// - [`Step::Done`]：Flow 完成 + summary 文本（runner 看到 Done 就停）
#[derive(Debug, Clone)]
pub enum Step {
    Work(work::WorkSpec),
    Parallel(Vec<work::WorkSpec>),
    /// v1 不实现：转 ReAct 做一次 LLM 决策。详见 `runner.rs`。
    LlmDecision { prompt: String },
    Done { summary: String },
}

/// Flow 标识。
///
/// 与 [`crate::intent::FlowId`] 同义；此处独立定义以避免循环依赖（router 不
/// 依赖 flow 模块的具体实现，但 flow 模块实现要参考 router 给的 FlowId）。
///
/// v1 阶段 router 直接给 FlowId；如果 router 给出 None，runtime 走完整 ReAct。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlowId {
    /// 打开单个文件（最简单 Flow：单 Work）。
    OpenFile,
    /// 列出目录文件（最简单 Flow：单 Work）。
    ListDir,
    /// 查生产日志 → 出报告（多 Work + LLM 决策）。
    SearchLogAndReport,
    /// 标注文件（读全文 → LLM 挑疑点 → 并行打 N 个批注）。
    AnnotateFile,
    /// 出当前文件报告（读全文 → LLM 整理 → 导出）。
    ExportCurrentReport,
}

impl FlowId {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenFile => "OpenFile",
            Self::ListDir => "ListDir",
            Self::SearchLogAndReport => "SearchLogAndReport",
            Self::AnnotateFile => "AnnotateFile",
            Self::ExportCurrentReport => "ExportCurrentReport",
        }
    }
}

/// Flow 上下文（执行 Flow 时需要的运行时依赖）。
///
/// v1 只放最小依赖；后续可能加 store（用于断点续跑）、cancellation token 等。
#[derive(Clone)]
pub struct FlowContext {
    /// qview 服务（路由到底层 ToolRegistry）。
    pub docs: Arc<dyn FlowDocs>,
    /// 可选：Flow 当前操作的目标文件路径（用户输入或当前 GUI 打开的文件）。
    /// SearchLogAndReport / AnnotateFile / ExportCurrentReport 用这个字段
    /// 决定要读 / 标 / 导出的目标。
    pub current_file: Option<String>,
    /// 用户的原始请求（供 LlmDecision 总结时知道用户在问什么）。
    /// 为 None 时 Flow 无法让 LLM 理解上下文，LlmDecision 会尽量兜底。
    pub user_query: Option<String>,
    /// **事件广播**：Flow 的 Work 走这里发 `ToolCallStarted/Finished` + `ViewIntentEmitted`，
    /// 让 GUI 工具列表有数据、view_intent 能应用到主视图、工具调用能落库。
    /// **为什么需要**：Flow 直接调 `ToolRegistry`，不经过 `QviewSinkHook`——
    /// 不广播的话，打开文件后 GUI 主视图不切换（用户以为"没打开"）、工具列表空、不入库。
    /// 为 None 时（测试）静默跳过广播。
    pub sinks: Option<WeakSinks>,
    /// 会话 id（广播事件用）。
    pub session_id: Option<String>,
}

/// Flow 用的服务抽象：路由到底层 `ToolRegistry`（与 ReAct 走同一套工具）。
///
/// ## 设计要点
///
/// - **Flow 与 ReAct 共用 ToolRegistry**（架构 §22.x）——不在 service 层复制工具实现。
/// - `tool_call` 一次调一个工具（open_document / list_directory / search_text /
///   read_context / create_annotation / export_report / write_document）。
/// - 每个工具的 schema 由 ToolRegistry 保证，Flow 不需自己写参数校验。
/// - 失败语义：返回 `Err(...)`，FlowRunner 会按 RetryPolicy 重试；最终失败时把
///   `Err` 转成 `{"error": "..."}` JSON 写进 `WorkResult.value`，让下游 Flow
///   能继续推进（不中断整个 Flow）。
#[async_trait::async_trait]
pub trait FlowDocs: Send + Sync + 'static {
    /// 调一个工具。`tool` 是注册名（如 `open_document`、`read_context`）。
    async fn tool_call(
        &self,
        tool: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value>;
}

/// `ToolRegistry` → `FlowDocs` 适配器（让 Flow 走与 ReAct 相同的工具链）。
///
/// 复用 ReActWorker 的同一份 ToolRegistry，确保：
/// - 权限策略一致（policy.allows 在 Flow / ReAct 同样生效）
/// - 脱敏一致（redact_patterns 在 Flow / ReAct 同样生效）
/// - 不重复实现工具
pub struct RegistryFlowDocs {
    pub registry: Arc<qview_application::tool::ToolRegistry>,
}

#[async_trait::async_trait]
impl FlowDocs for RegistryFlowDocs {
    async fn tool_call(
        &self,
        tool: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let result = self.registry.call_tool(tool, args).await;
        if result.is_error {
            // ToolResult 把失败包成 is_error=true + content={"error":...}
            // 转成 anyhow::Err 让 FlowExecutor 走重试
            let reason = if result.content.get("error").is_some() {
                result.content.to_string()
            } else {
                format!("tool error: {}", result.content)
            };
            anyhow::bail!("{reason}")
        } else {
            Ok(result.content)
        }
    }
}

/// Flow trait：定义"如何把 Intent 拆成 Step 列表"。
///
/// ## 设计要点
/// - **不可变**：Flow 一旦注册，行为不变；运行时不需要 mutex。
/// - **零状态**：`plan()` 是纯函数（除了 `ctx` 这个共享依赖）。
/// - **多 Work 并行由 Step::Parallel 支持**：Flow 本身只决定顺序，
///   并行由 [`runner`] 负责 join_all。
pub trait Flow: Send + Sync {
    fn id(&self) -> FlowId;
    /// 把 Intent 转成 Step 列表。ctx 用于解析"当前文件"等运行时上下文。
    fn plan(&self, intent: &Intent, ctx: &FlowContext) -> anyhow::Result<Vec<crate::flow::Step>>;
}

/// Flow 注册表（v1 简单 HashMap）。
///
/// ## 设计要点
/// - **全局单例**（`pub static FLOW_REGISTRY: Lazy<FlowRegistry>`）：所有 Flow
///   在第一次访问时 lazy 注册；测试也可以覆盖。
/// - **不可变**：注册后只读；hot-reload 不在 v1 范围。
pub struct FlowRegistry {
    flows: HashMap<FlowId, Arc<dyn Flow>>,
}

impl FlowRegistry {
    pub fn new() -> Self {
        Self {
            flows: HashMap::new(),
        }
    }

    pub fn register(&mut self, flow: Arc<dyn Flow>) {
        self.flows.insert(flow.id(), flow);
    }

    pub fn get(&self, id: FlowId) -> Option<Arc<dyn Flow>> {
        self.flows.get(&id).cloned()
    }

    /// 按 Intent 找 Flow：v1 简单用 kind 一一映射。
    pub fn find_for(&self, intent: &Intent) -> Option<Arc<dyn Flow>> {
        let id = match intent.kind {
            IntentKind::OpenFile => Some(FlowId::OpenFile),
            IntentKind::ListDir => Some(FlowId::ListDir),
            IntentKind::SearchLog => Some(FlowId::SearchLogAndReport),
            IntentKind::AnnotateFile => Some(FlowId::AnnotateFile),
            IntentKind::ExportReport => Some(FlowId::ExportCurrentReport),
            _ => None,
        }?;
        self.get(id)
    }

    pub fn ids(&self) -> Vec<FlowId> {
        self.flows.keys().copied().collect()
    }
}

impl Default for FlowRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_register_and_get() {
        let mut r = FlowRegistry::new();
        let flow = Arc::new(flows::OpenFileFlow);
        r.register(flow.clone());
        assert_eq!(r.get(FlowId::OpenFile).unwrap().id(), FlowId::OpenFile);
        assert!(r.get(FlowId::ListDir).is_none());
    }

    #[test]
    fn registry_find_for_intent() {
        let mut r = FlowRegistry::new();
        r.register(Arc::new(flows::OpenFileFlow));
        r.register(Arc::new(flows::ListDirFlow));

        let mut intent = Intent::unknown();
        intent.kind = IntentKind::OpenFile;
        assert!(r.find_for(&intent).is_some());

        intent.kind = IntentKind::SearchLog;
        // SearchLog 没注册 → None
        assert!(r.find_for(&intent).is_none());
    }
}
