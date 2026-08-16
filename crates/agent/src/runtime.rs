//! AgentRuntime：包装 `ReActWorker` + 启动后台任务（架构 §8.4）。
//!
//! 关键设计：
//! - Worker 不可变；同一实例顺序 / 并发 run 任意多个 task
//! - 每个 session 启动时构造 `QviewSinkHook` + `AuditHook`，注入到 ReActWorker
//! - 通过 `tokio::sync::Notify` / `Notify::notified` + `tokio::time::timeout`
//!   实现"1s 内取消"语义
//! - 终态翻译在 `QviewSinkHook::on_task_end` 完成（避免双重翻译）

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use contexa_core::{ReActWorker, Task, TaskSpec};
use contexa_multiagent::{make_delegation_tool, Delegation};
use contexa_tools::ToolSource;

use qview_application::protocol::{PermissionPolicy, ProposalId};

use crate::approval::ApprovalRegistry;
use crate::audit::AuditSink;
use crate::event::{AgentEvent, AgentSink, SessionId, SubscriptionGuard};
use crate::handle::{AgentError, AgentGoal, AgentRuntimeHandle};
use crate::intent::{IntentKind, IntentRouter};
use crate::proposal::ProposalDecision;
use crate::sink_hook::{emit_message_emitted, QviewSinkHook, WeakSinks};

/// Active session 的取消令牌（`run_with_cancel` 每轮检查）。
struct ActiveSession {
    cancel: CancellationToken,
}

/// 共享给 handle 的 Runtime 内部状态。
pub struct AgentRuntimeInner {
    pub(crate) worker: Arc<ReActWorker>,
    sinks: WeakSinks,
    approvals: Arc<ApprovalRegistry>,
    audit: Arc<dyn AuditSink>,
    /// session_id → 取消信号（Arc 以便任务结束回调里清理注册）。
    active: Arc<Mutex<HashMap<SessionId, ActiveSession>>>,
    /// instance_id（qview-agent-<uuid>）。
    instance_id: String,
    /// 本地存储（会话终态落盘）。`None` = 不持久化。
    store: Option<Arc<dyn qview_store::Storage>>,
    /// LLM provider 名（SessionMeta 落盘用）。
    provider: String,
    /// LLM 模型名（SessionMeta 落盘用）。
    model: String,
    /// 委派子 worker（项目经理把复杂独立子任务派发给它，contexa-multiagent Delegation）。
    delegate_worker: Option<Arc<ReActWorker>>,
    /// 意图分类器可见的对话历史上限（字符数；分类是轻任务，窗口小）。
    classifier_context_chars: usize,
    /// ReAct 完整推理可见的对话历史上限（字符数；截断由 runtime 按阶段做）。
    react_context_chars: usize,
}

impl std::fmt::Debug for AgentRuntimeInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRuntimeInner")
            .field("instance_id", &self.instance_id)
            .field("active", &self.active.lock().len())
            .finish()
    }
}

/// AgentRuntime：仅在 builder 阶段用；构造完转 `AgentRuntimeHandle`。
pub struct AgentRuntime {
    inner: Arc<AgentRuntimeInner>,
}

impl std::fmt::Debug for AgentRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRuntime").finish()
    }
}

