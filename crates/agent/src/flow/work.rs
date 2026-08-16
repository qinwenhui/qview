//! Work 单元（架构 §22.x — P2「Work」落地）。
//!
//! ## 定位
//!
//! Flow 内部最小执行单元：一次工具调用（`ToolCall`）或一次 LLM 推理（`LlmCall`，v1 stub）。
//! 每个 Work：
//!
//! - **独立超时**（`timeout_ms`）：不阻塞 Flow 主流程
//! - **独立重试**（`RetryPolicy`）：瞬时故障自动恢复
//! - **可命名**（`name`）：Flow 之间可互相引用前一个 Work 的结果
//!
//! ## 与 ReAct 的差异
//!
//! - Work **不是** ReAct 循环里的"tool_call"——它没有 LLM 决策，只是单纯的执行单元。
//! - Work 失败时按 RetryPolicy 重试；超过则返回错误 JSON（FlowRunner 不停止）。
//! - Work 完成后 FlowRunner 拿到结果，按 Step 序列推进下一个 Step。
//!
//! ## v1 范围
//!
//! - `WorkKind::ToolCall`：直接调 `FlowDocs::tool_call`，复用 ToolRegistry 的工具
//! - `WorkKind::LlmCall`：v1 stub（实际应走 ReAct；FlowRunner 遇到 bail）
//!
//! 不实现：
//! - 异步 Work / 跨 Flow 引用
//! - Work 状态持久化（断点续跑在 P2 后续）

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::flow::FlowContext;

/// Work 标识（在 Flow 内唯一；runner 用 name 取前序 Work 的结果）。
pub type WorkName = String;

/// Work 描述。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkSpec {
    pub name: WorkName,
    pub kind: WorkKind,
    /// 单次执行超时（毫秒）。默认 30s。
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    /// 重试策略。默认 None。
    #[serde(default)]
    pub retry: RetryPolicy,
}

fn default_timeout() -> u64 {
    30_000
}

/// Work 类型。
///
/// **工具调用走 ToolRegistry**：tool 字段是注册名（如 `open_document` / `search_text`），
/// args 是 JSON 参数。和 ReAct 用同一份工具定义，零重复。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkKind {
    /// 直接调用底层 ToolRegistry 的某个工具（不调 LLM）。
    ToolCall {
        tool: String,
        args: serde_json::Value,
    },
    /// 调 LLM 一次。`system` / `user` 都是 prompt 模板（v1 不做模板渲染，原样发）。
    /// v1 stub：FlowRunner 遇到此 WorkKind 直接 bail（实际应在 Step::LlmDecision 阶段处理）。
    LlmCall {
        system: String,
        user: String,
    },
}

impl WorkKind {
    /// 广播 ToolCallStarted 用的入参摘要（JSON）。
    fn to_input_json(&self) -> serde_json::Value {
        match self {
            WorkKind::ToolCall { args, .. } => args.clone(),
            WorkKind::LlmCall { system, user } => serde_json::json!({
                "system": system.chars().take(200).collect::<String>(),
                "user": user.chars().take(200).collect::<String>(),
            }),
        }
    }
}

/// 重试策略。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum RetryPolicy {
    /// 不重试（默认）。
    #[default]
    None,
    /// 指数退避。
    Exponential {
        max_retries: u32,
        base_ms: u64,
    },
}

/// Work 执行结果。
#[derive(Debug, Clone)]
pub struct WorkResult {
    pub name: WorkName,
    pub value: serde_json::Value,
    /// 执行耗时（毫秒），含重试时间。
    pub duration_ms: u64,
    /// 重试次数（0 = 一次就过）。
    pub retries: u32,
}

/// Work 执行器。
pub struct WorkExecutor<'a> {
    pub ctx: &'a FlowContext,
}

impl<'a> WorkExecutor<'a> {
    pub fn new(ctx: &'a FlowContext) -> Self {
        Self { ctx }
    }

