//! P4 e2e：完整的 GuardedTool → ApprovalRequired → proposal_decision(Approve) → 落盘 流程。

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::json;

use contexa_core::ReActWorker;
use contexa_llm::{DummyLLM, LLMResponse, ToolCall};
use contexa_tools::ToolSource;

use qview_application::protocol::{PermissionPolicy, ProposalId};
use qview_application::service::annotation::AnnotationService;
use qview_application::service::{DocumentService, SearchService};
use qview_application::tool::ToolRegistry;
use qview_application::tools::{register_defaults, ALL_TOOL_NAMES_WITH_WRITES};

use qview_agent::approval::ApprovalRegistry;
use qview_agent::audit::InMemoryAuditSink;
use qview_agent::event::{AgentEvent, AgentSink, Phase};
use qview_agent::guarded_tool::{GuardedTool, GuardedToolMeta, InnerInvokeFn};
use qview_agent::handle::{AgentGoal, ProposalDecision};
use qview_agent::runtime::AgentRuntime;

fn fixture_log() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("qview-p4-{}.log", uuid::Uuid::new_v4()));
    std::fs::write(&p, "line1\nline2\nline3\n").unwrap();
    p
}

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

fn fixture_annotation_path() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("qview-p4-ann-{}.json", uuid::Uuid::new_v4()));
    p
}

#[derive(Default)]
struct CollectingSink {
    events: Mutex<Vec<AgentEvent>>,
}

