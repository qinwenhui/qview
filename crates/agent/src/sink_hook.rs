//! QviewSinkHook：把 `contexa::Hook` 的 7 个点翻译为 `AgentEvent`（架构 §8.4）。
//!
//! 实现策略：
//! - 每个 session 持有一个 `WeakSinks`（订阅者 Weak 列表）+ session_id
//! - 每个工具调用生成 `ToolCallId` 并通过 `WeakSinks` 广播
//! - 从 `ToolResult.content["view_intents"]` 解析 `ViewIntent` → 广播 `ViewIntentEmitted`
//! - 检测 `is_error + content.error == "approval_required"` → 广播 `ApprovalRequired`
//! - `on_task_end` 解析 WorkerResult → 广播 `SessionFinished / Failed / Cancelled`

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use parking_lot::RwLock;

use contexa_context::Tier;
use contexa_core::{WorkerResult, WorkerStatus};
use contexa_hooks::{Hook, TaskContext};
use contexa_llm::LLMResponse;
use contexa_tools::ToolResult;
use serde_json::Value;

use qview_application::protocol::{ProposalId, ToolCallId, ViewIntent};

use crate::approval::ApprovalRegistry;
use crate::event::{AgentEvent, AgentSink, Phase, Role, SessionId};
use crate::proposal::Proposal;

/// Weak 化的 sink 列表（订阅者 drop 后自动失效）。
#[derive(Clone, Default)]
pub struct WeakSinks {
    inner: Arc<RwLock<Vec<std::sync::Weak<dyn AgentSink>>>>,
}

impl WeakSinks {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, sink: std::sync::Weak<dyn AgentSink>) {
        self.inner.write().push(sink);
    }

    /// 广播一条事件（自动清理已 drop 的 sink）。
    pub fn broadcast(&self, event: AgentEvent) {
        let mut sinks = self.inner.write();
        // 先清理
        sinks.retain(|w| w.strong_count() > 0);
        for w in sinks.iter() {
            if let Some(s) = w.upgrade() {
                s.on_event(event.clone());
            }
        }
    }

    pub fn len(&self) -> usize {
        self.inner.read().iter().filter(|w| w.strong_count() > 0).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::fmt::Debug for WeakSinks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WeakSinks")
            .field("alive", &self.len())
            .finish()
    }
}

/// QviewSinkHook：每个 session 一个实例。
pub struct QviewSinkHook {
    session_id: SessionId,
    sinks: WeakSinks,
    /// delegate 模式（子 worker 用）：只广播工具事件 / 进度，
    /// 不广播会话终态、不改 phase、不落盘。
    delegate: bool,
    /// 当前 Phase。
    phase: RwLock<Phase>,
    /// 当前正在执行的工具调用队列（call_id / start_time / name）。
    ///
    /// 并发工具（`execute_parallel_calls`）会先对所有调用发 `on_tool_call`，
    /// 再按**同一顺序**发 `post_tool_call`（join_all 保序）→ FIFO 恰好配对。
    /// 不能用单个槽 —— 否则并行时只有最后一个 (call_id, start, name) 幸存，
    /// 完成日志会串名。
    in_flight: RwLock<VecDeque<(ToolCallId, Instant, String)>>,
    /// approval registry（用于 peek — 决定是否要广播 ApprovalRequired）。
    approvals: Arc<ApprovalRegistry>,
    /// 本地存储（会话终态落盘）。`None` = 不持久化。
    store: Option<Arc<dyn qview_store::Storage>>,
    /// 会话目标（用户 query，落库 `SessionMeta.goal`）。
    goal: String,
    /// 会话关联文件（canonical path，落库 `file_id`）。
    file: Option<String>,
    /// LLM provider 名。
    provider: String,
    /// LLM 模型名。
    model: String,
}

impl std::fmt::Debug for QviewSinkHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QviewSinkHook")
            .field("session_id", &self.session_id)
            .finish()
    }
}

