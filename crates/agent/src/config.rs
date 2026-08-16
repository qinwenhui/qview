//! `AgentConfig` — qview-agent 的配置类型（对应 `qview-core::EngineConfig` 对 UI 的角色）。
//!
//! ## 设计动机
//! UI 层**不**手写 `ReActWorker` / `DummyLLM` / `ToolRegistry` / `ApprovalRegistry` 的装配，
//! 而是用自己的配置格式（如 egui 的 `AppConfig`、TUI 的 `toml`）派生一个 `AgentConfig`，
//! 再调 `AgentConfig::build(deps)` 得到现成的 `AgentRuntimeHandle`。
//!
//! 这保证了：
//! - UI 与 `contexa` 框架解耦（架构 §22.5 #8）
//! - 接入真实 LLM 时只改配置，不改代码
//! - 多端（egui / TUI / CLI）共享同一套装配逻辑
//!
//! ## 与架构文档的对应
//! - §11.4 资源预算表 → `PermissionPolicy` 字段
//! - §22.2/§22.3 LLM provider → `LlmProvider` + `ProviderConfig`
//! - §5.2.2 依赖 → `AgentConfig::build` 内部用 `qview_application` 服务

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use qview_application::protocol::SideEffect;
use qview_application::service::annotation::AnnotationService;
use qview_application::service::{DocumentService, SearchService};

use contexa_llm::{DeepSeekClient, DummyLLM, LLMClient, OpenAICompatClient};

use crate::audit::{AuditSink, FileAuditSink, InMemoryAuditSink};
use crate::reasoning_effort::ReasoningEffortClient;
use crate::runtime::AgentRuntime;

// ---------------------------------------------------------------------------
// Provider 配置
// ---------------------------------------------------------------------------

/// LLM Provider 协议。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmProvider {
    /// DummyLLM（测试 / 离线演示；不发网络请求）。
    Mock,
    /// OpenAI ChatCompletions 协议（官方 API 或任何兼容端点）。
    OpenAI,
    /// OpenAI 兼容的本地 / 通用端点（自定义 base_url，无鉴权或 Bearer）。
    OpenAICompat,
    /// Ollama 本地服务（`http://localhost:11434/v1`，无鉴权）。
    Ollama,
    /// DeepSeek（`https://api.deepseek.com/v1`）。
    DeepSeek,
}

impl Default for LlmProvider {
    fn default() -> Self {
        LlmProvider::Mock
    }
}

/// Provider 连接参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub provider: LlmProvider,
    /// 端点（OpenAI/OpenAICompat 必填；Ollama 缺省 localhost:11434；DeepSeek 缺省内建）。
    pub base_url: Option<String>,
    /// 模型名（OpenAI/Ollama/DeepSeek 必填；Mock 忽略）。
    pub model: String,
    /// 设置页直接填写的 API key（明文存本机配置文件；普通用户无需配环境变量）。
    /// 优先级高于 `api_key_env`。
    pub api_key: Option<String>,
    /// 从**环境变量**读 API key 的变量名（高级用法；`api_key` 已填则忽略）。
    pub api_key_env: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Mock 专用：DummyLLM 脚本 JSON 文件路径（`Vec<LLMResponse>` 序列化）。
    pub mock_script_path: Option<PathBuf>,
    /// Mock 专用：静态回复文本（当没有脚本路径时）。
    pub mock_static: Option<String>,
    /// DeepSeek 思考模式开关 + 强度（OpenAI 协议字段；Ollama 忽略）。
    ///
    /// 按 DeepSeek 官方文档，OpenAI 协议下需要同时控制两个字段：
    ///
    /// - **`thinking.type`** = `"enabled"` / `"disabled"`（开关，默认 enabled）
    /// - **`reasoning_effort`** = 强度
    ///
    /// 本字段用一个字符串统一表示，映射规则：
    ///
    /// | `reasoning_effort` 取值 | wire body 发送内容 |
    /// |---|---|
    /// | `None` | 不动 ChatRequest（业务方自己管） |
    /// | `"none"` | `{"thinking":{"type":"disabled"}}`，不发明 `reasoning_effort` |
    /// | `"low"`   | `{"thinking":{"type":"enabled"},"reasoning_effort":"low"}` |
    /// | `"high"`  | `{"thinking":{"type":"enabled"},"reasoning_effort":"high"}` |
    /// | `"xhigh"` | `{"thinking":{"type":"enabled"},"reasoning_effort":"xhigh"}` |
    /// | `"max"`   | `{"thinking":{"type":"enabled"},"reasoning_effort":"max"}` |
    ///
    /// DeepSeek v4-flash / v4-pro 的实际映射（按官方文档）：
    ///
    /// | 请求 effort | v4-flash 实际 | v4-pro 实际 |
    /// |---|---|---|
    /// | low | low | high |
    /// | high | high | high |
    /// | xhigh | high | max |
    /// | max | max | max |
    ///
    /// UI 默认 `"low"`；用户可在设置 → AI 切换。
    ///
    /// 注意：DeepSeek 思考模式下 `temperature` / `top_p` / `presence_penalty` /
    /// `frequency_penalty` 不生效（不报错但被忽略）。
    pub reasoning_effort: Option<String>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            provider: LlmProvider::Mock,
            base_url: None,
            model: String::new(),
            api_key: None,
            api_key_env: None,
            temperature: None,
            // 默认 4000 输出上限：钳制 LLM 单次输出量。实测不带 max_tokens 时
            // 模型单次能生成 33,948 tokens（84KB / 190s，批注慢主因）。4000 tokens
            // ≈ 6000+ 中文字，够总结/报告；需要长文可在设置里调大。
            max_tokens: Some(4000),
            mock_script_path: None,
            mock_static: None,
            // 默认 "low"：推理模型在 qview 这种"工具调用 + 读全文"场景下
            // 深度思考几乎不带来收益，反而是延迟主因（实测 v4-flash 第 5 轮
            // 思考耗时 227s，开 low 预计可降到 30s 内）。
            reasoning_effort: Some("low".into()),
        }
    }
}