/// 包装 AgentRuntime::new：e2e_p4 测试不配委派子 worker。
///
/// **必须传入共享的 approvals**：P4 测试里 GuardedTool 已持有 `approvals.clone()`，
/// runtime 必须用同一个 registry 才能让 `cancel_all()` / `complete()` 影响 GuardedTool。
fn make_runtime(
    worker: Arc<ReActWorker>,
    approvals: Arc<ApprovalRegistry>,
    audit: &Arc<InMemoryAuditSink>,
) -> (qview_agent::handle::AgentRuntimeHandle, Arc<ApprovalRegistry>) {
    let audit = audit.clone();
    AgentRuntime::new(
        worker,
        approvals.clone(),
        audit,
        qview_agent::sink_hook::WeakSinks::new(),
        None,
        "mock",
        "dummy",
        None,
        2_000,
        12_000,
    )
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

/// 构造 annotate_create GuardedTool 的 helper（接受外部 approvals 与 sink）。
fn make_annotate_guarded(
    ann: Arc<AnnotationService>,
    approvals: Arc<ApprovalRegistry>,
    sink: Arc<CollectingSink>,
) -> GuardedTool {
    let raw = qview_application::tools::annotate_tool(ann.clone()).unwrap();
    let name = raw.name().to_string();
    let spec = raw.spec().clone();
    let raw_arc = Arc::new(raw);
    let source: Arc<dyn ToolSource> = raw_arc.clone();
    let inner: InnerInvokeFn = Arc::new(move |args| {
        let s = source.clone();
        let n = name.clone();
        Box::pin(async move { s.call_tool(&n, args).await })
    });
    let g = GuardedTool::new(
        GuardedToolMeta {
            name: "annotate_create".into(),
            spec,
            side_effect: qview_application::protocol::SideEffect::Reversible,
            reason: "test annotation".into(),
        },
        approvals,
        inner,
    );
    g.add_sink(sink as Arc<dyn AgentSink>);
    g
}

#[tokio::test]
async fn guarded_tool_full_approval_flow() {
    let log_path = fixture_log();
    let ann_path = fixture_annotation_path();
    let docs = Arc::new(DocumentService::default());
    let id = docs.open(log_path.clone()).unwrap();
    let search = Arc::new(SearchService::new(docs.clone()));
    let ann = Arc::new(AnnotationService::with_path(docs.clone(), ann_path.clone()));

    // 注册只读工具（跳过 annotate_create / export_report）
    let mut registry = ToolRegistry::new(PermissionPolicy::with_allowlist(
        ALL_TOOL_NAMES_WITH_WRITES.iter().map(|s| s.to_string()).collect(),
    ));
    register_defaults(&mut registry, docs.clone(), search.clone(), Some(ann.clone()), qview_application::tools::SharedViewport::default(), &["annotate_create", "export_report"]).unwrap();
    let registry_arc = Arc::new(registry);

    // DummyLLM 脚本（第一条是意图分类 JSON → Unknown → 走 ReAct）
    let doc_id_val = id.get();
    let script = vec![
        classify_json(),
        LLMResponse {
            content: "".into(),
            tool_calls: vec![ToolCall::new(
                "c1",
                "annotate_create",
                json!({
                    "document_id": doc_id_val,
                    "start_byte": 0, "end_byte": 6,
                    "start_line": 0, "end_line": 1,
                    "start_col": 0, "end_col": 5,
                    "selected_text": "line1",
                    "text": "test note",
                    "_session_id": "s1",
                }),
            )],
            usage: Default::default(),
            raw: None,
        },
        LLMResponse {
            content: "".into(),
            tool_calls: vec![ToolCall::new(
                "c2",
                "worker_finish",
                json!({"status": "success", "result": {"ok": true}, "summary": "done"}),
            )],
            usage: Default::default(),
            raw: None,
        },
    ];
    let llm = Arc::new(DummyLLM::new(script));

    // 1) 先构造 ApprovalRegistry（让 GuardedTool 知道）
    let approvals = Arc::new(ApprovalRegistry::new());

    // 2) 构造 GuardedTool
    let pre_sink = Arc::new(CollectingSink::default());
    let annotate_guarded = make_annotate_guarded(ann.clone(), approvals.clone(), pre_sink.clone());

    // 3) Worker + instance_sources
    let mut worker = ReActWorker::try_new(llm, "sys", "i", "qview").unwrap();
    worker.instance_sources = vec![
        registry_arc as Arc<dyn ToolSource>,
        Arc::new(annotate_guarded) as Arc<dyn ToolSource>,
    ];
    worker.validate().unwrap();
    let worker = Arc::new(worker);

    // 4) Audit + Runtime
    let audit = InMemoryAuditSink::new();
    // AgentRuntime::new 会构造自己的 approvals；为保证 GuardedTool 写进同一个 registry，
    // 我们让 Runtime 使用我们的 approvals。P4 简化：用 builder。
    // 这里改用 builder 模式（如未来加）；当前直接接受两个 registry 不同的事实，
    // 然后用我们的 approvals 来 complete。
    let (handle, _approvals_from_rt) = make_runtime(worker, approvals.clone(), &audit);

    let sink = Arc::new(CollectingSink::default());
    let _g = handle.subscribe(sink.clone());

    // 5) 启动 session
    let goal = AgentGoal::new("test").with_spec("t", "test", "test");
    let sid = handle.start_session(goal).await.unwrap();

    // 等 ApprovalRequired（来自 GuardedTool 自己 broadcast，落到 pre_sink）
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut proposal_id = None;
    while std::time::Instant::now() < deadline {
        let events = pre_sink.events.lock();
        for e in events.iter() {
            if let AgentEvent::ApprovalRequired { proposal_id: pid, .. } = e {
                proposal_id = Some(*pid);
                break;
            }
        }
        drop(events);
        if proposal_id.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let pid = proposal_id.expect("ApprovalRequired 事件应在 5s 内出现");

    // 6) 决策：直接用我们的 approvals（不是 runtime 的）
    approvals.complete(pid, ProposalDecision::Approve).expect("complete");

    // 7) 等终态（5s 内）—— sink 应有 SessionFinished
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let events = sink.events.lock();
        let done = events.iter().any(|e| {
            matches!(
                e,
                AgentEvent::SessionFinished { .. } | AgentEvent::Failed { .. } | AgentEvent::Cancelled { .. }
            )
        });
        drop(events);
        if done {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 8) 校验：AnnotationService 应有 1 条批注
    let list = ann.list(id).await;
    assert_eq!(list.len(), 1, "批注应已写入：pre_sink={:?}", pre_sink.events.lock());
    assert_eq!(list[0].text, "test note");

    // 9) 校验事件流
    let pre_events = pre_sink.events.lock();
    let events = sink.events.lock();
    let saw_approval = pre_events
        .iter()
        .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. }));
    let saw_finish = events.iter().any(|e| matches!(e, AgentEvent::SessionFinished { .. }));
    assert!(saw_approval);
    assert!(saw_finish, "应当 SessionFinished");

    // 抑制 unused
    let _ = (sid, ProposalId::new());

    let _ = std::fs::remove_file(&log_path);
    let _ = std::fs::remove_file(&ann_path);
}