impl QviewSinkHook {
    pub fn new(
        session_id: SessionId,
        sinks: WeakSinks,
        approvals: Arc<ApprovalRegistry>,
        store: Option<Arc<dyn qview_store::Storage>>,
        goal: String,
        file: Option<String>,
        provider: String,
        model: String,
    ) -> Self {
        Self {
            session_id,
            sinks,
            delegate: false,
            phase: RwLock::new(Phase::Thinking),
            in_flight: RwLock::new(VecDeque::new()),
            approvals,
            store,
            goal,
            file,
            provider,
            model,
        }
    }

    /// delegate 模式构造（子 worker 用）：独立 in_flight 队列、共享 session_id + sinks。
    ///
    /// 子 worker 的工具事件（ToolCallStarted/Finished + report_progress 进度）照常进 GUI，
    /// 但**不**广播会话终态 / MessageEmitted、不切换 phase、不落盘——那都是项目经理的职责。
    pub fn delegate(
        session_id: SessionId,
        sinks: WeakSinks,
        store: Option<Arc<dyn qview_store::Storage>>,
        provider: String,
        model: String,
    ) -> Self {
        Self {
            session_id,
            sinks,
            delegate: true,
            phase: RwLock::new(Phase::Thinking),
            in_flight: RwLock::new(VecDeque::new()),
            approvals: Arc::new(ApprovalRegistry::new()),
            store,
            goal: String::new(),
            file: None,
            provider,
            model,
        }
    }

    /// 切换 phase 并广播事件。
    fn set_phase(&self, phase: Phase) {
        *self.phase.write() = phase;
        self.sinks.broadcast(AgentEvent::PhaseChanged {
            session_id: self.session_id.clone(),
            phase,
        });
    }

    /// 当前 phase。
    pub fn phase(&self) -> Phase {
        *self.phase.read()
    }

    /// 解析 ToolResult.content 里的 view_intents，广播。
    fn extract_view_intents(&self, content: &Value) {
        if let Some(arr) = content.get("view_intents").and_then(|v| v.as_array()) {
            for intent in arr {
                if let Ok(parsed) = serde_json::from_value::<ViewIntent>(intent.clone()) {
                    self.sinks.broadcast(AgentEvent::ViewIntentEmitted {
                        session_id: self.session_id.clone(),
                        intent: parsed,
                    });
                }
            }
        }
    }

    /// 把 `WorkerResult` 写入本地存储（meta + messages，单事务）。
    ///
    /// - 只存 User / Assistant / Tool 消息，**跳过 System / Developer**（系统提示词
    ///   是每会话同一段样板，落库只会撑大 DB）。
    /// - **多轮会话累积**：同一 `session_id` 会被连续任务复用（GUI 一次对话一个会话）。
    ///   先 `load_session` 读已有会话，再**追加**本轮新消息、累加 tokens/rounds，
    ///   `goal` 用最新一轮 query（历史列表标题 = 最近问的）。
    /// - `started_at` 由「最早的 finished_at - wall_seconds」推算（无需额外计时字段）。
    /// - 写盘在 `spawn_blocking`（后台阻塞线程池），await 等其完成保证崩溃前落盘。
    async fn persist_session(&self, wr: &WorkerResult) {
        let finished_at_ms = now_ms();
        let started_at_ms = finished_at_ms
            .saturating_sub((wr.wall_seconds * 1000.0).max(0.0) as u64);

        // 本轮新消息（seq 由 persist_messages 统一重排，这里先置 0）
        let mut new_msgs: Vec<qview_store::StoreMessage> = Vec::with_capacity(wr.messages.len());
        for m in wr.messages.iter() {
            if m.content.trim().is_empty() {
                continue;
            }
            let role = match m.tier {
                Tier::User => qview_store::StoreRole::User,
                Tier::Assistant => qview_store::StoreRole::Assistant,
                Tier::Tool => qview_store::StoreRole::Tool,
                Tier::System | Tier::Developer => continue,
            };
            new_msgs.push(qview_store::StoreMessage {
                role,
                content: m.content.clone(),
                seq: 0,
            });
        }
        // 模型最终回复正文（worker_finish.summary）补进消息列表：
        // ReAct 里模型经常只调 worker_finish、不带 assistant 正文（content 为空），
        // 摘要只落在 `summary` 字段 —— 不补的话历史回看就只有用户问题 + 工具调用，
        // 缺了器灵的最终回复（用户实测：点开会话只有最后一句用户问题）。
        let summary_text = wr
            .summary
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());
        if let Some(sum) = summary_text {
            let already_last = new_msgs
                .last()
                .map(|m| {
                    m.role == qview_store::StoreRole::Assistant && m.content.trim() == sum
                })
                .unwrap_or(false);
            if !already_last {
                new_msgs.push(qview_store::StoreMessage {
                    role: qview_store::StoreRole::Assistant,
                    content: sum.to_string(),
                    seq: 0,
                });
            }
        }
        // 本轮没有任何可落库消息（如仅 system/developer）→ 不更新
        if new_msgs.is_empty() {
            return;
        }