    /// 执行一个 WorkSpec，按 RetryPolicy 处理瞬时失败。
    ///
    /// **事件广播**：Work 开始/结束发 `ToolCallStarted/Finished` + `ViewIntentEmitted`——
    /// 让 GUI 工具列表有数据、view_intent 应用到主视图、工具调用落库
    /// （否则 Flow 直接调 ToolRegistry 绕过 QviewSinkHook，用户看不到工具调用、
    /// 打开文件后主视图不切换）。
    pub async fn run(&self, spec: WorkSpec) -> WorkResult {
        let started = std::time::Instant::now();
        let mut retries: u32 = 0;
        let timeout = Duration::from_millis(spec.timeout_ms);

        // 广播 ToolCallStarted（GUI 工具列表 + 落库用）
        let call_id = qview_application::protocol::ToolCallId::new();
        self.emit(crate::event::AgentEvent::ToolCallStarted {
            session_id: self.ctx.session_id.clone().unwrap_or_default(),
            call_id,
            tool: spec.name.clone(),
            input: spec.kind.to_input_json(),
        });

        let result = loop {
            let attempt = tokio::time::timeout(timeout, self.run_once(&spec)).await;
            match attempt {
                Ok(Ok(value)) => {
                    break WorkResult {
                        name: spec.name.clone(),
                        value,
                        duration_ms: started.elapsed().as_millis() as u64,
                        retries,
                    };
                }
                Ok(Err(e)) => {
                    tracing::warn!(
                        target: "qview_agent::flow",
                        work = %spec.name,
                        attempt = retries + 1,
                        "work 执行失败：{e}"
                    );
                    if !self.should_retry(&spec.retry, retries) {
                        break WorkResult {
                            name: spec.name.clone(),
                            value: serde_json::json!({
                                "error": format!("work failed after {} attempts: {}", retries + 1, e),
                            }),
                            duration_ms: started.elapsed().as_millis() as u64,
                            retries,
                        };
                    }
                    retries += 1;
                    self.backoff(&spec.retry, retries).await;
                }
                Err(_) => {
                    tracing::warn!(
                        target: "qview_agent::flow",
                        work = %spec.name,
                        attempt = retries + 1,
                        "work 执行超时（{} ms）",
                        spec.timeout_ms
                    );
                    if !self.should_retry(&spec.retry, retries) {
                        break WorkResult {
                            name: spec.name.clone(),
                            value: serde_json::json!({
                                "error": format!("work timeout after {} attempts", retries + 1),
                            }),
                            duration_ms: started.elapsed().as_millis() as u64,
                            retries,
                        };
                    }
                    retries += 1;
                    self.backoff(&spec.retry, retries).await;
                }
            }
        };

        // 广播 ToolCallFinished + 提取 view_intents 广播 ViewIntentEmitted
        self.emit_finished(&call_id, &result);
        self.emit_view_intents(&result.value);

        result
    }

    /// 广播一个 AgentEvent（若 FlowContext 配了 sinks）。
    fn emit(&self, event: crate::event::AgentEvent) {
        if let Some(sinks) = &self.ctx.sinks {
            sinks.broadcast(event);
        }
    }

    /// 广播 ToolCallFinished（结果摘要截断 + 是否出错）。
    fn emit_finished(&self, call_id: &qview_application::protocol::ToolCallId, result: &WorkResult) {
        let is_error = result.value.get("error").is_some();
        let summary = if is_error {
            result.value.to_string()
        } else {
            serde_json::to_string(&result.value).unwrap_or_default()
        };
        self.emit(crate::event::AgentEvent::ToolCallFinished {
            session_id: self.ctx.session_id.clone().unwrap_or_default(),
            call_id: *call_id,
            tool: result.name.clone(),
            output_summary: summary.chars().take(500).collect(),
            duration_ms: result.duration_ms,
            is_error,
        });
    }

    /// 从 Work 结果里提取 `view_intents` 并广播（让 GUI 应用打开文件/跳转/高亮等）。
    fn emit_view_intents(&self, value: &serde_json::Value) {
        let Some(arr) = value.get("view_intents").and_then(|v| v.as_array()) else {
            return;
        };
        for intent in arr {
            if let Ok(parsed) = serde_json::from_value::<qview_application::protocol::ViewIntent>(
                intent.clone(),
            ) {
                self.emit(crate::event::AgentEvent::ViewIntentEmitted {
                    session_id: self.ctx.session_id.clone().unwrap_or_default(),
                    intent: parsed,
                });
            }
        }
    }

    fn should_retry(&self, policy: &RetryPolicy, already_retried: u32) -> bool {
        match policy {
            RetryPolicy::None => false,
            RetryPolicy::Exponential { max_retries, .. } => already_retried < *max_retries,
        }
    }

    async fn backoff(&self, policy: &RetryPolicy, retry_idx: u32) {
        if let RetryPolicy::Exponential { base_ms, .. } = policy {
            let delay_ms = base_ms * 2u64.pow(retry_idx.min(10));
            tokio::time::sleep(Duration::from_millis(delay_ms.min(30_000))).await;
        }
    }