impl ProviderConfig {
    /// 读取 api key：优先设置页填写的 `api_key`，回退环境变量 `api_key_env`。
    pub fn api_key(&self) -> Option<String> {
        if let Some(k) = &self.api_key {
            if !k.is_empty() {
                return Some(k.clone());
            }
        }
        let var = self.api_key_env.as_ref()?;
        std::env::var(var).ok().filter(|s| !s.is_empty())
    }

    /// 按 provider 构造 `Arc<dyn LLMClient>`。
    ///
    /// 返回的 client 已经被 [`ReasoningEffortClient`] 包过——每次 `chat()` 会
    /// 按 `self.reasoning_effort` 注入 DeepSeek 思考控制字段：
    ///
    /// - `reasoning_effort == None` → 装饰器透传，业务方自己管 thinking
    /// - `Some("none")` → 发 `thinking.type=disabled`
    /// - `Some("low"|"high"|"xhigh"|"max")` → 发 `thinking.type=enabled`
    ///   + `reasoning_effort=<level>`
    ///
    /// 这里只过滤空字符串；`"none"` 必须**保留**传给装饰器，否则装饰器
    /// 永远收不到"关 thinking"的语义按钮。
    pub fn build_client(&self) -> anyhow::Result<Arc<dyn LLMClient>> {
        let raw: Arc<dyn LLMClient> = self.build_raw_client()?;
        let effort = self
            .reasoning_effort
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        Ok(Arc::new(ReasoningEffortClient::new(
            raw,
            effort,
            self.max_tokens,
        )))
    }