        let status = match wr.status {
            WorkerStatus::Success => qview_store::StoreStatus::Success,
            WorkerStatus::Failed => qview_store::StoreStatus::Failed,
            WorkerStatus::Timeout => {
                if wr.note.as_deref().map(|n| n.contains("cancel")).unwrap_or(false) {
                    qview_store::StoreStatus::Cancelled
                } else {
                    qview_store::StoreStatus::Timeout
                }
            }
            WorkerStatus::Empty => qview_store::StoreStatus::Empty,
        };

        let summary = wr
            .summary
            .clone()
            .unwrap_or_else(|| wr.note.clone().unwrap_or_default());

        self.persist_messages(
            new_msgs,
            started_at_ms,
            finished_at_ms,
            status,
            summary,
            wr.tokens_prompt,
            wr.tokens_completion,
            wr.rounds,
            wr.tool_calls_total,
        )
        .await;
    }

    /// Chat 短路路径（IntentRouter 直接给 reply，不进 ReAct）的会话落盘。
    ///
    /// 只存「用户问题 + 器灵回复」两条；复用 `persist_messages` 的 load-append 逻辑，
    /// 保证连续 Chat 轮次也累积到同一会话（否则纯聊天轮次在历史里一条都查不到）。
    pub(crate) async fn persist_chat(&self, query: &str, reply: &str) {
        let query = query.trim().to_string();
        let reply = reply.trim().to_string();
        if query.is_empty() || reply.is_empty() {
            return;
        }
        let finished_at_ms = now_ms();
        let new_msgs = vec![
            qview_store::StoreMessage {
                role: qview_store::StoreRole::User,
                content: query,
                seq: 0,
            },
            qview_store::StoreMessage {
                role: qview_store::StoreRole::Assistant,
                content: reply.clone(),
                seq: 0,
            },
        ];
        self.persist_messages(
            new_msgs,
            finished_at_ms,
            finished_at_ms,
            qview_store::StoreStatus::Success,
            reply,
            0,
            0,
            0,
            0,
        )
        .await;
    }

    /// 共享落盘核心：加载已有会话 → 追加本轮消息（重排 seq）→ 更新 meta → 单事务写盘。
    /// 由 `persist_session`（ReAct 终态）与 `persist_chat`（Chat 短路）共用。
    async fn persist_messages(
        &self,
        mut new_msgs: Vec<qview_store::StoreMessage>,
        started_at_ms: u64,
        finished_at_ms: u64,
        status: qview_store::StoreStatus,
        summary: String,
        tokens_prompt: u32,
        tokens_completion: u32,
        rounds: u32,
        tool_calls: u32,
    ) {
        let Some(store) = &self.store else { return };
        if new_msgs.is_empty() {
            return;
        }

        // 先加载该会话已有内容（多轮会话：追加而非覆盖）
        let existing = store.load_session(&self.session_id).ok().flatten();
        let meta = existing.as_ref().map(|s| s.meta.clone());
        let mut messages = existing.map(|s| s.messages).unwrap_or_default();
        // seq 接续现有消息数
        for (i, m) in new_msgs.iter_mut().enumerate() {
            m.seq = (messages.len() + i) as u64;
        }
        messages.extend(new_msgs);

        let mut m = meta.unwrap_or_else(|| qview_store::SessionMeta {
            id: self.session_id.clone(),
            started_at_ms,
            finished_at_ms,
            goal: self.goal.clone(),
            status,
            summary: summary.clone(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            file_id: self.file.clone(),
            tokens_prompt: 0,
            tokens_completion: 0,
            rounds: 0,
            tool_calls: 0,
        });
        // 多轮累积 / 更新
        m.goal = self.goal.clone();
        m.finished_at_ms = finished_at_ms;
        m.started_at_ms = m.started_at_ms.min(started_at_ms);
        m.status = status;
        m.summary = summary;
        m.provider = self.provider.clone();
        m.model = self.model.clone();
        m.file_id = self.file.clone();
        m.tokens_prompt = m.tokens_prompt.saturating_add(tokens_prompt);
        m.tokens_completion = m.tokens_completion.saturating_add(tokens_completion);
        m.rounds = m.rounds.saturating_add(rounds);
        m.tool_calls = m.tool_calls.saturating_add(tool_calls);

        let session = qview_store::StoredSession { meta: m, messages };
        let store = store.clone();
        let res = tokio::task::spawn_blocking(move || store.save_session(&session)).await;
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(target: "qview_agent", "会话落盘失败: {e:#}"),
            Err(e) => tracing::warn!(target: "qview_agent", "会话落盘任务被中断: {e}"),
        }
    }
}

