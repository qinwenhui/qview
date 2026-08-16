//! `AgentConfig::build()` 端到端测试：Mock provider 下 UI 只需两步——
//! 1. 用自己的配置派生 AgentConfig
//! 2. AgentConfig::build(deps) → AgentRuntimeHandle
//!
//! 验证：handle 可订阅、可 start_session、事件流正常、终态正确。

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::json;

use contexa_llm::{LLMResponse, ToolCall};

use qview_application::service::annotation::AnnotationService;
use qview_application::service::{DocumentService, SearchService};

use qview_agent::config::{AgentConfig, AgentDeps, LlmProvider};
use qview_agent::event::{AgentEvent, AgentSink};

/// 意图分类会消耗第一条 LLM 响应。Mock 脚本第一条必须返回分类 JSON。
/// 返回 Unknown → runtime 走完整 ReAct，用脚本剩余部分。
/// 意图分类会消耗第一条 LLM 响应（LLM 调用 route_intent 工具）。
/// Mock 脚本第一条必须返回一个 route_intent tool_call（Unknown → 走完整 ReAct）。
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
    p.push(format!("qview-cfg-{}.log", uuid::Uuid::new_v4()));
    std::fs::write(&p, "ERROR a\nINFO b\n").unwrap();
    p
}

fn make_deps(path: std::path::PathBuf) -> (AgentDeps, qview_application::protocol::DocumentId) {
    let docs = Arc::new(DocumentService::default());
    let id = docs.open(path.clone()).unwrap();
    let search = Arc::new(SearchService::new(docs.clone()));
    let ann = Arc::new(AnnotationService::with_path(
        docs.clone(),
        std::env::temp_dir().join(format!("qview-cfg-ann-{}.json", uuid::Uuid::new_v4())),
    ));
    (
        AgentDeps {
            docs,
            search,
            annotations: ann,
            viewport: qview_application::tools::SharedViewport::default(),
            store: None,
        },
        id,
    )
}

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

#[tokio::test]
async fn build_mock_provider_runs_session() {
    let path = fixture_log();
    let (deps, id) = make_deps(path.clone());

    // UI 从自己的配置派生 AgentConfig
    let mut config = AgentConfig::mock("(mock)");
    config.instance_id = "test-egui".into();
    config.allow_tools = qview_application::tools::ALL_TOOL_NAMES_WITH_WRITES
        .iter()
        .map(|s| s.to_string())
        .collect();

    // 注意：Mock 静态响应不会产生工具调用；直接跑 end-to-end 需要脚本。
    // 这里改用带脚本的 DummyLLM —— 通过 mock_script_path 提供。
    let script = vec![
        classify_json(), // 意图分类
        LLMResponse {
            content: "".into(),
            tool_calls: vec![ToolCall::new(
                "c1",
                "get_document_info",
                json!({"document_id": id.get()}),
            )],
            usage: Default::default(),
            raw: None,
        },
        LLMResponse {
            content: "".into(),
            tool_calls: vec![ToolCall::new(
                "c2",
                "worker_finish",
                json!({"status": "success", "result": null, "summary": "done"}),
            )],
            usage: Default::default(),
            raw: None,
        },
    ];
    // 写到临时脚本文件
    let script_path = std::env::temp_dir().join(format!("qview-cfg-script-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(&script_path, serde_json::to_string(&script).unwrap()).unwrap();
    config.provider.mock_script_path = Some(script_path.clone());
    config.provider.provider = LlmProvider::Mock;

    // build → handle
    let handle = config.build(deps.clone()).expect("build");
    let sink = Arc::new(CollectingSink::default());
    let _g = handle.subscribe(sink.clone());

    let sid = handle
        .start_session(qview_agent::AgentGoal::new("inspect"))
        .await
        .unwrap();

    // 等终态
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let events = sink.events.lock();
        if events.iter().any(|e| {
            matches!(
                e,
                AgentEvent::SessionFinished { .. } | AgentEvent::Failed { .. } | AgentEvent::Cancelled { .. }
            )
        }) {
            break;
        }
        drop(events);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let events = sink.events.lock();
    assert!(!events.is_empty(), "应收到事件");
    assert!(
        matches!(events.first(), Some(AgentEvent::SessionStarted { .. })),
        "首事件应为 SessionStarted"
    );
    let last = events.last().unwrap();
    assert!(
        matches!(last, AgentEvent::SessionFinished { .. }),
        "末事件应为 SessionFinished，实际: {last:?}"
    );
    if let AgentEvent::SessionStarted { session_id, .. } = &events[0] {
        assert_eq!(session_id, &sid);
    }

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&script_path);
    // 清理 annotation 临时文件
    let _ = std::fs::remove_file(deps.annotations.path());
}

#[tokio::test]
async fn build_requires_doc_id_from_deps() {
    // 验证 build() 后能通过 DocumentService 打开文档并让工具读到
    let path = fixture_log();
    let (deps, id) = make_deps(path.clone());

    let mut config = AgentConfig::mock("hi");
    config.allow_all_tools();
    let script = vec![
        classify_json(), // 意图分类
        LLMResponse {
            content: "".into(),
            tool_calls: vec![ToolCall::new(
                "c1",
                "get_document_info",
                json!({"document_id": id.get()}),
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
    let script_path = std::env::temp_dir().join(format!("qview-cfg-s2-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(&script_path, serde_json::to_string(&script).unwrap()).unwrap();
    config.provider.mock_script_path = Some(script_path.clone());

    let handle = config.build(deps.clone()).expect("build");
    let sink = Arc::new(CollectingSink::default());
    let _g = handle.subscribe(sink.clone());
    let _ = handle.start_session(qview_agent::AgentGoal::new("x")).await.unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let events = sink.events.lock();
        if events
            .iter()
            .any(|e| matches!(e, AgentEvent::SessionFinished { .. }))
        {
            break;
        }
        drop(events);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 至少看到一次 get_document_info 工具调用（ToolCallStarted），且终态为 SessionFinished
    let events = sink.events.lock();
    let saw_info_started = events.iter().any(|e| {
        matches!(
            e,
            AgentEvent::ToolCallStarted { tool, .. } if tool == "get_document_info"
        )
    });
    assert!(saw_info_started, "get_document_info 应被执行");
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::SessionFinished { .. })));

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&script_path);
}

#[test]
fn provider_config_maps_openai() {
    let mut p = qview_agent::ProviderConfig::default();
    p.provider = LlmProvider::OpenAI;
    p.model = "gpt-4o".into();
    p.api_key_env = Some("QVIEW_TEST_KEY".into());
    // 设一个假 env 验证能读到
    unsafe { std::env::set_var("QVIEW_TEST_KEY", "sk-test") };
    let key = p.api_key();
    assert_eq!(key.as_deref(), Some("sk-test"));
}