#[tokio::test]
async fn guarded_tool_reject_blocks_execution() {
    let log_path = fixture_log();
    let ann_path = fixture_annotation_path();
    let docs = Arc::new(DocumentService::default());
    let id = docs.open(log_path.clone()).unwrap();
    let ann = Arc::new(AnnotationService::with_path(docs.clone(), ann_path.clone()));
    let search = Arc::new(SearchService::new(docs.clone()));
    let mut registry = ToolRegistry::new(PermissionPolicy::with_allowlist(
        ALL_TOOL_NAMES_WITH_WRITES.iter().map(|s| s.to_string()).collect(),
    ));
    register_defaults(&mut registry, docs.clone(), search.clone(), Some(ann.clone()), qview_application::tools::SharedViewport::default(), &["annotate_create", "export_report"]).unwrap();
    let _registry_arc = Arc::new(registry);

    let approvals = Arc::new(ApprovalRegistry::new());
    let pre_sink = Arc::new(CollectingSink::default());
    let guarded = make_annotate_guarded(ann.clone(), approvals.clone(), pre_sink);

    let guarded_arc = Arc::new(guarded) as Arc<dyn ToolSource>;
    let h = tokio::spawn(async move {
        guarded_arc
            .call_tool(
                "annotate_create",
                json!({"document_id": id.get(), "start_byte":0,"end_byte":6,"start_line":0,"end_line":1,"start_col":0,"end_col":5,"selected_text":"line1","text":"x"}),
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    approvals.cancel_all();
    let r = h.await.unwrap().unwrap();
    assert!(r.is_error);
    assert_eq!(r.content["error"], "rejected_by_user");

    let list = ann.list(id).await;
    assert_eq!(list.len(), 0, "reject 后不应有批注");

    let _ = std::fs::remove_file(&log_path);
    let _ = std::fs::remove_file(&ann_path);
}

#[tokio::test]
async fn worker_result_phase_sequence_is_correct() {
    let log_path = fixture_log();
    let docs = Arc::new(DocumentService::default());
    docs.open(log_path.clone()).unwrap();
    let search = Arc::new(SearchService::new(docs.clone()));
    let ann = Arc::new(AnnotationService::with_path(docs.clone(), std::env::temp_dir().join(format!("{}.json", uuid::Uuid::new_v4()))));
    let mut registry = ToolRegistry::new(PermissionPolicy::with_allowlist(vec!["search_text".into()]));
    register_defaults(&mut registry, docs.clone(), search.clone(), Some(ann.clone()), qview_application::tools::SharedViewport::default(), &["annotate_create", "export_report"]).unwrap();
    let registry_arc = Arc::new(registry);

    let script = vec![
        classify_json(),
        LLMResponse {
            content: "".into(),
            tool_calls: vec![ToolCall::new(
                "c1",
                "search_text",
                json!({"document_id": 1, "query": "x", "limit": 1}),
            )],
            usage: Default::default(),
            raw: None,
        },
        LLMResponse {
            content: "".into(),
            tool_calls: vec![ToolCall::new(
                "c2",
                "worker_finish",
                json!({"status": "success", "result": null, "summary": "ok"}),
            )],
            usage: Default::default(),
            raw: None,
        },
    ];
    let llm = Arc::new(DummyLLM::new(script));
    let mut worker = ReActWorker::try_new(llm, "sys", "i", "qview").unwrap();
    worker.instance_sources = vec![registry_arc as Arc<dyn ToolSource>];
    worker.validate().unwrap();
    let worker = Arc::new(worker);

    let audit = InMemoryAuditSink::new();
    let (handle, _) = make_runtime(worker, Arc::new(ApprovalRegistry::new()), &audit);
    let sink = Arc::new(CollectingSink::default());
    let _g = handle.subscribe(sink.clone());
    let _ = handle.start_session(AgentGoal::new("x")).await.unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let events = sink.events.lock();
        let done = events
            .iter()
            .any(|e| matches!(e, AgentEvent::SessionFinished { .. }));
        drop(events);
        if done {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let events = sink.events.lock();
    let saw_thinking = events
        .iter()
        .any(|e| matches!(e, AgentEvent::PhaseChanged { phase: Phase::Thinking, .. }));
    let saw_done = events
        .iter()
        .any(|e| matches!(e, AgentEvent::PhaseChanged { phase: Phase::Done, .. }));
    let saw_session_finished = events
        .iter()
        .any(|e| matches!(e, AgentEvent::SessionFinished { .. }));
    assert!(saw_thinking);
    assert!(saw_done);
    assert!(saw_session_finished);

    let _ = std::fs::remove_file(&log_path);
}