#[async_trait]
impl Hook for QviewSinkHook {
    fn name(&self) -> &str {
        "qview-sink"
    }

    async fn on_task_start(&self, _ctx: &TaskContext<'_>) {
        if !self.delegate {
            self.set_phase(Phase::Thinking);
        }
    }

    async fn on_round_start(&self, _ctx: &TaskContext<'_>, _round: u32) {
        // 每轮开始 → Thinking
        if !self.delegate {
            self.set_phase(Phase::Thinking);
        }
    }

    async fn pre_llm_call(&self, _ctx: &TaskContext<'_>) {
        if !self.delegate {
            self.set_phase(Phase::Thinking);
        }
    }

    async fn post_llm_call(&self, ctx: &TaskContext<'_>) {
        // post_llm_call 在 contexa 里收到的是 `&TaskContext`，要拿 LLMResponse 需另寻。
        // contexa 的 post_llm_call 签名只接受 ctx；要拿 response 需要 hook chain 改造。
        // P2 简化：post_llm_call 仅在 on_round_start 之间做 phase 切换；消息文本由 on_task_end 汇总。
        let _ = ctx;
    }

    async fn on_tool_call(
        &self,
        _ctx: &TaskContext<'_>,
        name: &str,
        args: &serde_json::Value,
    ) {
        // report_progress：实时进度广播，不进 in_flight（避免工具气泡 / 日志噪音）。
        if name == "report_progress" {
            let text = args
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if !text.trim().is_empty() {
                self.sinks.broadcast(AgentEvent::ProgressNote {
                    session_id: self.session_id.clone(),
                    text,
                });
            }
            return;
        }

        // delegate 模式：不切换 phase（不让子 worker 的搜索/检视状态顶掉项目经理的）
        if !self.delegate {
            // 决定 phase（搜索 / 检视 / 视图）
            let phase = match name {
                "search_text" | "search_regex" => Phase::Searching,
                "read_context" | "summarize_range" | "inspect_matches" => Phase::Inspecting,
                "navigate_to_line" | "highlight_range" | "create_filter" => {
                    // view 类不切换 phase；保留最近一次
                    self.phase()
                }
                _ => self.phase(),
            };
            self.set_phase(phase);
        }

        // 分配 call_id 并入队（FIFO），广播 ToolCallStarted
        let call_id = ToolCallId::new();
        self.in_flight.write().push_back((call_id, Instant::now(), name.to_string()));
        self.sinks.broadcast(AgentEvent::ToolCallStarted {
            session_id: self.session_id.clone(),
            call_id,
            tool: name.to_string(),
            input: args.clone(),
        });

        // 如果 registry 里已经 pending 这个 session 的某个 proposal，
        // 触发 ApprovalRequired（让 UI 弹窗）。
        // delegate 模式跳过（子 worker 的工具不直接触发会话级审批事件）。
        if !self.delegate && self.approvals.has_pending() {
            // 由于当前 peek 不能定位到具体 proposal，这里用一个占位 id；
            // GuardedTool 自身负责把这个 proposal 写到 registry。
            // 真实 proposal_id 由 GuardedTool 提交后通过 ProposalCreated 事件告知 UI。
            let _ = ProposalId::new(); // 占位；UI 应忽略
        }
    }

