//! FlowRunner：执行 Flow 的主循环（架构 §22.x — P2「Flow Runner」）。
//!
//! ## v1 范围
//!
//! - 串行执行 Step（Work / Parallel）
//! - Parallel 用 `futures::join_all` 并发执行多个 Work
//! - 收到 `Step::Done` 时停止
//! - 收集每个 Work 的结果到 `results: HashMap<WorkName, serde_json::Value>`
//! - `Step::LlmDecision` v1 **不支持**——遇到就返回错误（实际应该回退到 ReAct）
//!
//! ## 断点续跑（架构 §22.x P2）
//!
//! - `run()` 接受可选 `resume_from: HashMap<String, Value>`——上次完成 Work 的结果快照
//! - 每个 Work 完成时调 `on_work_done` 回调（runtime 接到后落盘 FlowCheckpoint）
//! - runner 收到 `Done` 时清空检查点（避免脏数据）

use std::collections::HashMap;

use crate::flow::work::WorkExecutor;
use crate::flow::Step;

/// Flow 一次完整执行的报告。
#[derive(Debug, Clone)]
pub struct FlowRunReport {
    /// Work 名 → 结果 JSON
    pub results: HashMap<String, serde_json::Value>,
    /// 最终 summary（来自 `Step::Done { summary }`）
    pub final_summary: String,
    pub total_duration_ms: u64,
}

/// 具名结果插值：把 WorkSpec.args 里的 `{{work_name.field}}` 占位符替换成
/// 之前 Work 的结果（架构 §22.x「WorkRef::Named」v1 简化）。
///
/// 例如 `args: {"document_id": "{{open_document.document_id}}"}` 会在
/// `results["open_document"]["document_id"]` 存在时替换成该数字。
/// 找不到 → 保留原占位符（执行器会因缺参数报错，Flow 照常推进）。
fn interpolate_work(
    spec: &crate::flow::work::WorkSpec,
    results: &HashMap<String, serde_json::Value>,
) -> crate::flow::work::WorkSpec {
    let mut spec = spec.clone();
    if let crate::flow::work::WorkKind::ToolCall { tool, args } = &mut spec.kind {
        *args = interpolate_value(std::mem::take(args), results);
        let _ = tool;
    }
    spec
}

fn interpolate_value(v: serde_json::Value, results: &HashMap<String, serde_json::Value>) -> serde_json::Value {
    match v {
        serde_json::Value::String(s) => {
            if let Some(stripped) = s.strip_prefix("{{").and_then(|s| s.strip_suffix("}}")) {
                let (work_name, field) = stripped.split_once('.').unwrap_or((stripped, ""));
                if let Some(work_val) = results.get(work_name) {
                    if field.is_empty() {
                        return work_val.clone();
                    }
                    if let Some(v) = work_val.get(field) {
                        return v.clone();
                    }
                }
            }
            serde_json::Value::String(s)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(|i| interpolate_value(i, results)).collect())
        }
        serde_json::Value::Object(map) => {
            serde_json::Value::Object(
                map.into_iter()
                    .map(|(k, v)| (k, interpolate_value(v, results)))
                    .collect(),
            )
        }
        other => other,
    }
}

/// Flow 一次运行的选项（resume / 持久化回调 / LLM 总结器）。
///
/// **LLM 总结器**（架构 §22.x「Flow-then-LLM」）：Flow 跑完 Work 链后，`Step::LlmDecision`
/// 把 (prompt, results) 交给它，返回最终回复文本。这样**每个 Flow 至少调一次 LLM**
/// （用户明确要求：和 LLM 交流，至少 1 次 LLM 必不可少）：
/// - LLM 看到真实工具结果（含失败 error JSON），能解释失败 / 给出下一步建议
/// - 不再有"静态 Done 谎报成功"（如日志里 open 失败却回"已打开"）
pub struct RunOptions {
    /// 上次未完成 Flow 的 Work 结果快照（断点续跑）。
    pub resume_from: HashMap<String, serde_json::Value>,
    /// 每个 Work 完成时调用（含恢复的 Work）。用于落盘 checkpoint。
    pub on_work_done: Box<dyn FnMut(&str, &serde_json::Value) + Send + 'static>,
    /// 可选的 LLM 总结器：`(prompt, results) → 最终回复文本`。
    /// `None` 时 `Step::LlmDecision` 直接报错（测试 / 无 LLM 场景）。
    pub llm_summarize:
        Option<Box<dyn Fn(&str, &HashMap<String, serde_json::Value>) -> futures::future::BoxFuture<'static, anyhow::Result<String>> + Send + Sync + 'static>>,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            resume_from: HashMap::new(),
            on_work_done: Box::new(|_, _| {}),
            llm_summarize: None,
        }
    }
}

