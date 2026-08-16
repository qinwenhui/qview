//! Agent 端到端测试（架构文档 §15.5）。
//!
//! 流程：
//! 1. 构造 Application（含 DocumentService / SearchService / ToolRegistry）
//! 2. 构造 ReActWorker（用 DummyLLM 脚本驱动）
//! 3. AgentRuntime 启动 session
//! 4. 订阅事件，断言事件序列与终态

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::json;

use contexa_core::ReActWorker;
use contexa_llm::{DummyLLM, LLMResponse, ToolCall, ToolFunction};

use qview_application::protocol::{DocumentId, PermissionPolicy};
use qview_application::service::{DocumentService, SearchService};
use qview_application::tool::ToolRegistry;
use qview_application::tools::{register_defaults, ALL_TOOL_NAMES};

use qview_agent::audit::InMemoryAuditSink;
use qview_agent::event::{AgentEvent, AgentSink, Phase};
use qview_agent::handle::AgentGoal;

/// 意图分类会消耗第一条 LLM 响应。Mock 脚本第一条必须返回分类 JSON。
/// 返回 Unknown → runtime 走完整 ReAct，用脚本剩余部分。
fn classify_json() -> LLMResponse {
    let args = r#"{"kind":"Unknown","confidence":0.5,"params":{},"reply":"","flow":null}"#;
    LLMResponse {
        content: String::new(),
        tool_calls: vec![contexa_llm::ToolCall::new(
            "route_1",
            "route_intent",
            serde_json::Value::String(args.into()),
        )],
        usage: Default::default(),
        raw: None,
    }
}

fn fixture_log() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("qview-agent-e2e-{}.log", uuid::Uuid::new_v4()));
    let mut body = String::with_capacity(300 * 80);
    for i in 0..300 {
        if i % 5 == 0 {
            body.push_str(&format!("2026-08-06 ERROR 5{:02} req={}\n", (i % 9) + 1, i));
        } else {
            body.push_str(&format!("2026-08-06 INFO req={}\n", i));
        }
    }
    std::fs::write(&p, body).unwrap();
    p
}

fn make_app(path: std::path::PathBuf) -> (
    Arc<DocumentService>,
    Arc<SearchService>,
    Arc<ToolRegistry>,
    DocumentId,
) {
    let docs = Arc::new(DocumentService::default());
    let id = docs.open(path).unwrap();
    let search = Arc::new(SearchService::new(docs.clone()));
    let policy = PermissionPolicy::with_allowlist(
        ALL_TOOL_NAMES.iter().map(|s| s.to_string()).collect(),
    );
    let mut registry = ToolRegistry::new(policy);
    register_defaults(
        &mut registry,
        docs.clone(),
        search.clone(),
        None,
        qview_application::tools::SharedViewport::default(),
        &[],
    )
    .unwrap();
    (docs, search, Arc::new(registry), id)
}

fn make_worker(script: Vec<LLMResponse>, business_code: &str) -> Arc<ReActWorker> {
    let llm = DummyLLM::new(script);
    Arc::new(
        ReActWorker::try_new(
            Arc::new(llm),
            "qview-agent test",
            "qview-agent-test",
            business_code,
        )
        .expect("worker"),
    )
}

/// 包装 AgentRuntime::new：e2e 测试不配委派子 worker。
///
/// `store` 默认 None；需要持久化的测试自己开 store 并改用 make_runtime_with_store。
fn make_runtime(
    worker: Arc<ReActWorker>,
    audit: &Arc<InMemoryAuditSink>,
) -> (
    qview_agent::handle::AgentRuntimeHandle,
    Arc<qview_agent::approval::ApprovalRegistry>,
) {
    make_runtime_with_store(worker, audit, None)
}