    async fn post_tool_call(
        &self,
        _ctx: &TaskContext<'_>,
        name: &str,
        result: &ToolResult,
    ) {
        // report_progress 在 on_tool_call 已提前 return，不进队；这里也直接 return。
        if name == "report_progress" {
            return;
        }

        // 出队（FIFO）并广播 ToolCallFinished。
        // `name` 用 post_tool_call 参数（权威），不是队列里存的 —— 队列为空（异常）
        // 时也要能带上正确工具名。
        let (call_id, start) = match self.in_flight.write().pop_front() {
            Some((cid, st, _)) => (cid, st),
            None => (ToolCallId::new(), Instant::now()),
        };
        let duration_ms = start.elapsed().as_millis() as u64;
        let output_summary = if result.is_error {
            format!("error: {}", result.content.get("error").and_then(|v| v.as_str()).unwrap_or("?"))
        } else {
            result.as_text().chars().take(120).collect()
        };
        self.sinks.broadcast(AgentEvent::ToolCallFinished {
            session_id: self.session_id.clone(),
            call_id,
            tool: name.to_string(),
            output_summary,
            duration_ms,
            is_error: result.is_error,
        });

        // view_intents 解析
        self.extract_view_intents(&result.content);

        // delegate 模式：不做会话级审批检测（审批是项目经理的职责）。
        if self.delegate {
            return;
        }

        // approval_required 检测
        if result.is_error
            && result
                .content
                .get("error")
                .and_then(|v| v.as_str())
                .map(|s| s == "approval_required")
                .unwrap_or(false)
        {
            // proposal_id 由 GuardedTool 写入 result.content["proposal_id"]
            let pid = result
                .content
                .get("proposal_id")
                .and_then(|v| v.as_str())
                .and_then(|s| uuid::Uuid::parse_str(s).ok())
                .map(ProposalId)
                .unwrap_or_default();
            let reason = result
                .content
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            self.sinks.broadcast(AgentEvent::ApprovalRequired {
                session_id: self.session_id.clone(),
                proposal_id: pid,
                tool: name.to_string(),
                reason,
            });
            self.set_phase(Phase::AwaitingApproval);
        }
    }