    /// 构造**未包装**的 provider client。仅供测试 / 内部使用。
    fn build_raw_client(&self) -> anyhow::Result<Arc<dyn LLMClient>> {
        let ok = |c: Box<dyn LLMClient>| -> Arc<dyn LLMClient> { c.into() };
        let model = self.model.trim();
        match self.provider {
            LlmProvider::Mock => {
                let llm = match &self.mock_script_path {
                    Some(p) => {
                        let raw = std::fs::read_to_string(p)
                            .with_context(|| format!("read mock script {}", p.display()))?;
                        let responses: Vec<contexa_llm::LLMResponse> =
                            serde_json::from_str(&raw)
                                .context("mock script must be a Vec<LLMResponse> JSON")?;
                        DummyLLM::new(responses)
                    }
                    None => DummyLLM::static_response(contexa_llm::LLMResponse::new(
                        self.mock_static
                            .clone()
                            .unwrap_or_else(|| "(mock: no static text)".into()),
                    )),
                };
                Ok(Arc::new(llm))
            }
            LlmProvider::OpenAI => {
                if model.is_empty() {
                    anyhow::bail!("OpenAI provider 需要 model");
                }
                let base = self
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.openai.com/v1".into());
                let client = match self.api_key() {
                    Some(key) => OpenAICompatClient::new(base, key, model),
                    None => OpenAICompatClient::without_auth(base, model),
                };
                Ok(ok(Box::new(client)))
            }
            LlmProvider::OpenAICompat => {
                if model.is_empty() {
                    anyhow::bail!("OpenAICompat provider 需要 model");
                }
                let base = self
                    .base_url
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("OpenAICompat provider 需要 base_url"))?;
                let client = match self.api_key() {
                    Some(key) => OpenAICompatClient::new(base, key, model),
                    None => OpenAICompatClient::without_auth(base, model),
                };
                Ok(ok(Box::new(client)))
            }
            LlmProvider::Ollama => {
                if model.is_empty() {
                    anyhow::bail!("Ollama provider 需要 model");
                }
                let base = self
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "http://localhost:11434/v1".into());
                Ok(ok(Box::new(OpenAICompatClient::without_auth(base, model))))
            }
            LlmProvider::DeepSeek => {
                if model.is_empty() {
                    anyhow::bail!("DeepSeek provider 需要 model");
                }
                let key = self
                    .api_key()
                    .ok_or_else(|| anyhow::anyhow!("DeepSeek provider 需要 api_key（请设置环境变量）"))?;
                Ok(ok(Box::new(DeepSeekClient::with_model(key, model))))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Agent 运行配置
// ---------------------------------------------------------------------------

/// qview-agent 运行配置（UI 层通过自己的配置格式派生此类型）。
///
/// `build()` 把配置装配成 `AgentRuntimeHandle`（内含 ReActWorker / 工具 / 权限 / 审计）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    // ---- LLM ----
    pub provider: ProviderConfig,

    // ---- Worker 身份 ----
    pub instance_id: String,
    pub business_code: String,
    /// 内联覆盖系统提示词（优先级最高；非空时覆盖外部文件）。
    pub system_prompt: Option<String>,
    /// 外部系统提示词文件（分层 md，可编辑测试）。优先级低于 `system_prompt`、
    /// 高于内置默认；缺失 / 为空自动回退内置。GUI 缺省设为 `{config_dir}/system_prompt.md`。
    pub system_prompt_file: Option<PathBuf>,

    // ---- 限额（架构 §11.4）----
    pub max_tool_rounds: u32,
    pub max_tool_calls: u32,
    pub max_token_budget: u32,
    pub max_wall_seconds: f64,
    pub max_tool_workers: u32,
    pub tool_result_max_chars: usize,

    // ---- 压缩 / 预算 / 记忆（架构 §22.1）----
    pub context_compress_enabled: bool,
    pub context_budget_enabled: bool,
    pub memory_enabled: bool,

    // ---- 上下文窗口（按阶段截断对话历史）----
    /// 意图分类器（项目经理第一步：接需求→分析→计划）可见的对话历史上限（字符数）。
    pub classifier_context_chars: usize,
    /// ReAct 完整推理（项目经理执行阶段）可见的对话历史上限（字符数）。
    pub react_context_chars: usize,

    // ---- 权限 ----
    pub allow_tools: Vec<String>,
    pub require_approval: Vec<SideEffect>,
    pub max_read_lines: u64,
    pub redact_patterns: Vec<String>,

    // ---- 审计 ----
    /// 若 `Some(dir)` → FileAuditSink 落盘；`None` → InMemory。
    pub audit_dir: Option<PathBuf>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            provider: ProviderConfig::default(),
            instance_id: "qview-agent".into(),
            business_code: "qview".into(),
            system_prompt: None,
            system_prompt_file: None,
            max_tool_rounds: 20,
            max_tool_calls: 20,
            max_token_budget: 200_000,
            max_wall_seconds: 300.0,
            max_tool_workers: 20,
            tool_result_max_chars: 8_000,
            // 默认开上下文压缩：多轮对话后历史无限膨胀是慢的元凶之一
            //（实测 tokens_prompt 一路涨到 168K）。contexa 在超过
            // max_context_tokens×threshold 时自动压缩旧消息，保留最近几轮。
            context_compress_enabled: true,
            context_budget_enabled: true,
            // 记忆启用：InMemoryStore 已接线，executor 自动 recall/consolidate；
            // 每个 session 的 worker 共享同一 store Arc → 进程内跨会话可回忆。
            memory_enabled: true,
            classifier_context_chars: 2_000,
            react_context_chars: 12_000,
            allow_tools: Vec::new(),
            // 默认只对「写盘」类副作用审批：Mutating(导出报告/写文件) 与 Destructive。
            // Reversible（如创建批注）默认自动放行，避免器灵做常规操作也要等审批。
            require_approval: vec![
                SideEffect::Mutating,
                SideEffect::Destructive,
            ],
            max_read_lines: qview_application::DEFAULT_MAX_READ_LINES,
            redact_patterns: Vec::new(),
            audit_dir: None,
        }
    }
}