/// 支持自定义 store（持久化场景）。
fn make_runtime_with_store(
    worker: Arc<ReActWorker>,
    audit: &Arc<InMemoryAuditSink>,
    store: Option<Arc<dyn qview_store::Storage>>,
) -> (
    qview_agent::handle::AgentRuntimeHandle,
    Arc<qview_agent::approval::ApprovalRegistry>,
) {
    let audit = audit.clone();
    use qview_agent::runtime::AgentRuntime;
    let approvals = Arc::new(qview_agent::approval::ApprovalRegistry::new());
    AgentRuntime::new(
        worker,
        approvals.clone(),
        audit,
        qview_agent::sink_hook::WeakSinks::new(),
        store,
        "mock",
        "dummy",
        None,
        2_000,
        12_000,
    )
}

/// 收集所有事件到 Vec。
#[derive(Default)]
struct CollectingSink {
    events: Mutex<Vec<AgentEvent>>,
}

impl std::fmt::Debug for CollectingSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CollectingSink").finish()
    }
}

impl AgentSink for CollectingSink {
    fn on_event(&self, e: AgentEvent) {
        self.events.lock().push(e);
    }
}

fn await_terminated(events: &[AgentEvent]) -> bool {
    events.iter().any(|e| {
        matches!(
            e,
            AgentEvent::SessionFinished { .. } | AgentEvent::Failed { .. } | AgentEvent::Cancelled { .. }
        )
    })
}

