//! `qview-agent-cli` — 调试入口（架构 §17.2 P2 任务）。
//!
//! ## 用法
//! ```
//! # 默认：Mock provider + 脚本演示（离线）
//! cargo run -p qview-agent --features mock-provider --bin qview-agent-cli -- /path/to/log.log "找 5xx 错误"
//!
//! # 真实 LLM：OpenAI 兼容 / Ollama / DeepSeek
//! cargo run -p qview-agent --bin qview-agent-cli -- --provider ollama --model llama3 /path/log.log "总结这段"
//! cargo run -p qview-agent --bin qview-agent-cli -- --provider openai --model gpt-4o-mini --api-key-env OPENAI_API_KEY /path/log.log "..."
//! ```
//!
//! 与 GUI 一致：CLI 也是"配置 → `AgentConfig::build(deps)` → handle"。
//! 这就是 qview-agent 暴露配置类型给调用方（UI/CLI）的方式。

use std::sync::Arc;

use clap::Parser;
use serde_json::json;

use qview_agent::config::{AgentConfig, AgentDeps, LlmProvider};
use qview_agent::event::AgentEvent;
use qview_agent::handle::AgentGoal;
use qview_agent::sink::ChannelSink;

#[derive(Parser, Debug)]
#[command(version, about = "qview 器灵 CLI 调试入口")]
struct Cli {
    /// 日志文件路径。
    file: std::path::PathBuf,
    /// 用户目标（一句话）。
    query: String,

    /// LLM provider：mock / openai / openaicompat / ollama / deepseek。
    #[arg(long, default_value = "mock")]
    provider: String,

    /// 模型名（真实 provider 需要）。
    #[arg(long)]
    model: Option<String>,

    /// 自定义端点（openai_compat 必填；ollama 缺省 localhost:11434）。
    #[arg(long)]
    base_url: Option<String>,

    /// 从哪个环境变量读 API key。
    #[arg(long)]
    api_key_env: Option<String>,

    /// Mock 脚本 JSON 文件路径（Vec<LLMResponse>；缺省用内置演示脚本）。
    #[arg(long)]
    script: Option<std::path::PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber_init();
    // 原始 LLM 请求/响应日志：默认当前目录 llm_raw.log（contexa-llm 读
    // QVIEW_LLM_RAW_LOG；已设则不覆盖，置空可关闭）。
    if std::env::var_os("QVIEW_LLM_RAW_LOG").is_none() {
        std::env::set_var("QVIEW_LLM_RAW_LOG", "llm_raw.log");
    }
    let cli = Cli::parse();

    // 1) 打开文档 + 建服务
    let docs = Arc::new(qview_application::service::DocumentService::default());
    let doc_id = docs.open(cli.file.clone())?;
    eprintln!("[cli] opened {:?} as {doc_id}", cli.file);
    let search = Arc::new(qview_application::service::SearchService::new(docs.clone()));
    let ann = Arc::new(qview_application::service::annotation::AnnotationService::new(docs.clone()));
    let deps = AgentDeps {
        docs,
        search,
        annotations: ann,
        viewport: qview_application::tools::SharedViewport::default(),
        // CLI 调试默认不落库；需要时由调用方显式传入 store。
        store: None,
    };

    // 2) 配置（用户从命令行派生 AgentConfig）
    let mut config = build_config(&cli, doc_id)?;
    config.allow_all_tools();
    config.instance_id = "qview-agent-cli".into();

    // 3) 一站式装配
    let handle = config.build(deps)?;

    // 4) 订阅（保留 sink 强引用，避免 Weak 订阅立即失效）
    let sink = ChannelSink::new();
    let mut pri = sink.take_priority_receiver().unwrap();
    let mut rx = sink.take_receiver().unwrap();
    let _guard = handle.subscribe(sink.clone());
    let _keep_sink_alive = sink;

    // 5) 启动
    let goal = AgentGoal::new(cli.query.clone()).with_spec("cli", "CLI 调试", cli.query.clone());
    let sid = handle.start_session(goal).await?;
    eprintln!("[cli] started session {sid}");

    // 6) 消费事件直到终态（先 drain 普通通道，再查 priority 通道）
    consume_until_done(&mut pri, &mut rx, std::time::Duration::from_secs(60)).await;

    eprintln!("[cli] session {sid} ended");
    Ok(())
}

fn is_terminal(e: &AgentEvent) -> bool {
    matches!(
        e,
        AgentEvent::SessionFinished { .. } | AgentEvent::Failed { .. } | AgentEvent::Cancelled { .. }
    )
}

