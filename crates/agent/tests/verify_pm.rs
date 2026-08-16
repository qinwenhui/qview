//! 项目经理（PM）架构验证（重构后：去 Flow，意图层升级为项目经理）。
//!
//! 覆盖三条关键链路：
//! 1. **计划注入**：classify 产出 `plan` → 注入 ReAct 上下文（CaptureHook 断言）
//! 2. **进度汇报**：ReAct 调 `report_progress` → QviewSinkHook 广播 `ProgressNote`（config.build 全链路）
//! 3. **记忆跨会话回忆**：会话 1 worker_finish 落记忆 → 会话 2 ReAct 上下文出现 `## Memory recall`

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::json;

use contexa_core::{ReActWorker, WorkerConfig};
use contexa_hooks::{Hook, TaskContext};
use contexa_llm::{DummyLLM, LLMResponse, ToolCall};

use qview_application::protocol::PermissionPolicy;
use qview_application::service::annotation::AnnotationService;
use qview_application::service::{DocumentService, SearchService};
use qview_application::tool::ToolRegistry;
use qview_application::tools::{register_defaults, SharedViewport, ALL_TOOL_NAMES};

use qview_agent::config::{AgentConfig, AgentDeps};
use qview_agent::event::{AgentEvent, AgentSink};
use qview_agent::handle::AgentGoal;

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

/// 意图分类会消耗第一条 LLM 响应（route_intent 工具调用）。
/// `kind`：意图分类（Unknown / SearchLog 等）；`plan`：项目经理的分步执行计划（非任务类可传 None）。
fn classify_json(kind: &str, plan: Option<&str>) -> LLMResponse {
    let args = match plan {
        Some(p) => format!(
            r#"{{"kind":"{kind}","confidence":0.6,"params":{{}},"reply":"","plan":{}}}"#,
            serde_json::to_string(p).unwrap()
        ),
        None => format!(r#"{{"kind":"{kind}","confidence":0.6,"params":{{}},"reply":"","plan":null}}"#),
    };
    LLMResponse {
        content: String::new(),
        tool_calls: vec![contexa_llm::ToolCall::new(
            "route_1",
            "route_intent",
            serde_json::Value::String(args),
        )],
        usage: Default::default(),
        raw: None,
    }
}

fn fixture_log() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("qview-pm-{}.log", uuid::Uuid::new_v4()));
    let mut body = String::with_capacity(300 * 40);
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

fn make_app(path: std::path::PathBuf) -> (Arc<ToolRegistry>, qview_application::protocol::DocumentId) {
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
        SharedViewport::default(),
        &[],
    )
    .unwrap();
    (Arc::new(registry), id)
}

/// 构造 worker：注入 registry 工具源 + 可选的 capturing hook / memory store。
fn make_worker(
    script: Vec<LLMResponse>,
    registry: Arc<ToolRegistry>,
    business_code: &str,
) -> ReActWorker {
    let llm = DummyLLM::new(script);
    let mut w = ReActWorker::builder()
        .llm(Arc::new(llm))
        .system_prompt("你是 qview 的项目经理小Q。")
        .instance_id("pm-test")
        .business_code(business_code)
        .config(WorkerConfig::default())
        .build();
    w.instance_sources = vec![registry.as_arc_source()];
    w
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

/// 等待累计出现 `n` 个终态事件（SessionFinished / Failed / Cancelled）。
/// 多轮会话场景必须按**新增**终态数等待，否则会被上一轮的终态误判为"已完成"。
async fn wait_terminal_n(sink: &Arc<CollectingSink>, n: usize) {
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while std::time::Instant::now() < deadline {
        let count = sink
            .events
            .lock()
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    AgentEvent::SessionFinished { .. }
                        | AgentEvent::Failed { .. }
                        | AgentEvent::Cancelled { .. }
                )
            })
            .count();
        if count >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("session 未在时限内到达 {n} 个终态事件");
}

// ---------------------------------------------------------------------------
// CaptureHook：在 post_llm_call 记录「发给 LLM 的上下文」（此时记忆 recall 已注入）
// ---------------------------------------------------------------------------

#[derive(Default)]
struct CaptureHook {
    /// 每次 LLM 调用的完整消息文本（post_llm_call 时机）。
    contexts: Mutex<Vec<String>>,
}
impl std::fmt::Debug for CaptureHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureHook")
            .field("calls", &self.contexts.lock().len())
            .finish()
    }
}
#[async_trait]
impl Hook for CaptureHook {
    fn name(&self) -> &str {
        "capture"
    }

