//! 5 个内置 Flow 实现（架构 §22.x — P2「Flow」落地）。
//!
//! 所有 Flow 调底层工具都通过 `ToolRegistry`（与 ReAct 完全一致）：
//! - `open_document` / `list_directory` / `search_text` / `read_context`
//! - `create_annotation` / `export_report` / `write_document`
//!
//! **关键 Flow 不再需要 LlmDecision**（v1 改造）：
//! - SearchLogAndReportFlow：plan 阶段用 regex 从 query 提取关键词；后续 Work 链全是 ToolCall
//! - AnnotateFileFlow：plan 阶段读全文 + 按关键字 heuristic 挑批注
//! - ExportCurrentReportFlow：plan 阶段读全文 + 生成 markdown 模板 + export

use crate::flow::work::{RetryPolicy, WorkKind, WorkSpec};
use crate::flow::{Flow, FlowContext, FlowId, Step};
use crate::intent::Intent;

/// 判断两个文件路径是否指向同一个文件（轻量归一化，不落盘、不访问文件系统）。
///
/// - 去掉 Windows 扩展路径前缀 `\\?\` / `\\.\`
/// - 统一 `\` 与 `/`
/// - 大小写不敏感（Windows 文件系统）
///
/// 用途：OpenFileFlow 判断"目标文件就是当前已打开文件"时跳过重复 open_document。
fn same_file_path(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> String {
        s.trim()
            .trim_start_matches(r"\\?\")
            .trim_start_matches(r"\\.\")
            .replace('/', r"\")
            .to_lowercase()
    }
    norm(a) == norm(b) && !norm(a).is_empty()
}

/// 构造一个 WorkSpec（带默认 30s 超时 + 指数退避重试 2 次）。
fn work(name: &str, tool: &str, args: serde_json::Value) -> WorkSpec {
    WorkSpec {
        name: name.to_string(),
        kind: WorkKind::ToolCall {
            tool: tool.to_string(),
            args,
        },
        timeout_ms: 30_000,
        retry: RetryPolicy::Exponential {
            max_retries: 2,
            base_ms: 200,
        },
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// OpenFile
// ─────────────────────────────────────────────────────────────────────────────

/// 最简单 Flow：单 Work。
pub struct OpenFileFlow;

impl Flow for OpenFileFlow {
    fn id(&self) -> FlowId {
        FlowId::OpenFile
    }

    fn plan(&self, intent: &Intent, ctx: &FlowContext) -> anyhow::Result<Vec<Step>> {
        let path = intent
            .params
            .get("path")
            .ok_or_else(|| anyhow::anyhow!("OpenFileFlow 缺 path 参数"))?;

        // 幂等：目标文件就是当前打开的文件 → 跳过 open_document（避免"重复打开同个文件"）。
        // 比较前做轻量归一化（去掉 \\?\ 扩展路径前缀、统一分隔符、大小写），
        // 因为 list_directory 等工具返回的是 `\\?\D:\...` 形式。
        let already_current = ctx
            .current_file
            .as_ref()
            .map(|cur| same_file_path(cur, path))
            .unwrap_or(false);

        let mut steps = Vec::new();
        if !already_current {
            steps.push(Step::Work(work(
                "open_document",
                "open_document",
                serde_json::json!({ "path": path }),
            )));
        }
        steps.push(Step::LlmDecision {
            prompt: format!(
                "用户想打开文件 {path} 并看看内容。请根据工具结果向用户汇报：\
                 如果打开成功，简述这个文件是什么、大致内容；如果失败（error 字段），\
                 说明原因，并建议用户可能的正确路径（比如去掉多余字词、补全扩展名）。\
                 {}",
                if already_current {
                    "（该文件已在视图中打开，无需重复打开，直接向用户说明即可。）"
                } else {
                    ""
                }
            ),
        });
        Ok(steps)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ListDir
// ─────────────────────────────────────────────────────────────────────────────

pub struct ListDirFlow;

impl Flow for ListDirFlow {
    fn id(&self) -> FlowId {
        FlowId::ListDir
    }

    fn plan(&self, intent: &Intent, _ctx: &FlowContext) -> anyhow::Result<Vec<Step>> {
        let path = intent
            .params
            .get("path")
            .ok_or_else(|| anyhow::anyhow!("ListDirFlow 缺 path 参数"))?;
        Ok(vec![
            Step::Work(work(
                "list_directory",
                "list_directory",
                serde_json::json!({ "path": path }),
            )),
            Step::LlmDecision {
                prompt: format!(
                    "用户要求列出目录 {path} 下的文件。请根据 list_directory 工具结果向用户汇报：\
                     整理成清晰的清单（可区分文件 / 子文件夹），标出值得注意的大文件或日志文件，\
                     并给出简短点评和后续可做什么。"
                ),
            },
        ])
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SearchLogAndReport
// ─────────────────────────────────────────────────────────────────────────────

/// 关键场景 Flow：查生产日志 → 出报告（架构 §22.x）。
///
/// 完整流程（**v1 全程不调 LLM**）：
/// 1. `open_document(path)` 拿到 `document_id`
/// 2. `search_text(document_id, query=提取的关键词, limit=20)` 拿 top 命中
/// 3. `Parallel { read_context × N }` 并行取每条命中前后 10 行的上下文
/// 4. `export_report(document_id, content=汇总)` 把所有命中拼成报告写出
///
/// 关键词提取（plan 阶段正则）：
/// - "找/查/search/find/grep 关键字" → 提取首个 `\w{2,}` 串
/// - "ERROR / WARN / 错误" 等日志级别 → 当成关键词
/// - 没有关键字 → 退化为整 query
///
/// v2 可把这一步换成 LlmDecision（plan 时调一次小 LLM）。
pub struct SearchLogAndReportFlow;

impl Flow for SearchLogAndReportFlow {
    fn id(&self) -> FlowId {
        FlowId::SearchLogAndReport
    }

    fn plan(&self, intent: &Intent, ctx: &FlowContext) -> anyhow::Result<Vec<Step>> {
        let path = intent
            .params
            .get("path")
            .or_else(|| ctx.current_file.as_ref())
            .ok_or_else(|| anyhow::anyhow!("SearchLogAndReportFlow 缺文件路径"))?
            .clone();

        // 关键词：LLM 分类时已抽好（params["query"]）；没抽到就用原始用户输入原样
        // 交给 search_text（模糊匹配交给模型 / 搜索本身，不做代码剥词）。
        let query = intent
            .params
            .get("query")
            .cloned()
            .filter(|q| !q.trim().is_empty())
            .unwrap_or_else(|| intent.kind.as_str().to_string());

        // 1) open_document → document_id 由 runner 具名插值给后续 Work
        let steps = vec![
            Step::Work(work(
                "open_document",
                "open_document",
                serde_json::json!({ "path": path }),
            )),
            Step::Work(work(
                "search_text",
                "search_text",
                serde_json::json!({
                    "document_id": "{{open_document.document_id}}",
                    "query": query,
                    "limit": 20
                }),
            )),
            Step::LlmDecision {
                prompt: format!(
                    "用户想查日志文件 {path} 中与 {query:?} 相关的条目。\
                     请根据 search_text 工具结果向用户汇报：命中总数、Top 命中示例、\
                     有没有明显的问题模式（如 ERROR 密集、重复出现某模块），给出初步结论。\
                     如果 search_text 报错（error 字段），说明原因并给出建议。"
                ),
            },
        ];
        Ok(steps)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// AnnotateFile
// ─────────────────────────────────────────────────────────────────────────────

/// 标注当前打开的文件（**v1 全程不调 LLM**）：
///
/// 1. `open_document(path)` 拿 document_id
/// 2. `read_context(document_id, line=0, before=0, after=200)` 读前 200 行
/// 3. `Parallel { create_annotation × N }` 沿预定义模式（"TODO"/"FIXME"/"XXX"/"HACK"）
///    在文件前 200 行内挑最多 5 个批注（heuristic）
///
/// v2 可把"挑疑点"换成 LlmDecision。
pub struct AnnotateFileFlow;

impl Flow for AnnotateFileFlow {
    fn id(&self) -> FlowId {
        FlowId::AnnotateFile
    }

    fn plan(&self, intent: &Intent, ctx: &FlowContext) -> anyhow::Result<Vec<Step>> {
        let path = intent
            .params
            .get("path")
            .or_else(|| ctx.current_file.as_ref())
            .ok_or_else(|| anyhow::anyhow!("AnnotateFileFlow 缺文件路径"))?
            .clone();

        Ok(vec![
            Step::Work(work(
                "open_document",
                "open_document",
                serde_json::json!({ "path": path }),
            )),
            // document_id 来自上一个 Work（open_document）→ 具名插值
            Step::Work(work(
                "read_context",
                "read_context",
                serde_json::json!({
                    "document_id": "{{open_document.document_id}}",
                    "line": 0,
                    "before": 0,
                    "after": 200
                }),
            )),
            Step::LlmDecision {
                prompt: format!(
                    "用户要求给文件 {path} 打批注。下面是 read_context 取到的文件开头内容。\
                     请挑出 3-8 个值得打批注的疑点（TODO / 潜在 bug / 可疑逻辑 / 待确认），用列表逐条汇报。\
                     如果 read_context 报错（error 字段），说明原因并给建议。"
                ),
            },
        ])
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ExportCurrentReport
// ─────────────────────────────────────────────────────────────────────────────

/// 出当前文件报告（**v1 全程不调 LLM**）：
///
/// 1. `read_context(document_id, line=0, before=0, after=500)` 读前 500 行
/// 2. `export_report(document_id, content=模板)` 写出 markdown 头 + 前 500 行截断
///
/// v2 可在 read_context 后插一个 LlmDecision 让 LLM 整理结构化报告。
pub struct ExportCurrentReportFlow;

impl Flow for ExportCurrentReportFlow {
    fn id(&self) -> FlowId {
        FlowId::ExportCurrentReport
    }

    fn plan(&self, intent: &Intent, ctx: &FlowContext) -> anyhow::Result<Vec<Step>> {
        let path = intent
            .params
            .get("path")
            .or_else(|| ctx.current_file.as_ref())
            .ok_or_else(|| anyhow::anyhow!("ExportCurrentReportFlow 缺文件路径"))?
            .clone();
        Ok(vec![
            Step::Work(work(
                "open_document",
                "open_document",
                serde_json::json!({ "path": path }),
            )),
            Step::Work(work(
                "read_context",
                "read_context",
                serde_json::json!({
                    "document_id": "{{open_document.document_id}}",
                    "line": 0,
                    "before": 0,
                    "after": 500
                }),
            )),
            Step::LlmDecision {
                prompt: format!(
                    "用户要求给文件 {path} 出一份报告。下面是 read_context 取到的文件开头内容。\
                     请整理成结构化报告返回：文件概况、关键内容 / 要点、发现的问题或风险、建议。\
                     如果 read_context 报错（error 字段），说明原因并给建议。"
                ),
            },
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::runner;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn intent_with_path(kind: crate::intent::IntentKind, path: &str) -> Intent {
        Intent {
            kind,
            confidence: 0.9,
            params: HashMap::from([("path".into(), path.into())]),
            suggested_tools: vec![],
            suggested_flow: None,
            reply: None,
        }
    }

    fn dummy_ctx() -> FlowContext {
        use crate::flow::FlowDocs;
        struct Dummy;
        #[async_trait::async_trait]
        impl FlowDocs for Dummy {
            async fn tool_call(
                &self,
                tool: &str,
                _args: serde_json::Value,
            ) -> anyhow::Result<serde_json::Value> {
                match tool {
                    "open_document" => Ok(serde_json::json!({
                        "document_id": 99,
                        "view_intents": [{
                            "intent": "open_document",
                            "path": "/tmp/a.txt"
                        }]
                    })),
                    "list_directory" => Ok(serde_json::json!({ "entries": ["a", "b"] })),
                    "search_text" => Ok(serde_json::json!({ "total": 3, "hits": [] })),
                    "read_context" => Ok(serde_json::json!({ "content": "log line 1\nlog line 2" })),
                    _ => Ok(serde_json::json!({})),
                }
            }
        }
        FlowContext {
            docs: Arc::new(Dummy),
            current_file: None,
            user_query: None,
            sinks: None,
            session_id: None,
        }
    }

    /// 收集事件的 sink（验证 Flow 广播 ToolCallStarted/Finished/ViewIntentEmitted）。
    #[derive(Default, Debug)]
    struct CollectingSink {
        events: std::sync::Mutex<Vec<crate::event::AgentEvent>>,
    }
    impl crate::event::AgentSink for CollectingSink {
        fn on_event(&self, e: crate::event::AgentEvent) {
            self.events.lock().unwrap().push(e);
        }
    }

    /// **回归测试**：Flow 的 Work 必须广播 ToolCallStarted/Finished + ViewIntentEmitted——
    /// 否则 GUI 工具列表空、打开文件后主视图不切换（用户实际反馈的 bug）。
    #[tokio::test]
    async fn flow_broadcasts_tool_events_and_view_intents() {
        use crate::sink_hook::WeakSinks;
        use crate::event::AgentEvent;

        // 构造带 sinks 的 ctx（复刻 runtime 传参）
        let concrete = Arc::new(CollectingSink::default());
        let sink: Arc<dyn crate::event::AgentSink> = concrete.clone();
        let weak_sinks = WeakSinks::new();
        weak_sinks.push(Arc::downgrade(&sink));

        struct RealDocs;
        #[async_trait::async_trait]
        impl crate::flow::FlowDocs for RealDocs {
            async fn tool_call(
                &self,
                _tool: &str,
                _args: serde_json::Value,
            ) -> anyhow::Result<serde_json::Value> {
                Ok(serde_json::json!({
                    "document_id": 7,
                    "view_intents": [{"intent": "open_document", "path": "/tmp/a.txt"}]
                }))
            }
        }
        let ctx = FlowContext {
            docs: Arc::new(RealDocs),
            current_file: None,
            user_query: Some("打开文件".into()),
            sinks: Some(weak_sinks),
            session_id: Some("sess-1".into()),
        };

        // OpenFileFlow: Work(open_document) → LlmDecision
        let flow = OpenFileFlow;
        let intent = intent_with_path(crate::intent::IntentKind::OpenFile, "/tmp/a.txt");
        let steps = flow.plan(&intent, &ctx).unwrap();
        let _ = runner::run(&ctx, steps, opts_with_mock_llm()).await.unwrap();

        let events = concrete.events.lock().unwrap();
        let has_started = events.iter().any(|e| matches!(e, AgentEvent::ToolCallStarted { tool, .. } if tool == "open_document"));
        let has_finished = events.iter().any(|e| matches!(e, AgentEvent::ToolCallFinished { tool, .. } if tool == "open_document"));
        let has_view = events.iter().any(|e| matches!(e, AgentEvent::ViewIntentEmitted { .. }));
        assert!(has_started, "必须广播 ToolCallStarted；实际 {events:?}");
        assert!(has_finished, "必须广播 ToolCallFinished；实际 {events:?}");
        assert!(has_view, "必须广播 ViewIntentEmitted（打开文件让 GUI 切主视图）；实际 {events:?}");
    }

    #[test]
    fn open_file_plan_has_one_work() {
        let flow = OpenFileFlow;
        let intent = intent_with_path(crate::intent::IntentKind::OpenFile, "/tmp/a.txt");
        let steps = flow.plan(&intent, &dummy_ctx()).unwrap();
        assert_eq!(steps.len(), 2);
        match &steps[0] {
            Step::Work(w) => {
                assert_eq!(w.name, "open_document");
                assert!(matches!(w.kind, WorkKind::ToolCall { .. }));
            }
            _ => panic!("expected Work"),
        }
        match &steps[1] {
            Step::LlmDecision { prompt } => assert!(prompt.contains("/tmp/a.txt")),
            _ => panic!("expected LlmDecision"),
        }
    }

    #[test]
    fn open_file_missing_path_errors() {
        let flow = OpenFileFlow;
        let intent = Intent::unknown();
        let err = flow.plan(&intent, &dummy_ctx()).unwrap_err();
        assert!(err.to_string().contains("path"));
    }

    /// 目标文件 == 当前已打开文件 → 跳过 open_document Work（不重复打开）。
    #[test]
    fn open_file_skips_reopen_when_target_is_current() {
        let flow = OpenFileFlow;
        let intent = intent_with_path(crate::intent::IntentKind::OpenFile, r"\\?\D:\logs\a.txt");
        // current_file 用普通形式（GUI 注入的 canonical path），应归一化后视为同一文件
        let ctx = FlowContext {
            docs: dummy_ctx().docs,
            current_file: Some(r"D:\logs\a.txt".into()),
            user_query: Some("打开 a.txt".into()),
            sinks: None,
            session_id: None,
        };
        let steps = flow.plan(&intent, &ctx).unwrap();
        assert_eq!(steps.len(), 1, "已打开时只应剩 LlmDecision，不应有 open_document Work");
        match &steps[0] {
            Step::LlmDecision { prompt } => {
                assert!(prompt.contains("无需重复打开"));
            }
            _ => panic!("expected LlmDecision"),
        }
    }

    /// 目标文件 ≠ 当前文件 → 保留 open_document Work。
    #[test]
    fn open_file_keeps_work_when_target_differs() {
        let flow = OpenFileFlow;
        let intent = intent_with_path(crate::intent::IntentKind::OpenFile, r"D:\logs\b.txt");
        let ctx = FlowContext {
            docs: dummy_ctx().docs,
            current_file: Some(r"D:\logs\a.txt".into()),
            user_query: Some("打开 b.txt".into()),
            sinks: None,
            session_id: None,
        };
        let steps = flow.plan(&intent, &ctx).unwrap();
        assert_eq!(steps.len(), 2);
        match &steps[0] {
            Step::Work(w) => assert_eq!(w.name, "open_document"),
            _ => panic!("expected Work"),
        }
    }

    #[test]
    fn list_dir_plan_has_one_work() {
        let flow = ListDirFlow;
        let intent = intent_with_path(crate::intent::IntentKind::ListDir, "/tmp");
        let steps = flow.plan(&intent, &dummy_ctx()).unwrap();
        assert_eq!(steps.len(), 2);
        match &steps[0] {
            Step::Work(w) => assert_eq!(w.name, "list_directory"),
            _ => panic!(),
        }
        match &steps[1] {
            Step::LlmDecision { .. } => {}
            _ => panic!("expected LlmDecision"),
        }
    }

    /// 构造带 mock LLM 总结器的 RunOptions：总结器把 prompt 原样返回
    /// （prompt 里含 path / 关键词，所以 `contains` 断言仍有效）。
    fn opts_with_mock_llm() -> runner::RunOptions {
        let mut opts = runner::RunOptions::new();
        opts.llm_summarize = Some(Box::new(|prompt, _results| {
            let p = prompt.to_string();
            Box::pin(async move { Ok(p) })
        }));
        opts
    }

    /// e2e：完整 OpenFileFlow 跑通 runner（不依赖 ReAct，只用 mock LLM）。
    #[tokio::test]
    async fn open_file_flow_runs_end_to_end() {
        let flow = OpenFileFlow;
        let intent = intent_with_path(crate::intent::IntentKind::OpenFile, "/tmp/e2e.txt");
        let ctx = dummy_ctx();
        let steps = flow.plan(&intent, &ctx).unwrap();
        let report = runner::run(&ctx, steps, opts_with_mock_llm()).await.expect("flow run ok");
        assert!(report.final_summary.contains("/tmp/e2e.txt"));
        assert!(report.results.contains_key("open_document"));
        assert_eq!(report.results["open_document"]["document_id"], 99);
    }

    /// e2e：ListDirFlow 跑通 runner。
    #[tokio::test]
    async fn list_dir_flow_runs_end_to_end() {
        let flow = ListDirFlow;
        let intent = intent_with_path(crate::intent::IntentKind::ListDir, "/tmp");
        let ctx = dummy_ctx();
        let steps = flow.plan(&intent, &ctx).unwrap();
        let report = runner::run(&ctx, steps, opts_with_mock_llm()).await.expect("flow run ok");
        assert!(report.results.contains_key("list_directory"));
        assert!(report.final_summary.contains("/tmp"));
    }

    /// AnnotateFileFlow 缺路径应报错。
    #[test]
    fn annotate_missing_path_errors() {
        let flow = AnnotateFileFlow;
        let intent = Intent::unknown();
        assert!(flow.plan(&intent, &dummy_ctx()).is_err());
    }

    /// ExportCurrentReportFlow 缺路径应报错。
    #[test]
    fn export_missing_path_errors() {
        let flow = ExportCurrentReportFlow;
        let intent = Intent::unknown();
        assert!(flow.plan(&intent, &dummy_ctx()).is_err());
    }

    /// SearchLogAndReportFlow 完整跑通：open_document → search_text → LlmDecision
    #[tokio::test]
    async fn search_log_flow_runs_end_to_end() {
        let flow = SearchLogAndReportFlow;
        let mut intent = intent_with_path(crate::intent::IntentKind::SearchLog, "/tmp/prod.log");
        intent.params.insert(
            "query".into(),
            "查 ERROR".into(),
        );
        intent.params.insert("document_id".into(), "42".into());
        let ctx = dummy_ctx();
        let steps = flow.plan(&intent, &ctx).unwrap();
        let report = runner::run(&ctx, steps, opts_with_mock_llm()).await.expect("flow run ok");
        assert!(report.results.contains_key("open_document"));
        assert!(report.results.contains_key("search_text"));
        assert!(report.final_summary.contains("ERROR"));
    }

    /// AnnotateFileFlow 完整跑通：open_document → read_context → LlmDecision
    #[tokio::test]
    async fn annotate_flow_runs_end_to_end() {
        let flow = AnnotateFileFlow;
        let mut intent = intent_with_path(crate::intent::IntentKind::AnnotateFile, "/tmp/start.txt");
        intent.params.insert("document_id".into(), "5".into());
        let ctx = dummy_ctx();
        let steps = flow.plan(&intent, &ctx).unwrap();
        let report = runner::run(&ctx, steps, opts_with_mock_llm()).await.expect("flow run ok");
        assert!(report.results.contains_key("open_document"));
        assert!(report.results.contains_key("read_context"));
        assert!(report.final_summary.contains("start.txt"));
    }

    /// ExportCurrentReportFlow 完整跑通：open_document → read_context → LlmDecision
    #[tokio::test]
    async fn export_flow_runs_end_to_end() {
        let flow = ExportCurrentReportFlow;
        let mut intent = intent_with_path(crate::intent::IntentKind::ExportReport, "/tmp/start.txt");
        intent.params.insert("document_id".into(), "5".into());
        let ctx = dummy_ctx();
        let steps = flow.plan(&intent, &ctx).unwrap();
        let report = runner::run(&ctx, steps, opts_with_mock_llm()).await.expect("flow run ok");
        assert!(report.results.contains_key("read_context"));
        assert!(report.final_summary.contains("start.txt"));
    }

}