impl AgentRuntime {
    /// 用 worker + approvals + audit 构造。
    ///
    /// **重要**：`approvals` 必须与注入 GuardedTool 的是**同一个** `ApprovalRegistry`，
    /// 否则停止按钮的 `cancel_all()` 清的是另一个空的 registry，GuardedTool 的
    /// 审批等待永远无法被解除 → 工具卡死（用户实测"调用工具一直不结束、停止无效"）。
    ///
    /// `delegate_worker`：委派子 worker（项目经理派发复杂独立子任务；`None` = 不派发）。
    /// `classifier_context_chars` / `react_context_chars`：按阶段截断对话历史（字符数）。
    pub fn new(
        worker: Arc<ReActWorker>,
        approvals: Arc<ApprovalRegistry>,
        audit: Arc<dyn AuditSink>,
        sinks: WeakSinks,
        store: Option<Arc<dyn qview_store::Storage>>,
        provider: &str,
        model: &str,
        delegate_worker: Option<Arc<ReActWorker>>,
        classifier_context_chars: usize,
        react_context_chars: usize,
    ) -> (AgentRuntimeHandle, Arc<ApprovalRegistry>) {
        let instance_id = format!("qview-agent-{}", uuid::Uuid::new_v4());
        let inner = Arc::new(AgentRuntimeInner {
            worker,
            sinks,
            approvals: approvals.clone(),
            audit,
            active: Arc::new(Mutex::new(HashMap::new())),
            instance_id,
            store,
            provider: provider.to_string(),
            model: model.to_string(),
            delegate_worker,
            classifier_context_chars,
            react_context_chars,
        });
        let handle = AgentRuntimeHandle {
            inner: Arc::clone(&inner),
        };
        (handle, approvals)
    }