    async fn on_task_end(&self, _ctx: &TaskContext<'_>, result: &serde_json::Value) {
        // delegate 模式：子 worker 完成不是会话结束——终态 / 落盘都是项目经理的职责。
        if self.delegate {
            return;
        }

        // result 是 WorkerResult 的 JSON 序列化（含全量上下文消息 `messages`，
        // 见 contexa-core `make_result`）。完整反序列化，拿全字段 + 最后一条回复。
        let wr: WorkerResult = match serde_json::from_value(result.clone()) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(target: "qview_agent", "on_task_end 解析 WorkerResult 失败: {e}");
                self.set_phase(Phase::Failed);
                self.sinks.broadcast(AgentEvent::Failed {
                    session_id: self.session_id.clone(),
                    error: format!("WorkerResult 解析失败: {e}"),
                });
                return;
            }
        };

        // ── 调试日志：worker 终态到底拿到什么（用户要求打全，便于排查）──
        tracing::info!(
            target: "qview_agent",
            task_id = %wr.task_id,
            status = ?wr.status,
            summary = ?wr.summary,
            note = ?wr.note,
            rounds = wr.rounds,
            tool_calls_total = wr.tool_calls_total,
            tokens_prompt = wr.tokens_prompt,
            tokens_completion = wr.tokens_completion,
            wall_seconds = wr.wall_seconds,
            data = ?wr.data,
            messages = wr.messages.len(),
            "Agent 会话终态（WorkerResult）"
        );

        // ── 广播模型在本任务中的**全部** assistant 正文（不只最后一条）→ 实时气泡。
        //    背景：原来只取最后一条 assistant 消息广播。若模型把真正答案写在更早的
        //    assistant 轮次（末尾只是"搞定啦主人～✨"这类收尾），实时窗口就只剩一句
        //    废话，完整对话只有在历史会话里才看得到（用户实测）。现在把每条非空
        //    assistant 正文都广播，实时视图与历史回看一致。
        //    与 summary 完全相同的跳过（summary 由 SessionFinished 带出，UI 端去重）。
        for m in wr.messages.iter() {
            if !matches!(m.tier, Tier::Assistant) {
                continue;
            }
            let text = m.content.trim();
            if text.is_empty() {
                continue;
            }
            let same_as_summary = wr
                .summary
                .as_deref()
                .map(|s| s.trim() == text)
                .unwrap_or(false);
            if same_as_summary {
                continue;
            }
            self.sinks.broadcast(AgentEvent::MessageEmitted {
                session_id: self.session_id.clone(),
                role: Role::Assistant,
                text: text.to_string(),
            });
        }

        // ── 终态翻译 ──
        match wr.status {
            WorkerStatus::Success => {
                self.set_phase(Phase::Done);
                // summary 缺失时回退到最后一条 assistant 正文
                let summary = wr.summary.clone().unwrap_or_else(|| {
                    wr.messages
                        .iter()
                        .rev()
                        .find(|m| {
                            matches!(m.tier, Tier::Assistant) && !m.content.trim().is_empty()
                        })
                        .map(|m| m.content.trim().to_string())
                        .unwrap_or_default()
                });
                self.sinks.broadcast(AgentEvent::SessionFinished {
                    session_id: self.session_id.clone(),
                    status: WorkerStatus::Success,
                    summary,
                });
            }
            WorkerStatus::Failed | WorkerStatus::Timeout | WorkerStatus::Empty => {
                let cancelled = wr.status == WorkerStatus::Timeout
                    && wr
                        .note
                        .as_deref()
                        .map(|n| n.contains("cancel"))
                        .unwrap_or(false);
                if cancelled {
                    self.set_phase(Phase::Cancelled);
                    self.sinks.broadcast(AgentEvent::Cancelled {
                        session_id: self.session_id.clone(),
                    });
                } else {
                    self.set_phase(Phase::Failed);
                    let reason = wr
                        .note
                        .clone()
                        .unwrap_or_else(|| wr.summary.clone().unwrap_or_default());
                    self.sinks.broadcast(AgentEvent::Failed {
                        session_id: self.session_id.clone(),
                        error: reason,
                    });
                }
            }
        }

        // ── 会话终态落盘（后台线程；失败不阻塞会话终态）──
        self.persist_session(&wr).await;
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 给定 LLMResponse 文本，构造 `MessageEmitted` 事件（供 runtime 在 post_llm_call 处调）。
pub fn emit_message_emitted(sinks: &WeakSinks, session_id: &str, resp: &LLMResponse) {
    if !resp.content.trim().is_empty() {
        sinks.broadcast(AgentEvent::MessageEmitted {
            session_id: session_id.to_string(),
            role: Role::Assistant,
            text: resp.content.clone(),
        });
    }
}