    async fn post_llm_call(&self, ctx: &TaskContext<'_>) {
        let mut s = String::new();
        for m in ctx.messages {
            if !m.content.trim().is_empty() {
                s.push_str(&m.content);
                s.push('\n');
            }
        }
        self.contexts.lock().push(s);
    }
}

// ---------------------------------------------------------------------------
// 测试 1：plan 注入 ReAct 上下文（CaptureHook 断言）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pm_plan_is_injected_into_react_context() {
    let path = fixture_log();
    let (registry, _doc_id) = make_app(path.clone());

    let plan = "1 打开文件\n2 搜 ERROR\n3 看上下文\n4 汇总结论";
    let script = vec![
        classify_json("SearchLog", Some(plan)),
        LLMResponse {
            content: "".into(),
            tool_calls: vec![ToolCall::new(
                "c1",
                "worker_finish",
                json!({"status": "success", "result": null, "summary": "ok"}),
            )],
            usage: Default::default(),
            raw: None,
        },
    ];

    let capture = Arc::new(CaptureHook::default());
    let mut worker = make_worker(script, registry, "qview");
    worker.hooks.push(capture.clone());
    let worker = Arc::new(worker);

    let audit = qview_agent::audit::InMemoryAuditSink::new();
    let approvals = Arc::new(qview_agent::approval::ApprovalRegistry::new());
    let (handle, _) = qview_agent::runtime::AgentRuntime::new(
        worker,
        approvals,
        audit,
        qview_agent::sink_hook::WeakSinks::new(),
        None,
        "mock",
        "dummy",
        None,
        2_000,
        12_000,
    );

    let sink = Arc::new(CollectingSink::default());
    let _g = handle.subscribe(sink.clone());
    let _ = handle.start_session(AgentGoal::new("分析这段日志")).await.unwrap();
    wait_terminal_n(&sink, 1).await;

    // 断言：至少一次 ReAct 上下文含「意图分类」标题 + 项目经理的 plan
    let ctxs = capture.contexts.lock();
    let ctx = ctxs
        .iter()
        .find(|c| c.contains("## 意图分类"))
        .unwrap_or_else(|| panic!("应有一次上下文含意图分类，calls={}", ctxs.len()));
    assert!(
        ctx.contains("plan: 1 打开文件"),
        "ReAct 上下文应注入 plan，实际: {ctx}"
    );
    assert!(ctx.contains("- kind: SearchLog"), "应含意图 kind");

    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// 测试 2：report_progress → ProgressNote 广播（config.build 全链路）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pm_report_progress_broadcasts_progress_note() {
    let path = fixture_log();
    let (deps, id) = {
        let docs = Arc::new(DocumentService::default());
        let id = docs.open(path.clone()).unwrap();
        let search = Arc::new(SearchService::new(docs.clone()));
        let ann = Arc::new(AnnotationService::with_path(
            docs.clone(),
            std::env::temp_dir().join(format!("qview-pm-ann2-{}.json", uuid::Uuid::new_v4())),
        ));
        (
            AgentDeps {
                docs,
                search,
                annotations: ann,
                viewport: SharedViewport::default(),
                store: None,
            },
            id,
        )
    };

    let mut config = AgentConfig::mock("(mock)");
    config.instance_id = "pm-e2e".into();
    config.allow_tools = qview_application::tools::ALL_TOOL_NAMES_WITH_WRITES
        .iter()
        .map(|s| s.to_string())
        .collect();

    let script = vec![
        classify_json("SearchLog", Some("1 抽样 2 搜 ERROR 3 看上下文")),
        LLMResponse {
            content: "".into(),
            tool_calls: vec![ToolCall::new(
                "c1",
                "report_progress",
                json!({"message": "正在扫描日志…"}),
            )],
            usage: Default::default(),
            raw: None,
        },
        LLMResponse {
            content: "".into(),
            tool_calls: vec![ToolCall::new(
                "c2",
                "read_context",
                json!({"document_id": id.get(), "line": 0, "before": 2, "after": 2}),
            )],
            usage: Default::default(),
            raw: None,
        },
        LLMResponse {
            content: "".into(),
            tool_calls: vec![ToolCall::new(
                "c3",
                "worker_finish",
                json!({"status": "success", "result": null, "summary": "分析完成"}),
            )],
            usage: Default::default(),
            raw: None,
        },
    ];
    let script_path =
        std::env::temp_dir().join(format!("qview-pm-script-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(&script_path, serde_json::to_string(&script).unwrap()).unwrap();
    config.provider.mock_script_path = Some(script_path.clone());
    config.provider.provider = qview_agent::config::LlmProvider::Mock;

    let handle = config.build(deps.clone()).expect("build");
    let sink = Arc::new(CollectingSink::default());
    let _g = handle.subscribe(sink.clone());
    let _ = handle.start_session(AgentGoal::new("分析这段日志")).await.unwrap();
    wait_terminal_n(&sink, 1).await;

    let events = sink.events.lock();
    // 1) ProgressNote：report_progress 被 hook 拦截并广播
    let notes: Vec<&AgentEvent> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ProgressNote { .. }))
        .collect();
    assert!(!notes.is_empty(), "应广播 ProgressNote，events: {events:?}");
    match notes[0] {
        AgentEvent::ProgressNote { text, .. } => {
            assert_eq!(text, "正在扫描日志…");
        }
        other => panic!("unexpected: {other:?}"),
    }
    // 2) read_context 真实执行成功（字节偏移工具链路）
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCallFinished { tool, is_error: false, .. } if tool == "read_context"
        )),
        "read_context 应执行成功，events: {events:?}"
    );
    // 3) 终态 SessionFinished
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::SessionFinished { summary, .. } if summary == "分析完成"
        )),
        "应以 SessionFinished(success, 分析完成) 结束"
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&script_path);
    let _ = std::fs::remove_file(deps.annotations.path());
}