/// 消费直到出现终态事件；出现后把普通通道里残留的进度事件也一并 drain。
async fn consume_until_done(
    pri: &mut tokio::sync::mpsc::Receiver<AgentEvent>,
    rx: &mut tokio::sync::mpsc::Receiver<AgentEvent>,
    overall_timeout: std::time::Duration,
) {
    let deadline = std::time::Instant::now() + overall_timeout;
    loop {
        if std::time::Instant::now() > deadline {
            eprintln!("[cli] timeout — bailing out");
            return;
        }
        // 1) drain 普通通道（非阻塞式，直到 100ms 没事件）
        loop {
            match tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await {
                Ok(Some(e)) => print_event(&e),
                Ok(None) | Err(_) => break,
            }
        }
        // 2) 查 priority 通道（终态事件）
        match tokio::time::timeout(std::time::Duration::from_millis(100), pri.recv()).await {
            Ok(Some(e)) => {
                print_event(&e);
                if is_terminal(&e) {
                    // 3) 终态出现后，再 drain 一次普通通道残留
                    while let Ok(Some(e)) =
                        tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await
                    {
                        print_event(&e);
                    }
                    return;
                }
            }
            Ok(None) | Err(_) => {}
        }
    }
}

/// 按 CLI 参数构造 AgentConfig（provider 等）。
fn build_config(cli: &Cli, doc_id: qview_application::protocol::DocumentId) -> anyhow::Result<AgentConfig> {
    let provider = match cli.provider.as_str() {
        "mock" => LlmProvider::Mock,
        "openai" => LlmProvider::OpenAI,
        "openaicompat" => LlmProvider::OpenAICompat,
        "ollama" => LlmProvider::Ollama,
        "deepseek" => LlmProvider::DeepSeek,
        other => anyhow::bail!("未知 provider: {other}（mock/openai/openaicompat/ollama/deepseek）"),
    };

    let mut config = AgentConfig::default();
    config.provider.provider = provider;
    config.provider.model = cli.model.clone().unwrap_or_default();
    config.provider.base_url = cli.base_url.clone();
    config.provider.api_key_env = cli.api_key_env.clone();

    // Mock：用演示脚本（或用户 --script）
    if provider == LlmProvider::Mock {
        let script_path = match &cli.script {
            Some(p) => p.clone(),
            None => {
                // 内置演示脚本：get_document_info → worker_finish
                let script = vec![
                    contexa_llm::LLMResponse {
                        content: "".into(),
                        tool_calls: vec![contexa_llm::ToolCall::new(
                            "c1",
                            "get_document_info",
                            json!({"document_id": doc_id.get()}),
                        )],
                        usage: Default::default(),
                        raw: None,
                    },
                    contexa_llm::LLMResponse {
                        content: "".into(),
                        tool_calls: vec![contexa_llm::ToolCall::new(
                            "c2",
                            "worker_finish",
                            json!({"status": "success", "result": null, "summary": "CLI 演示完成"}),
                        )],
                        usage: Default::default(),
                        raw: None,
                    },
                ];
                let p = std::env::temp_dir().join(format!("qview-cli-script-{}.json", uuid::Uuid::new_v4()));
                std::fs::write(&p, serde_json::to_string(&script)?)?;
                p
            }
        };
        config.provider.mock_script_path = Some(script_path);
    }
    Ok(config)
}

fn print_event(e: &AgentEvent) {
    let line = match e {
        AgentEvent::SessionStarted { session_id, goal, .. } => {
            format!("[START {session_id}] goal={goal:?}")
        }
        AgentEvent::PhaseChanged { phase, .. } => format!("[PHASE] {phase:?}"),
        AgentEvent::ToolCallStarted { tool, call_id, .. } => {
            format!("[TOOL_CALL_START {call_id}] {tool}")
        }
        AgentEvent::ToolCallFinished {
            call_id,
            tool,
            output_summary,
            duration_ms,
            is_error,
            ..
        } => format!(
            "[TOOL_CALL_FINISH {call_id}] {tool} err={is_error} dur={duration_ms}ms summary={output_summary:?}"
        ),
        AgentEvent::ViewIntentEmitted { intent, .. } => {
            format!("[VIEW_INTENT] {intent:?}")
        }
        AgentEvent::MessageEmitted { text, .. } => {
            format!("[MSG] {}", text.chars().take(120).collect::<String>())
        }
        AgentEvent::SessionFinished { status, summary, .. } => {
            format!("[FINISHED {status:?}] {summary}")
        }
        AgentEvent::Cancelled { .. } => "[CANCELLED]".into(),
        AgentEvent::Failed { error, .. } => format!("[FAILED] {error}"),
        AgentEvent::ProposalCreated { proposal, .. } => {
            format!("[PROPOSAL {}] {}", proposal.id, proposal.reason)
        }
        AgentEvent::ApprovalRequired { proposal_id, .. } => {
            format!("[APPROVAL_REQUIRED {proposal_id}]")
        }
        AgentEvent::ToolCallProgress { message, progress, .. } => {
            format!("[PROGRESS] {message} {progress:?}")
        }
    };
    eprintln!("{line}");
}

fn tracing_subscriber_init() {
    use tracing_subscriber::{fmt, EnvFilter};
    let _ = fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .try_init();
}