/// 把 Proposal 写到 registry + 广播 ProposalCreated + ApprovalRequired。
pub fn emit_proposal_created(
    sinks: &WeakSinks,
    proposal: Proposal,
) {
    sinks.broadcast(AgentEvent::ProposalCreated {
        session_id: proposal.session_id.clone(),
        proposal,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn weak_sinks_clean_up_dropped() {
        let sinks = WeakSinks::new();
        let s: Arc<dyn AgentSink> = Arc::new(CountSink::default());
        sinks.push(std::sync::Arc::downgrade(&s) as std::sync::Weak<dyn AgentSink>);
        assert_eq!(sinks.len(), 1);
        drop(s);
        sinks.broadcast(AgentEvent::PhaseChanged {
            session_id: "x".into(),
            phase: Phase::Thinking,
        });
        assert_eq!(sinks.len(), 0, "dropped sink should be cleaned");
    }

    #[derive(Default)]
    struct CountSink(Arc<parking_lot::Mutex<u32>>);
    impl std::fmt::Debug for CountSink {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("CountSink").finish()
        }
    }
    impl AgentSink for CountSink {
        fn on_event(&self, _event: AgentEvent) {
            *self.0.lock() += 1;
        }
    }

    #[derive(Default)]
    struct RecSink(Arc<parking_lot::Mutex<Vec<AgentEvent>>>);
    impl std::fmt::Debug for RecSink {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("RecSink")
                .field("events", &self.0.lock().len())
                .finish()
        }
    }
    impl AgentSink for RecSink {
        fn on_event(&self, event: AgentEvent) {
            self.0.lock().push(event);
        }
    }

    fn hook_with_sink(
        events: Arc<parking_lot::Mutex<Vec<AgentEvent>>>,
    ) -> (QviewSinkHook, Arc<RecSink>) {
        let sink = Arc::new(RecSink(events));
        let dyn_sink: Arc<dyn AgentSink> = sink.clone();
        let sinks = WeakSinks::new();
        sinks.push(Arc::downgrade(&dyn_sink));
        let hook = QviewSinkHook::new(
            "sess-1".into(),
            sinks,
            Arc::new(crate::approval::ApprovalRegistry::new()),
            None, // store
            "goal".into(),
            None,
            "mock".into(),
            "dummy".into(),
        );
        (hook, sink)
    }

    /// 带真实 redb store 的 hook（临时文件），测会话落盘。
    fn hook_with_store() -> (QviewSinkHook, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "qview-agent-sink-{}.db",
            uuid::Uuid::new_v4()
        ));
        let store: Arc<dyn qview_store::Storage> =
            Arc::new(qview_store::RedbStore::open(&path).unwrap());
        let sinks = WeakSinks::new();
        let hook = QviewSinkHook::new(
            "sess-persist".into(),
            sinks,
            Arc::new(crate::approval::ApprovalRegistry::new()),
            Some(store),
            "goal".into(),
            None,
            "mock".into(),
            "dummy".into(),
        );
        (hook, path)
    }

    /// 模型只调 worker_finish、没写 assistant 正文时，summary 也要作为最终回复落库。
    #[tokio::test]
    async fn persist_session_saves_finish_summary_as_assistant() {
        use contexa_context::Message;
        use contexa_core::WorkerResult;

        let mut wr = WorkerResult::success(
            "i",
            "qview",
            "t",
            None,
            Some("最终回复：文件共 5 亿行。".to_string()),
        );
        wr.messages = vec![
            Message::system("你是器灵"),
            Message::user("你能打开多大的文件"),
            Message::tool("{\"encoding\":\"UTF-8\"}".to_string(), "call_1".to_string()),
            Message::assistant(""), // 模型只调 worker_finish、无正文
        ];
        let result = serde_json::to_value(&wr).unwrap();

        let (hook, db_path) = hook_with_store();
        let ctx = contexa_hooks::TaskContext::empty();
        hook.on_task_end(&ctx, &result).await;
        drop(hook);

        let store = qview_store::open_store(&db_path).unwrap();
        let sess = store.load_session("sess-persist").unwrap().unwrap();
        let _ = std::fs::remove_file(&db_path);
        assert_eq!(sess.messages.len(), 3, "应为 User + Tool + Assistant(summary)");
        assert_eq!(sess.messages[0].role, qview_store::StoreRole::User);
        assert_eq!(sess.messages[1].role, qview_store::StoreRole::Tool);
        assert_eq!(sess.messages[2].role, qview_store::StoreRole::Assistant);
        assert!(sess.messages[2].content.contains("5 亿行"));
    }

    /// Chat 短路路径：问题 + 回复 都要落库（否则纯聊天轮次在历史里查不到）。
    #[tokio::test]
    async fn persist_chat_saves_user_and_reply() {
        let (hook, db_path) = hook_with_store();
        hook.persist_chat("你好", "你好呀～有什么想看的日志或文件吗？")
            .await;
        drop(hook);

        let store = qview_store::open_store(&db_path).unwrap();
        let sess = store.load_session("sess-persist").unwrap().unwrap();
        let _ = std::fs::remove_file(&db_path);
        assert_eq!(sess.messages.len(), 2);
        assert_eq!(sess.messages[0].role, qview_store::StoreRole::User);
        assert_eq!(sess.messages[0].content, "你好");
        assert_eq!(sess.messages[1].role, qview_store::StoreRole::Assistant);
        assert!(sess.messages[1].content.contains("你好呀"));
    }

    /// Worker 正常结束：模型最后一条 assistant 回复应广播 MessageEmitted
    /// （此前从未触发），summary 走 SessionFinished。
    #[tokio::test]
    async fn on_task_end_emits_last_assistant_message() {
        use contexa_context::Message;
        use contexa_core::WorkerResult;

        let mut wr = WorkerResult::success("i", "qview", "t", None, Some("已回复用户问候。".to_string()));
        wr.messages = vec![
            Message::system("你是器灵"),
            Message::user("你好"),
            Message::assistant("你好！我是器灵，有什么可以帮你？"),
        ];
        let result = serde_json::to_value(&wr).unwrap();

        let (hook, sink) = hook_with_sink(Arc::default());
        let ctx = contexa_hooks::TaskContext::empty();
        hook.on_task_end(&ctx, &result).await;

        let evs = sink.0.lock();
        let emitted = evs
            .iter()
            .filter(|e| matches!(e, AgentEvent::MessageEmitted { .. }))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(emitted.len(), 1, "应广播 1 条 MessageEmitted");
        match &emitted[0] {
            AgentEvent::MessageEmitted { text, role, .. } => {
                assert_eq!(text, "你好！我是器灵，有什么可以帮你？");
                assert_eq!(*role, Role::Assistant);
            }
            other => panic!("unexpected: {other:?}"),
        }
        let finished = evs
            .iter()
            .filter(|e| matches!(e, AgentEvent::SessionFinished { .. }))
            .count();
        assert_eq!(finished, 1);
    }

    /// 模型把真正答案写在**更早**的 assistant 轮次、末尾只是收尾废话时，
    /// 所有 assistant 正文都应广播（实时窗口不再只看到最后一句"搞定啦主人～✨"）。
    #[tokio::test]
    async fn on_task_end_emits_all_assistant_prose() {
        use contexa_context::Message;
        use contexa_core::WorkerResult;

        let mut wr = WorkerResult::success(
            "i",
            "qview",
            "t",
            None,
            Some("能看到～目录里共 47 个条目。".to_string()),
        );
        wr.messages = vec![
            Message::user("你能看到这个目录有啥文件吗？"),
            Message::assistant("好嘞主人～让我瞅一眼这个目录里都有啥！🔍"),
            Message::tool("{\"count\":47}".to_string(), "call_1".to_string()),
            Message::assistant("搞定啦主人～✨"),
        ];
        let result = serde_json::to_value(&wr).unwrap();

        let (hook, sink) = hook_with_sink(Arc::default());
        let ctx = contexa_hooks::TaskContext::empty();
        hook.on_task_end(&ctx, &result).await;

        let evs = sink.0.lock();
        let texts: Vec<String> = evs
            .iter()
            .filter_map(|e| match e {
                AgentEvent::MessageEmitted { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            vec![
                "好嘞主人～让我瞅一眼这个目录里都有啥！🔍",
                "搞定啦主人～✨"
            ],
            "两条 assistant 正文都应广播（不能只广播最后一条）"
        );
    }

    /// summary 与模型最后一条回复相同时，不重复广播 MessageEmitted（避免 UI 双气泡）。
    #[tokio::test]
    async fn on_task_end_dedupes_summary_equal_to_last_message() {
        use contexa_context::Message;
        use contexa_core::WorkerResult;

        let mut wr = WorkerResult::success(
            "i",
            "qview",
            "t",
            None,
            Some("已回复用户问候，并等待进一步指示。".to_string()),
        );
        wr.messages = vec![
            Message::user("你好"),
            Message::assistant("已回复用户问候，并等待进一步指示。"),
        ];
        let result = serde_json::to_value(&wr).unwrap();

        let (hook, sink) = hook_with_sink(Arc::default());
        let ctx = contexa_hooks::TaskContext::empty();
        hook.on_task_end(&ctx, &result).await;

        let evs = sink.0.lock();
        let emitted = evs
            .iter()
            .filter(|e| matches!(e, AgentEvent::MessageEmitted { .. }))
            .count();
        assert_eq!(emitted, 0, "summary == 回复正文时不应再广播 MessageEmitted");
        let finished = evs
            .iter()
            .filter(|e| matches!(e, AgentEvent::SessionFinished { .. }))
            .count();
        assert_eq!(finished, 1);
    }
}