impl AgentConfig {
    /// 便捷：纯 Mock provider 的配置（离线演示 / 测试）。
    pub fn mock(static_text: impl Into<String>) -> Self {
        let mut c = Self::default();
        c.provider.provider = LlmProvider::Mock;
        c.provider.mock_static = Some(static_text.into());
        c
    }

    /// 便捷：OpenAI 兼容端点（Ollama / 本地 / 自定义）。
    pub fn openai_compat(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        let mut c = Self::default();
        c.provider.provider = LlmProvider::OpenAICompat;
        c.provider.base_url = Some(base_url.into());
        c.provider.model = model.into();
        c
    }

    /// 把 `allow_tools` 转成 qview 全量工具列表（含写工具，供 GuardedTool 用）。
    pub fn allow_all_tools(&mut self) -> &mut Self {
        self.allow_tools = qview_application::tools::ALL_TOOL_NAMES_WITH_WRITES
            .iter()
            .map(|s| s.to_string())
            .collect();
        self
    }

    /// 是否允许某工具（worker_finish 总是允许）。
    pub fn allows(&self, tool: &str) -> bool {
        tool == contexa_context::FINISH_TOOL_NAME || self.allow_tools.iter().any(|t| t == tool)
    }
}

/// Agent 装配所需的 qview-application 服务（UI 层已经建好）。
#[derive(Clone)]
pub struct AgentDeps {
    pub docs: Arc<DocumentService>,
    pub search: Arc<SearchService>,
    pub annotations: Arc<AnnotationService>,
    /// UI 每帧发布的共享视口快照（`get_viewport` 工具读；不提供则传默认空）。
    pub viewport: qview_application::tools::SharedViewport,
    /// 本地结构化存储（AI 会话/消息落盘）。`None` = 不持久化。
    pub store: Option<Arc<dyn qview_store::Storage>>,
}

impl std::fmt::Debug for AgentDeps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentDeps")
            .field("docs_open", &self.docs.len())
            .field("annotations", &self.annotations.total_count())
            .field("store", &self.store.is_some())
            .finish()
    }
}