// ---------------------------------------------------------------------------
// 测试 3：记忆跨会话回忆（会话 1 落记忆 → 会话 2 上下文出现 ## Memory recall）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pm_memory_recall_across_sessions() {
    let path = fixture_log();
    let (registry, _doc_id) = make_app(path.clone());

    // 脚本同时覆盖两个会话（worker 共享同一 DummyLLM，顺序消费）：
    //   会话 1：classify → worker_finish(summary 含"服务器内存 128GB")
    //   会话 2：classify → worker_finish
    let script = vec![
        classify_json("Unknown", None),
        LLMResponse {
            content: "".into(),
            tool_calls: vec![ToolCall::new(
                "s1",
                "worker_finish",
                json!({"status": "success", "result": null, "summary": "服务器内存 128GB，磁盘 2TB"}),
            )],
            usage: Default::default(),
            raw: None,
        },
        classify_json("Unknown", None),
        LLMResponse {
            content: "".into(),
            tool_calls: vec![ToolCall::new(
                "s2",
                "worker_finish",
                json!({"status": "success", "result": null, "summary": "收到"}),
            )],
            usage: Default::default(),
            raw: None,
        },
    ];

    let im_store = Arc::new(contexa_memory::InMemoryStore::new());
    let store: Arc<dyn contexa_memory::MemoryStore> = im_store.clone();

    let capture = Arc::new(CaptureHook::default());
    let mut worker = make_worker(script, registry, "qview");
    worker.hooks.push(capture.clone());
    worker.memory_store = Some(store.clone());
    let worker = Arc::new(worker);

    let audit = qview_agent::audit::InMemoryAuditSink::new();
    let approvals = Arc::new(qview_agent::approval::ApprovalRegistry::new());
    let (handle, _) = qview_agent::runtime::AgentRuntime::new(
        worker,
        approvals,
        audit,
        qview_agent::sink_hook::WeakSinks::new(),
        None,
        "mock",
        "dummy",
        None,
        2_000,
        12_000,
    );

    let sink = Arc::new(CollectingSink::default());
    let _g = handle.subscribe(sink.clone());

    // 会话 1
    let _ = handle
        .start_session(AgentGoal::new("服务器内存多大"))
        .await
        .unwrap();
    wait_terminal_n(&sink, 1).await;

    // consolidate 在 on_task_end 之后异步执行 → 轮询 InMemoryStore 直到落一条记忆
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        if im_store.len() >= 1 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "记忆应在会话 1 结束后落库");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // 会话 2（同一 handle → 共享 worker → 共享 memory_store）
    let _ = handle
        .start_session(AgentGoal::new("服务器内存多大"))
        .await
        .unwrap();
    wait_terminal_n(&sink, 2).await;

    // 断言：某个 ReAct 上下文出现了 memory recall，且含会话 1 的摘要
    let ctxs = capture.contexts.lock();
    let recalled = ctxs
        .iter()
        .find(|c| c.contains("## Memory recall"))
        .unwrap_or_else(|| panic!("应有上下文含 Memory recall，calls={}", ctxs.len()));
    assert!(
        recalled.contains("服务器内存 128GB"),
        "recall 应含会话 1 摘要，实际: {recalled}"
    );

    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// 测试 4：委派（delegate_analysis）——项目经理把独立子任务派发给子 worker 员工
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pm_delegate_analysis_runs_child_worker() {
    let path = fixture_log();
    let docs = Arc::new(DocumentService::default());
    let _id = docs.open(path.clone()).unwrap();
    let search = Arc::new(SearchService::new(docs.clone()));
    let ann = Arc::new(AnnotationService::with_path(
        docs.clone(),
        std::env::temp_dir().join(format!("qview-pm-ann3-{}.json", uuid::Uuid::new_v4())),
    ));
    let deps = AgentDeps {
        docs,
        search,
        annotations: ann,
        viewport: SharedViewport::default(),
        store: None,
    };

    let mut config = AgentConfig::mock("(mock)");
    config.instance_id = "pm-delegate".into();
    config.allow_all_tools();

    // 脚本顺序（父 / 子共享同一 DummyLLM，顺序消费）：
    //   0. 父 classify（Unknown → 走 ReAct，全工具可用）
    //   1. 父 ReAct 调 delegate_analysis（触发子 worker run）
    //   2. 子 worker ReAct round 1 → worker_finish（子立即完成）
    //   3. 父 ReAct round 2 → worker_finish
    let script = vec![
        classify_json("Unknown", None),
        LLMResponse {
            content: "".into(),
            tool_calls: vec![ToolCall::new(
                "p1",
                "delegate_analysis",
                json!({"query": "统计一下这份日志里 ERROR 有几条"}),
            )],
            usage: Default::default(),
            raw: None,
        },
        LLMResponse {
            content: "".into(),
            tool_calls: vec![ToolCall::new(
                "c1",
                "worker_finish",
                json!({"status": "success", "result": {"errors": 60}, "summary": "共 60 条 ERROR"}),
            )],
            usage: Default::default(),
            raw: None,
        },
        LLMResponse {
            content: "".into(),
            tool_calls: vec![ToolCall::new(
                "p2",
                "worker_finish",
                json!({"status": "success", "result": null, "summary": "已派发子任务并汇总"}),
            )],
            usage: Default::default(),
            raw: None,
        },
    ];
    let script_path =
        std::env::temp_dir().join(format!("qview-pm-delegate-script-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(&script_path, serde_json::to_string(&script).unwrap()).unwrap();
    config.provider.mock_script_path = Some(script_path.clone());
    config.provider.provider = qview_agent::config::LlmProvider::Mock;

    let handle = config.build(deps.clone()).expect("build");
    let sink = Arc::new(CollectingSink::default());
    let _g = handle.subscribe(sink.clone());
    let _ = handle.start_session(AgentGoal::new("分析这段日志")).await.unwrap();
    wait_terminal_n(&sink, 1).await;

    let events = sink.events.lock();
    // 1) 父调用了 delegate_analysis（项目经理把子任务派发给员工）
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCallStarted { tool, .. } if tool == "delegate_analysis"
        )),
        "应调用 delegate_analysis，events: {events:?}"
    );
    // 2) 子 worker 的结果折叠回父上下文（delegate_analysis 非错误，output 含子任务返回）
    let dlg_finish = events
        .iter()
        .find(|e| matches!(
            e,
            AgentEvent::ToolCallFinished { tool, .. } if tool == "delegate_analysis"
        ))
        .unwrap_or_else(|| panic!("delegate_analysis 应有完成事件，events: {events:?}"));
    match dlg_finish {
        AgentEvent::ToolCallFinished {
            is_error: false,
            output_summary,
            ..
        } => {
            assert!(
                output_summary.contains("errors"),
                "子 worker 结果应折叠回父上下文，output: {output_summary}"
            );
        }
        other => panic!("delegate_analysis 应成功完成，实际: {other:?}"),
    }
    // 3) 终态成功
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::SessionFinished { summary, .. } if summary == "已派发子任务并汇总"
        )),
        "应以 SessionFinished 结束，events: {events:?}"
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&script_path);
    let _ = std::fs::remove_file(deps.annotations.path());
}