#[tokio::test]
async fn end_to_end_search_finish() {
    let path = fixture_log();
    let (_docs, _search, registry, doc_id) = make_app(path.clone());

    // 脚本：0. 意图分类(Unknown→走ReAct)  1. search_text  2. worker_finish
    let script = vec![
        classify_json(), // 意图分类
        LLMResponse {
            content: "".into(),
            tool_calls: vec![ToolCall::new(
                "c1",
                "search_text",
                json!({"document_id": doc_id.get(), "query": "ERROR", "limit": 5}),
            )],
            usage: Default::default(),
            raw: None,
        },
        LLMResponse {
            content: "".into(),
            tool_calls: vec![ToolCall::new(
                "c2",
                "worker_finish",
                json!({"status": "success", "result": {"matches": 60}, "summary": "完成"}),
            )],
            usage: Default::default(),
            raw: None,
        },
    ];

    let worker = make_worker(script, "qview");
    let audit = InMemoryAuditSink::new();
    let (handle, _approvals) = make_runtime(worker, &audit);

    // 由于 worker.instance_sources 为空，工具调用会失败 → Failed 终态。
    // 这里只验证事件流顺序正确 + 终态翻译正确。

    let sink = Arc::new(CollectingSink::default());
    let _g = handle.subscribe(sink.clone());

    let sid = handle
        .start_session(AgentGoal::new("find 5xx").with_spec("e2e", "找错误", "find 5xx"))
        .await
        .unwrap();
    let _ = doc_id;

    // 等待终态（最多 5s）
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if await_terminated(&sink.events.lock()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let events = sink.events.lock();
    assert!(!events.is_empty(), "should receive events");
    assert!(
        matches!(events.first(), Some(AgentEvent::SessionStarted { .. })),
        "first event should be SessionStarted"
    );
    // 终态：Failed（因为 tool source 为空）
    let last = events.last().unwrap();
    assert!(
        matches!(last, AgentEvent::Failed { .. } | AgentEvent::SessionFinished { .. }),
        "last event should be terminal, got {last:?}"
    );

    // session_id 与 start_session 返回值一致
    if let AgentEvent::SessionStarted { session_id, .. } = &events[0] {
        assert_eq!(session_id, &sid);
    }

    // 抑制 unused 警告
    let _ = registry;
    let _ = std::fs::remove_file(&path);
}

/// 每轮 chat 都睡 150ms 的慢 LLM（让取消测试有时间介入，避免快速跑完）。
struct SlowLLM {
    inner: DummyLLM,
}
#[async_trait::async_trait]
impl contexa_llm::LLMClient for SlowLLM {
    async fn chat(&self, req: contexa_llm::ChatRequest<'_>) -> Result<LLMResponse, contexa_core::ContexaError> {
        tokio::time::sleep(Duration::from_millis(150)).await;
        self.inner.chat(req).await
    }
}

/// contexa-rs 取消支持：预取消的令牌应让 executor 立即终止（不进入任何轮）。
#[tokio::test]
async fn run_with_cancel_precancelled_stops_immediately() {
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();

    // 脚本无论为何都不该被执行（token 已取消）
    let worker = make_worker(vec![], "cancel-test");
    let spec = contexa_core::TaskSpec::simple("cancel", "should not run");
    let task = contexa_core::Task::new("cancel-task", spec, "should not run");
    let result = worker.run_with_cancel(task, &cancel).await;

    assert_eq!(result.status, contexa_core::WorkerStatus::Timeout);
    assert!(
        result.note.as_deref().unwrap_or("").contains("cancel"),
        "note should mention cancel, got {:?}",
        result.note
    );
}

/// 运行时取消：`AgentRuntimeHandle::cancel` 应让活跃 session 收到 `Cancelled` 终态。
#[tokio::test]
async fn runtime_cancel_emits_cancelled_event() {
    let path = fixture_log();
    let (_docs, _search, _registry, doc_id) = make_app(path.clone());

    // 脚本：0.意图分类  1.. search_text（多轮）；慢 LLM 让取消有时间介入
    let mut script = vec![classify_json()];
    for i in 0..10 {
        script.push(LLMResponse {
            content: "".into(),
            tool_calls: vec![ToolCall::new(
                &format!("c{i}"),
                "search_text",
                json!({"document_id": doc_id.get(), "query": "ERROR", "limit": 5}),
            )],
            usage: Default::default(),
            raw: None,
        });
    }
    let slow = Arc::new(SlowLLM { inner: DummyLLM::new(script) });
    let worker = Arc::new(
        contexa_core::ReActWorker::try_new(slow, "qview-agent test", "qview-agent-test", "qview")
            .expect("worker"),
    );
    let audit = InMemoryAuditSink::new();
    let (handle, _approvals) = make_runtime(worker, &audit);

    let sink = Arc::new(CollectingSink::default());
    let _g = handle.subscribe(sink.clone());

    let sid = handle
        .start_session(AgentGoal::new("long task").with_spec("e2e", "长任务", "long task"))
        .await
        .unwrap();

    // 等第一轮 LLM 在途时取消（慢 LLM 保证 session 仍在跑）
    tokio::time::sleep(Duration::from_millis(30)).await;
    handle.cancel(sid.clone()).await;

    // 等待 Cancelled 终态（最多 5s）
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if sink.events.lock().iter().any(|e| matches!(e, AgentEvent::Cancelled { .. })) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        sink.events.lock().iter().any(|e| matches!(e, AgentEvent::Cancelled { .. })),
        "expected a Cancelled event, got: {:?}",
        *sink.events.lock()
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn sink_receives_phase_changes() {
    let path = fixture_log();
    let (_docs, _search, _registry, doc_id) = make_app(path.clone());

    let script = vec![
        classify_json(),
        LLMResponse {
        content: "".into(),
        tool_calls: vec![ToolCall::new(
            "c1",
            "search_text",
            json!({"document_id": doc_id.get(), "query": "ERROR"}),
        )],
        usage: Default::default(),
        raw: None,
    }];
    let worker = make_worker(script, "qview");
    let audit = InMemoryAuditSink::new();
    let (handle, _approvals) = make_runtime(worker, &audit);

    let sink = Arc::new(CollectingSink::default());
    let _g = handle.subscribe(sink.clone());
    let _ = handle
        .start_session(AgentGoal::new("find errors"))
        .await
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if await_terminated(&sink.events.lock()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 至少出现一次 PhaseChanged(Thinking)
    let events = sink.events.lock();
    let saw_thinking = events
        .iter()
        .any(|e| matches!(e, AgentEvent::PhaseChanged { phase: Phase::Thinking, .. }));
    assert!(saw_thinking, "should see at least one Thinking phase");

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn audit_hook_records_task_end() {
    let path = fixture_log();
    let (_docs, _search, _registry, _doc_id) = make_app(path.clone());

    // 第一个响应调 search_text（会失败，因为没 source）→ WorkerResult::Failed
    let script = vec![
        classify_json(),
        LLMResponse {
        content: "".into(),
        tool_calls: vec![ToolCall::new(
            "c1",
            "search_text",
            json!({"document_id": 1, "query": "x"}),
        )],
        usage: Default::default(),
        raw: None,
    }];
    let worker = make_worker(script, "qview");
    let audit = InMemoryAuditSink::new();
    let (handle, _) = make_runtime(worker, &audit);

    let sink = Arc::new(CollectingSink::default());
    let _g = handle.subscribe(sink.clone());
    let _ = handle.start_session(AgentGoal::new("x")).await.unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if !audit.snapshot().is_empty() {
            break;
        }
    }

    // 至少有一条 task_end 审计
    let recs = audit.snapshot();
    assert!(!recs.is_empty(), "audit should record task_end");
    let has_task_end = recs.iter().any(|r| r.tool == "<task_end>");
    assert!(has_task_end);

    let _ = std::fs::remove_file(&path);
}

/// 会话落盘集成测试：会话终态后，store 应能查到该会话（meta + 消息）。
#[tokio::test]
async fn session_is_persisted_to_store_on_finish() {
    let path = fixture_log();
    let (_docs, _search, _registry, _doc_id) = make_app(path.clone());

    // 脚本：直接 worker_finish（success）→ 终态 Success，无工具依赖
    let script = vec![
        classify_json(),
        LLMResponse {
        content: "".into(),
        tool_calls: vec![ToolCall::new(
            "c1",
            "worker_finish",
            json!({"status": "success", "result": null, "summary": "完成"}),
        )],
        usage: Default::default(),
        raw: None,
    }];
    let worker = make_worker(script, "qview");

    // 真实 redb store（临时文件）
    let store_path = std::env::temp_dir()
        .join(format!("qview-agent-e2e-store-{}.db", uuid::Uuid::new_v4()));
    let store = qview_store::open_store(&store_path).unwrap();

    let audit = InMemoryAuditSink::new();
    let (handle, _approvals) = make_runtime_with_store(worker, &audit, Some(store.clone()));

    let sink = Arc::new(CollectingSink::default());
    let _g = handle.subscribe(sink.clone());

    let sid = handle
        .start_session(
            AgentGoal::new("find 5xx errors")
                .with_spec("e2e", "找错误", "find 5xx errors")
                .with_document_path(path.display().to_string()),
        )
        .await
        .unwrap();

    // 等待终态（最多 5s）
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if await_terminated(&sink.events.lock()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        await_terminated(&sink.events.lock()),
        "session should reach a terminal event"
    );

    // store 里有该会话
    let recent = store.recent_sessions(10).unwrap();
    assert_eq!(recent.len(), 1, "应恰好落盘一条会话");
    assert_eq!(recent[0].id, sid);
    assert_eq!(recent[0].file_id.as_deref(), Some(path.display().to_string().as_str()));
    assert_eq!(recent[0].provider, "mock");

    // 消息完整：含用户 query 消息
    let loaded = store.load_session(&sid).unwrap().expect("会话可加载");
    assert!(!loaded.messages.is_empty(), "messages 不应为空");
    assert!(
        loaded
            .messages
            .iter()
            .any(|m| m.role == qview_store::StoreRole::User && m.content.contains("find 5xx errors")),
        "应包含用户消息，got: {:?}",
        loaded.messages
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&store_path);
}

// 抑制未使用导入
#[allow(dead_code)]
fn _force_link(_t: &ToolFunction) {}