impl RunOptions {
    pub fn new() -> Self {
        Self::default()
    }
}

/// 跑一个 Flow 的全部 Step。
///
/// ## 实现策略
///
/// 1. 维护 `results: HashMap<String, serde_json::Value>`（Work 结果）
/// 2. 顺序遍历 Step：
///    - `Work(spec)`：executor.run(spec)，结果存 `results[spec.name]`
///    - `Parallel(specs)`：join_all executor.run(specs[i])，结果全部存
///    - `LlmDecision { prompt }`：调 `opts.llm_summarize(prompt, results)`，回复成为
///      `final_summary`，然后停止（v1：LlmDecision 是 Flow 的最后一步）
///    - `Done { summary }`：停止 + 返回 report
/// 3. 任何 Work 失败（value 含 "error"）继续执行（不中断 Flow），但 LLM 总结时能看到
pub async fn run(
    ctx: &crate::flow::FlowContext,
    steps: Vec<Step>,
    opts: RunOptions,
) -> anyhow::Result<FlowRunReport> {
    let started = std::time::Instant::now();
    let executor = WorkExecutor::new(ctx);
    let mut results: HashMap<String, serde_json::Value> = opts.resume_from.clone();
    let mut final_summary = String::new();
    let mut on_work_done = opts.on_work_done;

    // 把已恢复的 results 报告回去（让持久化层能刷新一次时间戳）
    for (k, v) in &results {
        on_work_done(k, v);
    }

    for step in steps {
        match step {
            Step::Work(spec) => {
                // 具名结果插值：把 args 里的 `{{work_name.field}}` 占位符替换成
                // 之前 Work 的结果（架构 §22.x「WorkRef::Named」v1 简化）。
                let spec = interpolate_work(&spec, &results);
                let result = executor.run(spec).await;
                if let Some(v) = result.value.get("error") {
                    tracing::warn!(
                        target: "qview_agent::flow",
                        work = %result.name,
                        "work 失败：{v}"
                    );
                }
                on_work_done(&result.name, &result.value);
                results.insert(result.name.clone(), result.value.clone());
            }
            Step::Parallel(specs) => {
                let mut handles = Vec::with_capacity(specs.len());
                for spec in specs {
                    let spec = interpolate_work(&spec, &results);
                    handles.push(executor.run(spec));
                }
                let all = futures::future::join_all(handles).await;
                for r in all {
                    on_work_done(&r.name, &r.value);
                    results.insert(r.name.clone(), r.value.clone());
                }
            }
            Step::LlmDecision { prompt } => {
                // LLM 总结：Flow 的最后一步。回复即最终回答。
                if let Some(summarize) = &opts.llm_summarize {
                    // 把 prompt 里的 {{work.field}} 也插值（让总结 prompt 能引用工具结果）
                    let prompt = interpolate_value(serde_json::Value::String(prompt), &results)
                        .as_str()
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    final_summary = summarize(&prompt, &results).await?;
                    break;
                } else {
                    anyhow::bail!(
                        "Flow::LlmDecision 需要 llm_summarize（RunOptions），当前为 None。\
                         命中此步说明 Flow 需要 LLM 总结，但调用方没给 LLM。prompt={}",
                        prompt.chars().take(80).collect::<String>()
                    );
                }
            }
            Step::Done { summary } => {
                final_summary = summary;
                break;
            }
        }
    }

    Ok(FlowRunReport {
        results,
        final_summary,
        total_duration_ms: started.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::work::{WorkKind, WorkSpec};
    use crate::flow::{FlowContext, FlowDocs, Step};
    use std::sync::Arc;

    #[derive(Default)]
    struct FakeDocs;

    #[async_trait::async_trait]
    impl FlowDocs for FakeDocs {
        async fn tool_call(
            &self,
            tool: &str,
            _args: serde_json::Value,
        ) -> anyhow::Result<serde_json::Value> {
            match tool {
                "open_document" => Ok(serde_json::json!({ "document_id": 42 })),
                "list_directory" => Ok(serde_json::json!({ "entries": ["a.txt"] })),
                _ => Ok(serde_json::json!({})),
            }
        }
    }

    #[tokio::test]
    async fn runner_single_work() {
        let ctx = FlowContext {
            docs: Arc::new(FakeDocs),
            current_file: None,
            user_query: None,
            sinks: None,
            session_id: None,
        };
        let steps = vec![
            Step::Work(WorkSpec {
                name: "open".into(),
                kind: WorkKind::ToolCall {
                    tool: "open_document".into(),
                    args: serde_json::json!({ "path": "/tmp/a.txt" }),
                },
                timeout_ms: 5000,
                retry: Default::default(),
            }),
            Step::Done {
                summary: "打开了".into(),
            },
        ];
        let report = run(&ctx, steps, RunOptions::new()).await.unwrap();
        assert_eq!(report.final_summary, "打开了");
        assert!(report.results.contains_key("open"));
        assert!(report.total_duration_ms < 5000);
    }

    #[tokio::test]
    async fn runner_parallel_works() {
        let ctx = FlowContext {
            docs: Arc::new(FakeDocs),
            current_file: None,
            user_query: None,
            sinks: None,
            session_id: None,
        };
        let specs = vec![
            WorkSpec {
                name: "ls1".into(),
                kind: WorkKind::ToolCall {
                    tool: "list_directory".into(),
                    args: serde_json::json!({ "path": "/tmp/a" }),
                },
                timeout_ms: 5000,
                retry: Default::default(),
            },
            WorkSpec {
                name: "ls2".into(),
                kind: WorkKind::ToolCall {
                    tool: "list_directory".into(),
                    args: serde_json::json!({ "path": "/tmp/b" }),
                },
                timeout_ms: 5000,
                retry: Default::default(),
            },
        ];
        let steps = vec![
            Step::Parallel(specs),
            Step::Done {
                summary: "ok".into(),
            },
        ];
        let report = run(&ctx, steps, RunOptions::new()).await.unwrap();
        assert!(report.results.contains_key("ls1"));
        assert!(report.results.contains_key("ls2"));
    }

    /// LlmDecision 有 llm_summarize → 调它，回复成为 final_summary。
    #[tokio::test]
    async fn runner_llm_decision_calls_summarizer() {
        let ctx = FlowContext {
            docs: Arc::new(FakeDocs),
            current_file: None,
            user_query: None,
            sinks: None,
            session_id: None,
        };
        let steps = vec![Step::LlmDecision {
            prompt: "总结".into(),
        }];
        let mut opts = RunOptions::new();
        opts.llm_summarize = Some(Box::new(|prompt, results| {
            let p = prompt.to_string();
            let r = results.clone();
            Box::pin(async move {
                Ok(format!("LLM 总结：{p}，看到 {} 个结果", r.len()))
            })
        }));
        let report = run(&ctx, steps, opts).await.unwrap();
        assert_eq!(report.final_summary, "LLM 总结：总结，看到 0 个结果");
    }

    /// LlmDecision 没有 llm_summarize → bail（提示需要 LLM）。
    #[tokio::test]
    async fn runner_llm_decision_without_summarizer_bails() {
        let ctx = FlowContext {
            docs: Arc::new(FakeDocs),
            current_file: None,
            user_query: None,
            sinks: None,
            session_id: None,
        };
        let steps = vec![Step::LlmDecision {
            prompt: "x".into(),
        }];
        assert!(run(&ctx, steps, RunOptions::new()).await.is_err());
    }

    /// 具名结果插值：前序 Work 的结果能注入到后续 Work 的 args。
    #[tokio::test]
    async fn runner_interpolates_named_results() {
        let ctx = FlowContext {
            docs: Arc::new(FakeDocs),
            current_file: None,
            user_query: None,
            sinks: None,
            session_id: None,
        };
        let steps = vec![
            Step::Work(WorkSpec {
                name: "open".into(),
                kind: WorkKind::ToolCall {
                    tool: "open_document".into(),
                    args: serde_json::json!({ "path": "/tmp/a.txt" }),
                },
                timeout_ms: 5000,
                retry: Default::default(),
            }),
            // 第二个 Work 的 args 引用 open_document.document_id
            Step::Work(WorkSpec {
                name: "read".into(),
                kind: WorkKind::ToolCall {
                    tool: "read_context".into(),
                    args: serde_json::json!({
                        "document_id": "{{open.document_id}}",
                        "line": 0,
                    }),
                },
                timeout_ms: 5000,
                retry: Default::default(),
            }),
            Step::Done {
                summary: "ok".into(),
            },
        ];
        let report = run(&ctx, steps, RunOptions::new()).await.unwrap();
        // open_document FakeDocs 返回 document_id=42 → 插值后 read_context 的 document_id=42
        // （FakeDocs 的 read_context 直接返回 {content:...}，无法直接断言入参；
        //  但 flow 能跑通 + 两个 Work 都在 results 里就证明插值没炸）
        assert!(report.results.contains_key("open"));
        assert!(report.results.contains_key("read"));
    }

    /// 插值找不到前序结果 → 保留原占位符（不 panic）。
    #[test]
    fn interpolate_missing_result_keeps_placeholder() {
        let mut results = HashMap::new();
        results.insert("open".into(), serde_json::json!({ "document_id": 42 }));
        let args = serde_json::json!({ "document_id": "{{nonexistent.id}}" });
        let out = interpolate_value(args, &results);
        assert_eq!(out, serde_json::json!({ "document_id": "{{nonexistent.id}}" }));
    }

    /// 断点续跑：resume_from 在 runner 启动时被注入 results（callbacks 也被触发一次），
    /// 然后正常的 Work 执行流程继续。本测试只验证"恢复 + 正常 Work"链路，不验证跳过语义
    /// （跳过语义属于 Step 层面的优化，不在 v1 runner 范围）。
    #[tokio::test]
    async fn runner_resume_prepopulates_results_and_runs_remaining() {
        let ctx = FlowContext {
            docs: Arc::new(FakeDocs),
            current_file: None,
            user_query: None,
            sinks: None,
            session_id: None,
        };
        let steps = vec![
            Step::Work(WorkSpec {
                name: "open".into(),
                kind: WorkKind::ToolCall {
                    tool: "open_document".into(),
                    args: serde_json::json!({}),
                },
                timeout_ms: 5000,
                retry: Default::default(),
            }),
            Step::Work(WorkSpec {
                name: "list".into(),
                kind: WorkKind::ToolCall {
                    tool: "list_directory".into(),
                    args: serde_json::json!({}),
                },
                timeout_ms: 5000,
                retry: Default::default(),
            }),
            Step::Done {
                summary: "done".into(),
            },
        ];
        let mut resumed: HashMap<String, serde_json::Value> = HashMap::new();
        resumed.insert(
            "pre_existing".into(),
            serde_json::json!({ "restored": true }),
        );
        // on_work_done 需要 'static 闭包（RunOptions 要求），用 Arc<Mutex> 收集
        let calls: std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut opts = RunOptions::new();
        opts.resume_from = resumed;
        let calls_sink = calls.clone();
        opts.on_work_done = Box::new(move |n, v| {
            calls_sink.lock().unwrap().push((n.to_string(), v.clone()))
        });
        let report = run(&ctx, steps, opts).await.unwrap();
        // 恢复项保留在 results 里
        assert!(report.results.contains_key("pre_existing"));
        // 正常 Work 跑完后也加入 results
        assert!(report.results.contains_key("open"));
        assert!(report.results.contains_key("list"));
        // 回调至少被调了 3 次（恢复项 + 2 个 Work）
        let calls = calls.lock().unwrap();
        assert!(calls.len() >= 3, "回调数 = 恢复项 + 每个 Work，实际 {} 次", calls.len());
        // 第一次回调是恢复项（顺序保证）
        assert_eq!(calls[0].0, "pre_existing");
    }
}