    async fn run_once(&self, spec: &WorkSpec) -> anyhow::Result<serde_json::Value> {
        match &spec.kind {
            WorkKind::ToolCall { tool, args } => {
                let docs: &dyn crate::flow::FlowDocs = self.ctx.docs.as_ref();
                docs.tool_call(tool, args.clone()).await
            }
            WorkKind::LlmCall { system: _, user: _ } => {
                // v1 不实现：LLM 调用需要接入 ReActWorker.chat()，超出 Work 单元边界。
                // 实际场景：Flow 里需要 LLM 决策的地方（如 AnnotateFile 挑疑点）
                // 应该作为 LlmDecision Step 走 ReAct，而不是 Work::LlmCall。
                anyhow::bail!("WorkKind::LlmCall 在 v1 不实现；请用 Step::LlmDecision")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{FlowContext, FlowDocs};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// Fake FlowDocs：记录所有调用，返回固定结果。
    #[derive(Default)]
    struct FakeDocs {
        captured: Mutex<HashMap<String, serde_json::Value>>,
    }

    #[async_trait::async_trait]
    impl FlowDocs for FakeDocs {
        async fn tool_call(
            &self,
            tool: &str,
            args: serde_json::Value,
        ) -> anyhow::Result<serde_json::Value> {
            self.captured
                .lock()
                .unwrap()
                .insert(tool.to_string(), args);
            // open_document 返 document_id=42；list_directory 返两个名字；其他返成功
            match tool {
                "open_document" => Ok(serde_json::json!({ "document_id": 42 })),
                "list_directory" => Ok(serde_json::json!({ "entries": ["a.txt", "b.log"] })),
                "read_context" => Ok(serde_json::json!({ "content": "line0\nline1\n" })),
                "search_text" => Ok(serde_json::json!({
                    "total": 3,
                    "hits": [{"line": 10, "text": "ERROR a"}, {"line": 20, "text": "ERROR b"}]
                })),
                "create_annotation" => Ok(serde_json::json!({ "annotation_id": 7 })),
                "export_report" => Ok(serde_json::json!({ "report_path": "/tmp/r.md" })),
                _ => Ok(serde_json::json!({"ok": true})),
            }
        }
    }

    fn work(name: &str, tool: &str, args: serde_json::Value) -> WorkSpec {
        WorkSpec {
            name: name.into(),
            kind: WorkKind::ToolCall {
                tool: tool.into(),
                args,
            },
            timeout_ms: 5_000,
            retry: RetryPolicy::None,
        }
    }

    #[tokio::test]
    async fn work_open_document_routes_args() {
        let docs = Arc::new(FakeDocs::default());
        let ctx = FlowContext {
            docs,
            current_file: None,
            user_query: None,
            sinks: None,
            session_id: None,
        };
        let executor = WorkExecutor::new(&ctx);
        let spec = work("w1", "open_document", serde_json::json!({ "path": "/tmp/foo.txt" }));
        let result = executor.run(spec).await;
        assert_eq!(result.value["document_id"], 42);
        assert_eq!(
            ctx.docs
                .as_ref()
                .tool_call("open_document", serde_json::json!({"path":"/tmp/foo.txt"}))
                .await
                .unwrap()["document_id"],
            42
        );
    }

    #[tokio::test]
    async fn work_list_directory_returns_entries() {
        let docs = Arc::new(FakeDocs::default());
        let ctx = FlowContext {
            docs,
            current_file: None,
            user_query: None,
            sinks: None,
            session_id: None,
        };
        let executor = WorkExecutor::new(&ctx);
        let spec = work("ls", "list_directory", serde_json::json!({ "path": "/tmp" }));
        let result = executor.run(spec).await;
        let entries = result.value["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], "a.txt");
    }

    #[tokio::test]
    async fn work_search_text_returns_hits() {
        let docs = Arc::new(FakeDocs::default());
        let ctx = FlowContext {
            docs,
            current_file: None,
            user_query: None,
            sinks: None,
            session_id: None,
        };
        let executor = WorkExecutor::new(&ctx);
        let spec = work(
            "search",
            "search_text",
            serde_json::json!({
                "document_id": 1, "query": "ERROR", "limit": 50
            }),
        );
        let result = executor.run(spec).await;
        assert_eq!(result.value["total"], 3);
        let hits = result.value["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 2);
    }

    /// 工具不存在 → anyhow::bail → WorkResult.value 是 `{"error": ...}`。
    #[tokio::test]
    async fn work_unknown_tool_returns_error_value() {
        struct FailDocs;
        #[async_trait::async_trait]
        impl FlowDocs for FailDocs {
            async fn tool_call(&self, _: &str, _: serde_json::Value) -> anyhow::Result<serde_json::Value> {
                anyhow::bail!("tool not registered")
            }
        }
        let ctx = FlowContext {
            docs: Arc::new(FailDocs),
            current_file: None,
            user_query: None,
            sinks: None,
            session_id: None,
        };
        let executor = WorkExecutor::new(&ctx);
        let spec = work("bad", "no_such_tool", serde_json::json!({}));
        let result = executor.run(spec).await;
        assert!(result.value.get("error").is_some());
    }
}