    /// 转 handle。
    pub fn handle(&self) -> AgentRuntimeHandle {
        AgentRuntimeHandle {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl AgentRuntimeInner {
    /// 启动一次任务（新建会话）。
    pub async fn start_session(&self, goal: AgentGoal) -> Result<SessionId, AgentError> {
        self.start_session_with(goal, None, None).await
    }

    /// 启动 / 继续一次任务。
    ///
    /// **多轮对话（AI 标准会话流）**：GUI 一次对话共用一个 `session_id`。
    /// - `session_id = Some`：**复用**既有会话（同一历史会话继续），并清空旧任务取消态；
    ///   `None`：生成新会话。
    /// - `conversation_history`：前几轮的 User/Agent 文本块，注入 `TaskSpec.context_hints`
    ///   （LLM 上下文可见前几轮，否则每轮都是"失忆"的新对话）。
    ///
    /// **项目经理前置**（意图层）：整个执行单元只有一个 ReActWorker（项目经理）。
    /// 1. `IntentRouter::classify(query)`：项目经理先分析需求 → 制定 `plan` → 分类
    /// 2. `Chat` 且 LLM 给了 reply → **短路**：直接模板回复 + SessionFinished
    /// 3. 其他意图：把 Intent（kind/params/plan）注入 context_hints；按 `suggested_tools`
    ///    设置 `worker.tool_filter`（永远追加 report_progress + delegate_analysis 两个管理工具）
    /// 4. 复杂独立子任务 → `delegate_analysis` 派发给子 ReActWorker 员工执行
    pub async fn start_session_with(
        &self,
        goal: AgentGoal,
        session_id: Option<SessionId>,
        conversation_history: Option<String>,
    ) -> Result<SessionId, AgentError> {
        let session_id = session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // 广播 SessionStarted
        self.sinks.broadcast(AgentEvent::SessionStarted {
            session_id: session_id.clone(),
            goal: goal.query.clone(),
            instance_id: self.instance_id.clone(),
        });

        // ── 意图 Router 前置（LLM 驱动：项目经理先分析需求 → 制定计划）──
        // 每次对话至少 1 次 LLM（分类就是那 1 次）——符合"和 LLM 交流必有 LLM"。
        // 分类是轻任务：对话历史截到 classifier_context_chars，窗口小、决策快。
        let classifier_history = conversation_history
            .as_deref()
            .map(|h| truncate_chars(h, self.classifier_context_chars));
        let intent = IntentRouter::classify(
            &self.worker.llm,
            &goal.query,
            classifier_history.as_deref(),
        ).await;

        // 构造当前 session 的 QviewSinkHook（store / goal / file / provider / model 供会话落盘）。
        // 提前到 Chat 判断之前：Chat 短路路径也要用它把「问题 + 回复」落库，
        // 否则纯聊天轮次在历史会话里一条都查不到（用户实测）。
        let sink_hook = Arc::new(QviewSinkHook::new(
            session_id.clone(),
            self.sinks.clone(),
            Arc::clone(&self.approvals),
            self.store.clone(),
            goal.query.clone(),
            goal.document_path.clone(),
            self.provider.clone(),
            self.model.clone(),
        ));

        if let Some(reply) = intent.reply.as_ref() {
            // Chat 意图：LLM 直接给出了回复正文 → 广播 + 落库 + 结束，不进 ReAct
            let reply = reply.clone();
            self.sinks.broadcast(AgentEvent::MessageEmitted {
                session_id: session_id.clone(),
                role: crate::event::Role::Assistant,
                text: reply.clone(),
            });
            self.sinks.broadcast(AgentEvent::SessionFinished {
                session_id: session_id.clone(),
                status: contexa_core::WorkerStatus::Success,
                summary: reply.clone(),
            });
            sink_hook.persist_chat(&goal.query, &reply).await;
            return Ok(session_id);
        }

        // ── 构造 Task（context_hints：文档上下文 + 对话历史 + 项目经理计划）───
        let mut spec = TaskSpec::simple(&goal.name, &goal.goal);
        spec.success_criteria = goal.success_criteria.unwrap_or_default();
        let mut hints: Vec<String> = Vec::new();
        if let Some(doc_ctx) = &goal.document_context {
            hints.push(doc_ctx.clone());
        }
        if let Some(hist) = conversation_history {
            if !hist.trim().is_empty() {
                // ReAct 完整推理窗口更大，但也按 react_context_chars 截断防止无限膨胀
                let hist = truncate_chars(&hist, self.react_context_chars);
                hints.push(format!(
                    "## 对话历史（本会话之前几轮，供你延续上下文；不要重复已回答的内容）\n{hist}"
                ));
            }
        }
        // 注入意图元信息（让 LLM 知道"我被 router 分到了哪类" + 项目经理的执行计划）
        if !matches!(intent.kind, IntentKind::Unknown | IntentKind::Chat) {
            let mut intent_hint = format!("## 意图分类（来自 router）\n- kind: {}\n- confidence: {:.2}", intent.kind.as_str(), intent.confidence);
            if !intent.params.is_empty() {
                intent_hint.push_str("\n- params:");
                for (k, v) in &intent.params {
                    intent_hint.push_str(&format!("\n  - {}: {}", k, v));
                }
            }
            if let Some(plan) = &intent.plan {
                intent_hint.push_str(&format!("\n- plan: {plan}"));
            }
            hints.push(intent_hint);
        }
        spec.context_hints = hints;
        let task = Task::new(session_id.clone(), spec, goal.query.clone());

        // 注册取消令牌（executor 主循环每轮检查；cancel 后立即终止）。
        // 复用会话时这里覆盖旧任务（同一会话同时只跑一个任务，GUI 侧有 active 闸门）。
        let cancel = CancellationToken::new();
        self.active.lock().insert(
            session_id.clone(),
            ActiveSession { cancel: cancel.clone() },
        );

        // AuditHook 在构造时不需要 session_id，post_tool_call 时通过 ctx.task_id 拿到
        // （sink_hook 已在分类之后构造，供 Chat 短路与 ReAct 两路径共用）
        let audit_hook = Arc::new(crate::audit::AuditHook::new(Arc::clone(&self.audit)));

        // 把 hooks 注入到 worker（注意：worker 是不可变的；我们要么新 clone 一个 worker，要么
        // 把 hooks 共享池改用 Arc<RwLock<Vec<Arc<dyn Hook>>>>——P2 简化：构造期注入。
        // 当前实现：每次 start_session 都重新构造一份新的 ReActWorker 副本（worker.config
        // 等字段共享；hooks 单独）。
        let mut worker = (*self.worker).clone();
        {
            let mut hooks = worker.hooks.clone();
            hooks.push(sink_hook.clone());
            hooks.push(audit_hook.clone());
            worker.hooks = hooks;
        }
        // 委派子 worker：clone 子 ReActWorker，注入本 session 的 sink/audit hook，
        // 包成 delegate_analysis 工具加进父 worker 的 instance_sources（员工是 worker）。
        if let Some(delegate_worker) = &self.delegate_worker {
            let mut child = (**delegate_worker).clone();
            // 子 worker 用独立 hook 实例（delegate 模式：只广播工具事件 / 进度，
            // 不广播会话终态、不改 phase、不落盘），避免共享 in_flight 队列错配。
            let child_hook = Arc::new(QviewSinkHook::delegate(
                session_id.clone(),
                self.sinks.clone(),
                self.store.clone(),
                self.provider.clone(),
                self.model.clone(),
            ));
            let mut child_hooks = child.hooks.clone();
            child_hooks.push(child_hook);
            child_hooks.push(audit_hook.clone());
            child.hooks = child_hooks;
            let child = Arc::new(child);
            let delegation = Delegation::new(
                "delegate_analysis",
                "把复杂独立子任务派发给子 worker 员工执行，返回其结果。优先用原子工具（read_context / search_text 等）完成简单步骤；只有当子任务足够复杂、独立、值得一次独立推理时再用本工具。",
                child,
            );
            match make_delegation_tool(delegation) {
                Ok(tool) => {
                    worker
                        .instance_sources
                        .push(tool as Arc<dyn ToolSource>);
                }
                Err(e) => {
                    warn!(
                        target: "qview_agent",
                        "构造 delegate_analysis 工具失败（{e}），跳过委派能力"
                    );
                }
            }
        }
        // 工具筛选：按意图的 suggested_tools 设置 worker.tool_filter
        // （worker_finish 永远保留——已由 effective_tools 保证）
        // 项目经理永远看得见「汇报进度 + 派发子任务」两个管理工具：
        // suggested_tools 非空时追加二者（与 worker_finish 同理）。
        let tool_filter: Option<Vec<String>> = if intent.suggested_tools.is_empty() {
            None
        } else {
            let mut names: Vec<String> = intent
                .suggested_tools
                .iter()
                .map(|s| s.to_string())
                .collect();
            names.push("report_progress".to_string());
            names.push("delegate_analysis".to_string());
            Some(names)
        };
        worker.tool_filter = tool_filter;
        let worker = Arc::new(worker);

        // 后台 spawn worker.run_with_cancel（可取消）
        let sinks = self.sinks.clone();
        let session_id_clone = session_id.clone();
        let approvals = Arc::clone(&self.approvals);
        let active = Arc::clone(&self.active);
        tokio::spawn(async move {
            let result = worker.run_with_cancel(task, &cancel).await;

            // 终态翻译已在 QviewSinkHook::on_task_end 完成；这里只清理 approvals
            approvals.cancel_all();
            // 清理 active 注册（任务结束；同一会话后续任务会重新插入）
            active.lock().remove(&session_id_clone);
            let _ = (result, sinks);
        });

        Ok(session_id)
    }

    /// 取消 session：令牌 cancel 后，executor 在下一轮迭代前 / LLM 返回后终止
    /// （已取消 → `WorkerResult::Timeout(note="cancelled…")` → `AgentEvent::Cancelled`）。
    pub async fn cancel(&self, session_id: SessionId) {
        let token = self.active.lock().remove(&session_id).map(|s| s.cancel);
        if let Some(token) = token {
            token.cancel();
            // 也清掉 pending approvals
            self.approvals.cancel_all();
        } else {
            warn!(target: "qview_agent", "cancel 未找到 session: {session_id}");
        }
    }

    /// 订阅。
    pub fn subscribe(&self, sink: Arc<dyn AgentSink>) -> SubscriptionGuard {
        let weak = std::sync::Arc::downgrade(&sink);
        self.sinks.push(weak);
        SubscriptionGuard::new(&sink)
    }

    /// 提案决策。
    pub async fn proposal_decision(
        &self,
        proposal_id: ProposalId,
        decision: ProposalDecision,
    ) -> Result<(), AgentError> {
        self.approvals
            .complete(proposal_id, decision)
            .map_err(AgentError::InvalidArgument)
    }

    pub fn active_sessions(&self) -> usize {
        self.active.lock().len()
    }
}

/// 静态分层系统提示词（外部可编辑的 md 文件，`include_str!` 编译期内嵌）。
///
/// 文件：`agent/prompts/system_prompt.md`。它是内置默认 + 首次运行时 seed 到
/// `data/system_prompt.md` 的源；运行时优先读外部文件，见 [`resolve_system_prompt`]。
pub fn static_system_prompt() -> &'static str {
    include_str!("../prompts/system_prompt.md")
}

/// qview 默认系统提示词（静态分层内容 + 动态「当前会话策略」）。
///
/// 暴露为 `pub` 供 `AgentConfig::build` 与 GUI seed 外部文件使用；
/// 也可通过 `AgentConfig.system_prompt` / `system_prompt_file` 覆盖，见 [`resolve_system_prompt`]。
pub fn default_system_prompt() -> String {
    append_policy_section(static_system_prompt().to_string())
}

/// 解析生效的系统提示词（**分层优先级**）：
///
/// 1. `inline` 显式内联覆盖（config `system_prompt`，非空才生效）
/// 2. `file` 外部文件（`system_prompt_file`，可读且非空；这是编辑测试的主入口）
/// 3. 内置默认（`static_system_prompt()`）
///
/// 外部文件缺失 / 为空时**静默回退**内置默认，绝不让提示词文件损坏导致程序起不来。
/// 最后统一追加动态「当前会话策略」（真实限额，不写死在 md 里）。
pub fn resolve_system_prompt(inline: Option<&str>, file: Option<&std::path::Path>) -> String {
    let base = if let Some(s) = inline {
        if s.trim().is_empty() {
            static_system_prompt().to_string()
        } else {
            s.to_string()
        }
    } else if let Some(path) = file {
        match std::fs::read_to_string(path) {
            Ok(content) if !content.trim().is_empty() => content,
            _ => static_system_prompt().to_string(),
        }
    } else {
        static_system_prompt().to_string()
    };
    append_policy_section(base)
}

/// 解析本机用户主目录（`dirs::home_dir()`，跨平台：macOS/Linux 返回 `/Users/<user>` 或
/// `/home/<user>`，Windows 返回 `C:\Users\<user>`；优先 `$HOME`，macOS 还回退 Foundation）。
///
/// 返回 `None` 表示无法解析主目录（如无家目录的受限环境）。
pub fn host_home_dir() -> Option<String> {
    dirs::home_dir().map(|p| p.to_string_lossy().into_owned())
}

/// 组装「本机环境」提示段（含真实主目录），供 LLM 拼绝对路径时参考。
///
/// 注入的是**运行时真实值**，不写死任何平台 / 用户名 / 主机名：任何机器上都能解析出
/// 该用户自己的主目录，从而避免 LLM 从主机名或昵称猜用户名导致路径错误。
/// 返回 `None` 表示无法解析主目录（此时不注入）。
pub fn home_env_hint() -> Option<String> {
    host_home_dir().map(|home| {
        format!(
            "## 本机环境\n\
             - 用户主目录：{home}\n\
             - 用户说\"桌面\"、\"下载\"、\"文档\"、\"Projects 目录\"等相对位置时，\
             基于此主目录拼绝对路径；**不要**用主机名或昵称猜用户名。"
        )
    })
}

/// 追加动态「当前会话策略」小节（用真实限额；避免把可能变化的数字写死在 md 里）。
fn append_policy_section(mut s: String) -> String {
    s.push_str("\n## 当前会话策略\n");
    let policy = PermissionPolicy::default();
    s.push_str(&format!(
        "- 最大读取行数：{}\n- 工具调用上限：{}\n- 脱敏模式：{} 个\n",
        policy.max_read_lines,
        policy.max_tool_calls,
        policy.redact_patterns.len()
    ));
    // 注入本机环境：让 LLM 拼绝对路径时用真实主目录，而不是猜用户名。
    if let Some(hint) = home_env_hint() {
        s.push_str(&format!("\n{hint}\n"));
    }
    s
}

/// 重新导出供 tests 用。
#[doc(hidden)]
pub fn _emit_message_emitted(sinks: &WeakSinks, session_id: &str, resp: &contexa_llm::LLMResponse) {
    emit_message_emitted(sinks, session_id, resp);
}

/// 按字符数截断文本（保留头部，尾部提示省略）。`n=0` → 空。
///
/// 用于按阶段限制 LLM 上下文：分类器窗口小（classifier_context_chars）、
/// ReAct 完整推理窗口大（react_context_chars）。
pub fn truncate_chars(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push_str(&format!("\n…[内容过长已截断，共 {} 字符]", s.chars().count()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_md(name: &str, content: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!(
            "qview-sp-{name}-{}-{nanos}.md",
            std::process::id()
        ));
        std::fs::write(&p, content).unwrap();
        p
    }

    /// 内置默认：静态分层内容 + 动态策略段；软件介绍章节存在。
    #[test]
    fn default_prompt_has_layered_sections_and_policy() {
        let def = default_system_prompt();
        assert!(def.contains("## 一、角色定义"), "角色定义章节");
        assert!(def.contains("## 二、软件介绍：qview"), "软件介绍章节");
        assert!(def.contains("## 五、日志分析处理流程"), "处理流程章节");
        assert!(def.contains("## 十、安全与隐私协议"), "安全隐私章节");
        assert!(def.contains("## 当前会话策略"), "动态追加策略段");
        assert!(def.contains("最大读取行数"), "策略含真实限额");
    }

    /// 系统提示词注入本机环境（真实主目录），让 LLM 拼绝对路径时不猜用户名。
    #[test]
    fn default_prompt_injects_home_env_section() {
        let def = default_system_prompt();
        assert!(def.contains("## 本机环境"), "应注入本机环境段");
        if let Some(home) = host_home_dir() {
            assert!(def.contains(&format!("用户主目录：{home}")), "应含真实主目录");
        }
        assert!(def.contains("猜用户名"), "应警告别用主机名猜用户名");
    }

    /// 优先级：内联 > 外部文件 > 内置默认；文件缺失/为空静默回退。
    #[test]
    fn resolve_precedence_and_fallback() {
        // 1) 无覆盖 → 内置默认
        let def = resolve_system_prompt(None, None);
        assert!(def.contains("## 一、角色定义"));
        assert!(!def.starts_with("# 自定义"));

        // 2) 外部文件 → 优先于内置
        let p = tmp_md("file", "# 自定义\n\n## 测试章节\n内容");
        let from_file = resolve_system_prompt(None, Some(&p));
        assert!(from_file.starts_with("# 自定义"), "外部文件内容优先");
        assert!(from_file.contains("## 当前会话策略"), "文件路径也追加策略");

        // 3) 内联 → 最高优先级
        let inline = resolve_system_prompt(Some("内联覆盖"), Some(&p));
        assert!(inline.starts_with("内联覆盖"));
        assert!(inline.contains("## 当前会话策略"));

        // 4) 空内联 / 缺失文件 / 空文件 → 回退内置
        assert!(resolve_system_prompt(Some("  "), None).contains("## 一、角色定义"));
        assert!(resolve_system_prompt(None, Some(&PathBuf::from("no_such_file.md"))).contains("## 一、角色定义"));
        let empty = tmp_md("empty", "   \n");
        assert!(resolve_system_prompt(None, Some(&empty)).contains("## 一、角色定义"));

        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(empty);
    }
}