impl AgentConfig {
    /// 一站式装配：把配置变成 `AgentRuntimeHandle`。
    ///
    /// 内部完成（UI 无需关心）：
    /// 1. provider → LLMClient
    /// 2. ToolRegistry + GuardedTool 写工具（按 allow_tools / require_approval）
    /// 3. ReActWorker（限额 / 压缩 / 预算 / 记忆 / hooks）
    /// 4. ApprovalRegistry + AuditHook（file / memory）
    /// 5. AgentRuntime
    pub fn build(self, deps: AgentDeps) -> anyhow::Result<Arc<crate::handle::AgentRuntimeHandle>> {
        // 1) LLM client
        let llm = self.provider.build_client()?;

        // 2) 权限策略 + 工具注册
        let policy = qview_application::protocol::PermissionPolicy {
            allow_tools: self.allow_tools.clone(),
            require_approval: self.require_approval.clone(),
            max_read_lines: self.max_read_lines,
            max_tool_calls: self.max_tool_calls,
            max_token_budget: self.max_token_budget,
            max_tool_rounds: self.max_tool_rounds,
            max_wall_seconds: self.max_wall_seconds,
            tool_result_max_chars: self.tool_result_max_chars,
            max_tool_workers: self.max_tool_workers,
            tool_timeout_secs: 30,
            redact_patterns: self.redact_patterns.clone(),
        };

        let mut registry = qview_application::tool::ToolRegistry::new(policy.clone());

        // 需要审批的写工具名（按 require_approval 决定）：
        // 覆盖的 → 不注册为普通工具、改走 GuardedTool；未覆盖的 → 普通 LocalTool 自动放行。
        const WRITE_TOOLS: &[(&str, SideEffect)] = &[
            ("annotate_create", SideEffect::Reversible),
            ("annotate_update", SideEffect::Reversible),
            ("annotate_delete", SideEffect::Reversible),
            ("export_report", SideEffect::Mutating),
            ("write_document", SideEffect::Mutating),
        ];
        let guard_names: Vec<&str> = WRITE_TOOLS
            .iter()
            .filter(|(_, s)| policy.needs_approval(*s))
            .map(|(n, _)| *n)
            .collect();
        qview_application::tools::register_defaults(
            &mut registry,
            deps.docs.clone(),
            deps.search.clone(),
            Some(deps.annotations.clone()),
            deps.viewport.clone(),
            &guard_names,
        )?;

        // 注册完后包成 Arc——主 worker 和委派子 worker 共用这一份实例（保证权限/脱敏一致）
        let registry: Arc<qview_application::tool::ToolRegistry> = Arc::new(registry);

        // 3) ApprovalRegistry + GuardedTool 写工具（共享 WeakSinks，审批事件才能到达 UI）
        let approvals = Arc::new(crate::ApprovalRegistry::new());
        let sinks = crate::sink_hook::WeakSinks::new();
        let write_sources = crate::builder::make_guarded_sources(
            deps.annotations.clone(),
            deps.docs.clone(),
            approvals.clone(),
            &guard_names,
            sinks.clone(),
        )?;

        // 4) Audit sink
        let audit: Arc<dyn AuditSink> = match &self.audit_dir {
            Some(dir) => FileAuditSink::with_dir(dir.clone())?,
            None => InMemoryAuditSink::new(),
        };

        // 5) Worker 配置（含压缩 / 预算）
        let mut worker_config = policy.to_worker_config();
        worker_config.context_compress_enabled = self.context_compress_enabled;
        worker_config.context_budget_enabled = self.context_budget_enabled;
        // 压缩阈值：24+ 工具 schema + 系统提示词本身就近万 token，默认 16k 太紧。
        // 提到 40k：既防 168k 的无限膨胀，又不至于每轮都压。budget 目标随之提高。
        worker_config.max_context_tokens = 40_000;

        // 记忆（P2 用 InMemoryStore）
        let memory_store: Option<Arc<dyn contexa_memory::MemoryStore>> = if self.memory_enabled {
            Some(Arc::new(contexa_memory::InMemoryStore::new()))
        } else {
            None
        };

        // 6) ReActWorker（项目经理）
        // 系统提示词分层解析：内联覆盖 > 外部 md 文件 > 内置默认（含动态会话策略）。
        let system_prompt = crate::runtime::resolve_system_prompt(
            self.system_prompt.as_deref(),
            self.system_prompt_file.as_deref(),
        );
        let mut worker = contexa_core::ReActWorker::builder()
            .llm(llm.clone())
            .system_prompt(system_prompt.clone())
            .instance_id(self.instance_id.clone())
            .business_code(self.business_code.clone())
            .config(worker_config.clone())
            .build();
        worker.memory_store = memory_store.clone();
        // 注入 tools：registry + guarded 写工具（registry 已是 Arc，直接 clone 出 ToolSource）
        let registry_source = registry.as_arc_source();
        let mut sources: Vec<Arc<dyn contexa_tools::ToolSource>> = vec![registry_source];
        sources.extend(write_sources);
        worker.instance_sources = sources.clone();
        worker.validate()?;
        let worker = Arc::new(worker);

        // 7) 委派子 worker（员工）：与主 worker 共享 llm / system_prompt / 工具源 /
        //    memory_store / config；tool_filter = None 用全量工具。项目经理需要时把它
        //    派发出去（delegate_analysis），员工也是 worker。
        let delegate_worker: Option<Arc<contexa_core::ReActWorker>> = Some({
            let mut child = contexa_core::ReActWorker::builder()
                .llm(llm)
                .system_prompt(system_prompt)
                .instance_id(format!("qview-delegate-{}", uuid::Uuid::new_v4()))
                .business_code(self.business_code.clone())
                .config(worker_config)
                .build();
            child.memory_store = memory_store;
            child.instance_sources = sources;
            child.validate()?;
            Arc::new(child)
        });

        // 8) AgentRuntime → handle（注入共享 WeakSinks：UI 订阅 + GuardedTool 广播同一通道）
        //    store / provider / model 用于会话终态落盘（SessionMeta）。
        let provider_name = serde_json::to_string(&self.provider.provider)
            .ok()
            .map(|s| s.trim_matches('"').to_string())
            .unwrap_or_default();
        let (handle, _approvals) = AgentRuntime::new(
            worker,
            approvals,
            audit,
            sinks,
            deps.store,
            &provider_name,
            &self.provider.model,
            delegate_worker,
            self.classifier_context_chars,
            self.react_context_chars,
        );
        Ok(Arc::new(handle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_serde_round_trip() {
        let mut c = AgentConfig::mock("hi");
        c.provider.provider = LlmProvider::OpenAI;
        c.provider.model = "gpt-4o-mini".into();
        c.allow_tools = vec!["search_text".into()];
        let s = serde_json::to_string_pretty(&c).unwrap();
        let back: AgentConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(back.provider.provider, LlmProvider::OpenAI);
        assert_eq!(back.provider.model, "gpt-4o-mini");
        assert_eq!(back.allow_tools, vec!["search_text"]);
    }

    #[test]
    fn provider_defaults() {
        let p = ProviderConfig::default();
        assert_eq!(p.provider, LlmProvider::Mock);
    }

    #[test]
    fn provider_build_mock() {
        let p = ProviderConfig {
            provider: LlmProvider::Mock,
            mock_static: Some("ok".into()),
            ..Default::default()
        };
        let client = p.build_client().unwrap();
        // DummyLLM 实现 LLMClient；此处只验证构造成功
        let _ = client;
    }

    #[test]
    fn provider_build_openai_requires_model() {
        let p = ProviderConfig {
            provider: LlmProvider::OpenAI,
            ..Default::default()
        };
        assert!(p.build_client().is_err());
    }

    #[test]
    fn provider_build_ollama_no_auth() {
        let p = ProviderConfig {
            provider: LlmProvider::Ollama,
            model: "llama3".into(),
            ..Default::default()
        };
        let client = p.build_client().unwrap();
        let _ = client;
    }

    #[test]
    fn allows_always_includes_finish() {
        let c = AgentConfig::mock("x");
        assert!(c.allows(contexa_context::FINISH_TOOL_NAME));
        assert!(!c.allows("search_text")); // 未在 allow_tools
    }

    #[test]
    fn api_key_prefers_stored_over_env() {
        // 设置页填写的 key 优先；env 回退
        let p = ProviderConfig {
            api_key: Some("sk-stored".into()),
            api_key_env: Some("QVIEW_TEST_KEY".into()),
            ..Default::default()
        };
        assert_eq!(p.api_key().as_deref(), Some("sk-stored"));

        // 未填 → 从 env 读
        std::env::set_var("QVIEW_TEST_KEY", "sk-env");
        let p2 = ProviderConfig {
            api_key: None,
            api_key_env: Some("QVIEW_TEST_KEY".into()),
            ..Default::default()
        };
        assert_eq!(p2.api_key().as_deref(), Some("sk-env"));
        std::env::remove_var("QVIEW_TEST_KEY");
    }

    #[test]
    fn config_serde_keeps_api_key() {
        let mut c = AgentConfig::mock("hi");
        c.provider.provider = LlmProvider::OpenAI;
        c.provider.model = "gpt-4o-mini".into();
        c.provider.api_key = Some("sk-secret".into());
        let s = serde_json::to_string_pretty(&c).unwrap();
        let back: AgentConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(back.provider.api_key.as_deref(), Some("sk-secret"));
    }

    #[test]
    fn provider_default_reasoning_effort_is_low() {
        let p = ProviderConfig::default();
        assert_eq!(
            p.reasoning_effort.as_deref(),
            Some("low"),
            "默认 low：推理模型在 qview 这种读全文 + 工具调用场景下深度思考收益低、延迟高"
        );
    }

    #[test]
    fn provider_reasoning_effort_round_trip() {
        let p = ProviderConfig {
            reasoning_effort: Some("high".into()),
            ..Default::default()
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("\"reasoning_effort\":\"high\""), "{s}");
        let back: ProviderConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(back.reasoning_effort.as_deref(), Some("high"));
    }

    /// 缺省字段应回退到默认 "low"（旧配置文件没有 reasoning_effort 时不爆炸）。
    #[test]
    fn provider_reasoning_effort_back_compat_default_is_low() {
        let json = r#"{"provider":"mock","model":"x","api_key":null}"#;
        let p: ProviderConfig = serde_json::from_str(json).unwrap();
        assert_eq!(p.reasoning_effort.as_deref(), Some("low"));
    }

    /// 显式 "none" 应被 `build_client` 视为关闭（不注入 reasoning_effort）。
    #[test]
    fn provider_reasoning_effort_none_is_off() {
        let p = ProviderConfig {
            reasoning_effort: Some("none".into()),
            ..Default::default()
        };
        let client = p.build_client().expect("build mock client");
        // 客户端类型是 ReasoningEffortClient<dyn>；仅验证构造成功。
        let _ = client;
    }
}
