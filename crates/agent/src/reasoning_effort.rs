//! `ReasoningEffortClient`：在每次 `chat()` 时给 `ChatRequest.extra` 注入
//! DeepSeek 思考模式开关 + 强度的 LLMClient 装饰器。
//!
//! ## 设计动机
//!
//! Provider 的实现是按 `Arc<dyn LLMClient>` 暴露的，构造期只跑一次；
//! 而 `reasoning_effort` 是 `ProviderConfig` 的字段，UI 改完会触发 runtime 重建
//! （`QLogApp::rebuild_agent_runtime`），所以在 `build_client` 内部构造包装器
//! 就能让"改 UI → 重建 runtime → 新包装器生效"自然联动。
//!
//! ## 为什么不在 `contexa-rs` 框架里加 `worker.default_extra` 字段
//!
//! 那是更"正确"的设计，但 `ReActExecutor` 构造 `ChatRequest` 的代码
//! (`react_executor.rs:276`) 不读 worker 上的任何额外字段；引入会改动框架核心
//! 执行路径、影响所有 qview 之外的调用方。装饰器方案把"qview 自己的偏好"留在
//! qview 这一侧，框架契约不变。
//!
//! ## DeepSeek 思考控制的两层
//!
//! 按 DeepSeek 官方文档，OpenAI 协议下需要发两个独立字段：
//!
//! - **`thinking.type`** = `"enabled"` / `"disabled"`（开关，默认 enabled）
//! - **`reasoning_effort`** = `"low"` / `"high"` / `"xhigh"` / `"max"`
//!   （强度；DeepSeek v4-flash 按映射表转：`low→low`、`high→high`、
//!   `xhigh→high`、`max→max`）
//!
//! `ChatRequest::with_deepseek_thinking(level)` 一次性配齐两层：
//!
//! - `level == "none"` → 仅 `thinking.type="disabled"`（reasoning_effort 被忽略）
//! - 其他 → `thinking.type="enabled"` + `reasoning_effort=<level>`

use std::sync::Arc;

use contexa_context::Result;
use contexa_llm::{ChatRequest, LLMClient, LLMResponse};

/// 注入 LLM 调用偏好的装饰器：DeepSeek 思考开关 + 强度 + `max_tokens` 上限。
///
/// 设计要点：
/// - **`effort == None` 时透传**：调用方完全控制 `thinking`/`reasoning_effort`。
/// - **`'static` key 协变到 `'a`**：`"thinking"` / `"reasoning_effort"` 是
///   `&'static str`，可作为 `&'a str` 插入新 `ChatRequest.extra`；不分配临时字符串。
/// - **尊重调用方显式设置**：请求里已显式带 `thinking` 字段时，装饰器**不覆盖**——
///   意图分类等轻任务（想 `thinking.type=disabled` 快速出结构化 JSON）能按自己的
///   需求走，不受全局 `reasoning_effort` 影响；只有调用方没显式控制时，全局设置
///   才是权威来源。
/// - **max_tokens 兜底**：`ReActExecutor` / Flow 总结构造 `ChatRequest` 都不设
///   `max_tokens`（框架不管），装饰器在请求没显式设时注入 provider 配置的值。
///   这是"钳制 LLM 单次输出量"的关键——不带上限时模型一次能吐 34K tokens。
pub struct ReasoningEffortClient {
    inner: Arc<dyn LLMClient>,
    /// 取值语义：
    /// - `None` → 不动 ChatRequest（业务方自己管 thinking）
    /// - `Some("none")` → 发 `thinking.type=disabled`、不发明 reasoning_effort
    /// - `Some("low"|"high"|"xhigh"|"max"|"medium")` → 发 `thinking.type=enabled`
    ///   + `reasoning_effort=<level>`
    effort: Option<String>,
    /// 请求没显式设 `max_tokens` 时注入的值（钳制单次输出）。
    max_tokens: Option<u32>,
}

impl ReasoningEffortClient {
    pub fn new(inner: Arc<dyn LLMClient>, effort: Option<String>, max_tokens: Option<u32>) -> Self {
        Self {
            inner,
            effort,
            max_tokens,
        }
    }
}

#[async_trait::async_trait]
impl LLMClient for ReasoningEffortClient {
    async fn chat(&self, mut req: ChatRequest<'_>) -> Result<LLMResponse> {
        // max_tokens 兜底：请求没显式设才注入。
        if req.max_tokens.is_none() {
            req.max_tokens = self.max_tokens;
        }
        if let Some(level) = &self.effort {
            // 调用方已显式设 thinking（意图分类想 thinking disabled 快速出 JSON）→
            // 尊重调用方，不覆盖、也不发明 reasoning_effort。
            if req.extra.contains_key("thinking") {
                // 若调用方还带了 reasoning_effort 就保留；否则只保留它自己的 thinking。
                return self.inner.chat(req).await;
            }
            // 'static 字面量 → 'a（ChatRequest 的借用生命周期）。
            if level == "none" {
                let key: &str = "thinking";
                req.extra.insert(key, serde_json::json!({"type": "disabled"}));
            } else {
                let key: &str = "thinking";
                req.extra.insert(key, serde_json::json!({"type": "enabled"}));
                let key: &str = "reasoning_effort";
                req.extra
                    .insert(key, serde_json::Value::String(level.clone()));
            }
        }
        self.inner.chat(req).await
    }

    async fn warm_up(&self) -> Result<()> {
        self.inner.warm_up().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// 抓拍 `chat()` 收到的 `extra` 副本（把 key 拥有化便于断言）。
    struct Capture {
        captured: Mutex<Option<HashMap<String, serde_json::Value>>>,
    }

    #[async_trait::async_trait]
    impl LLMClient for Capture {
        async fn chat(&self, req: ChatRequest<'_>) -> Result<LLMResponse> {
            let mut map = HashMap::new();
            for (k, v) in req.extra.iter() {
                map.insert((*k).to_string(), v.clone());
            }
            *self.captured.lock().unwrap() = Some(map);
            Ok(LLMResponse::new("ok"))
        }
    }

    /// `level = "low"` → 同时注入 `thinking.type=enabled` + `reasoning_effort=low`。
    /// 这是 DeepSeek 推荐的"开关+强度"组合。
    #[tokio::test]
    async fn decorator_injects_thinking_enabled_and_reasoning_effort() {
        let cap = Arc::new(Capture {
            captured: Mutex::new(None),
        });
        let wrapped = ReasoningEffortClient::new(
            cap.clone() as Arc<dyn LLMClient>,
            Some("low".into()),
            None,
        );

        let msg = contexa_context::Message::user("ping");
        let req = ChatRequest::new(std::slice::from_ref(&msg), &[]);
        wrapped.chat(req).await.unwrap();

        let extra = cap.captured.lock().unwrap().clone().unwrap();
        assert_eq!(
            extra.get("thinking").and_then(|v| v.get("type")).and_then(|v| v.as_str()),
            Some("enabled"),
            "thinking 开关必须是 enabled（level!=none）"
        );
        assert_eq!(
            extra.get("reasoning_effort").and_then(|v| v.as_str()),
            Some("low")
        );
    }

    /// `level = "none"` → 仅发 `thinking.type=disabled`，不发明 `reasoning_effort`
    /// （按 DeepSeek 文档，关闭思考后强度字段被服务端忽略，所以不要污染 wire）。
    #[tokio::test]
    async fn decorator_injects_thinking_disabled_when_level_is_none() {
        let cap = Arc::new(Capture {
            captured: Mutex::new(None),
        });
        let wrapped = ReasoningEffortClient::new(
            cap.clone() as Arc<dyn LLMClient>,
            Some("none".into()),
            None,
        );

        let msg = contexa_context::Message::user("ping");
        let req = ChatRequest::new(std::slice::from_ref(&msg), &[]);
        wrapped.chat(req).await.unwrap();

        let extra = cap.captured.lock().unwrap().clone().unwrap();
        assert_eq!(
            extra.get("thinking").and_then(|v| v.get("type")).and_then(|v| v.as_str()),
            Some("disabled")
        );
        assert!(
            extra.get("reasoning_effort").is_none(),
            "level=none 时不应发明 reasoning_effort 字段（服务端会忽略，但保持 wire 干净）"
        );
    }

    /// 请求没显式设 max_tokens → 装饰器注入 provider 配置的值（钳制单次输出）。
    #[tokio::test]
    async fn decorator_injects_max_tokens_when_absent() {
        struct CaptureMaxTokens {
            captured: Mutex<Option<u32>>,
        }
        #[async_trait::async_trait]
        impl LLMClient for CaptureMaxTokens {
            async fn chat(&self, req: ChatRequest<'_>) -> Result<LLMResponse> {
                *self.captured.lock().unwrap() = req.max_tokens;
                Ok(LLMResponse::new("ok"))
            }
        }
        let cap = Arc::new(CaptureMaxTokens {
            captured: Mutex::new(None),
        });
        let wrapped = ReasoningEffortClient::new(
            cap.clone() as Arc<dyn LLMClient>,
            Some("low".into()),
            Some(4000),
        );

        let msg = contexa_context::Message::user("ping");
        let req = ChatRequest::new(std::slice::from_ref(&msg), &[]);
        wrapped.chat(req).await.unwrap();
        assert_eq!(*cap.captured.lock().unwrap(), Some(4000));
    }

    /// 请求已显式设 max_tokens → 装饰器不覆盖（调用方权威）。
    #[tokio::test]
    async fn decorator_keeps_explicit_max_tokens() {
        struct CaptureMaxTokens {
            captured: Mutex<Option<u32>>,
        }
        #[async_trait::async_trait]
        impl LLMClient for CaptureMaxTokens {
            async fn chat(&self, req: ChatRequest<'_>) -> Result<LLMResponse> {
                *self.captured.lock().unwrap() = req.max_tokens;
                Ok(LLMResponse::new("ok"))
            }
        }
        let cap = Arc::new(CaptureMaxTokens {
            captured: Mutex::new(None),
        });
        let wrapped = ReasoningEffortClient::new(
            cap.clone() as Arc<dyn LLMClient>,
            None,
            Some(4000),
        );

        let msg = contexa_context::Message::user("ping");
        let req = ChatRequest::new(std::slice::from_ref(&msg), &[]).with_max_tokens(999);
        wrapped.chat(req).await.unwrap();
        assert_eq!(*cap.captured.lock().unwrap(), Some(999), "显式值优先");
    }

    /// `effort = None` → 完全不动 ChatRequest（业务方自己管 thinking）。
    #[tokio::test]
    async fn decorator_passes_through_when_no_effort() {
        let cap = Arc::new(Capture {
            captured: Mutex::new(None),
        });
        let wrapped = ReasoningEffortClient::new(cap.clone() as Arc<dyn LLMClient>, None, None);

        let msg = contexa_context::Message::user("ping");
        let req = ChatRequest::new(std::slice::from_ref(&msg), &[])
            .with_extra("thinking", serde_json::json!({"type": "enabled"}))
            .with_extra("reasoning_effort", serde_json::json!("high"));
        wrapped.chat(req).await.unwrap();

        let extra = cap.captured.lock().unwrap().clone().unwrap();
        // 调用方塞的值原样透传
        assert_eq!(
            extra.get("thinking").and_then(|v| v.get("type")).and_then(|v| v.as_str()),
            Some("enabled")
        );
        assert_eq!(
            extra.get("reasoning_effort").and_then(|v| v.as_str()),
            Some("high")
        );
    }

    /// 调用方显式设置了 `thinking` → 装饰器尊重，不覆盖（意图分类等轻任务用）。
    /// 例：分类请求想 `thinking.type=disabled` 快速出结构化 JSON，全局 effort=high
    /// 也不该把它强开成 enabled。
    #[tokio::test]
    async fn decorator_respects_explicit_thinking() {
        let cap = Arc::new(Capture {
            captured: Mutex::new(None),
        });
        let wrapped = ReasoningEffortClient::new(
            cap.clone() as Arc<dyn LLMClient>,
            Some("high".into()),
            None,
        );

        let msg = contexa_context::Message::user("ping");
        let req = ChatRequest::new(std::slice::from_ref(&msg), &[])
            .with_extra("thinking", serde_json::json!({"type": "disabled"}));
        wrapped.chat(req).await.unwrap();

        let extra = cap.captured.lock().unwrap().clone().unwrap();
        assert_eq!(
            extra.get("thinking").and_then(|v| v.get("type")).and_then(|v| v.as_str()),
            Some("disabled"),
            "显式 disabled 应保留，不被全局 effort 覆盖"
        );
        assert!(
            extra.get("reasoning_effort").is_none(),
            "显式 thinking 时不发明 reasoning_effort"
        );
    }

    /// 调用方显式设置了 thinking 且带 reasoning_effort → 两个都保留原样。
    #[tokio::test]
    async fn decorator_respects_explicit_thinking_and_effort() {
        let cap = Arc::new(Capture {
            captured: Mutex::new(None),
        });
        let wrapped = ReasoningEffortClient::new(
            cap.clone() as Arc<dyn LLMClient>,
            Some("low".into()),
            None,
        );

        let msg = contexa_context::Message::user("ping");
        let req = ChatRequest::new(std::slice::from_ref(&msg), &[])
            .with_extra("reasoning_effort", serde_json::json!("high"))
            .with_extra("thinking", serde_json::json!({"type": "enabled"}));
        wrapped.chat(req).await.unwrap();

        let extra = cap.captured.lock().unwrap().clone().unwrap();
        assert_eq!(
            extra.get("thinking").and_then(|v| v.get("type")).and_then(|v| v.as_str()),
            Some("enabled")
        );
        assert_eq!(
            extra.get("reasoning_effort").and_then(|v| v.as_str()),
            Some("high")
        );
    }

    /// 端到端：用 `ProviderConfig::build_client` 构造真实 OpenAI 兼容客户端，
    /// 本地起极简 HTTP server 抓包，验证 wire body 顶层同时含
    /// `thinking: {"type": "enabled"}` 和 `reasoning_effort: "low"`。
    /// 这是把"装饰器注入 → wire body 序列化"整条链路打通的关键测试。
    #[tokio::test]
    async fn end_to_end_provider_config_emits_thinking_and_reasoning_effort() {
        use std::sync::Arc as StdArc;
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let captured_body: StdArc<Mutex<Option<Vec<u8>>>> = StdArc::new(Mutex::new(None));
        let cap_for_server = captured_body.clone();

        let response_body = r#"{"id":"x","choices":[{"message":{"role":"assistant","content":"hi"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 32 * 1024];
            let n = sock.read(&mut buf).await.unwrap();
            *cap_for_server.lock().unwrap() = Some(buf[..n].to_vec());
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
        });

        // 关键路径：ProviderConfig::build_client → ReasoningEffortClient 装饰 →
        // OpenAICompatClient → wire body
        let p = crate::config::ProviderConfig {
            provider: crate::config::LlmProvider::OpenAICompat,
            base_url: Some(format!("http://{addr}/v1")),
            model: "deepseek-v4-flash".into(),
            api_key: Some("k".into()),
            reasoning_effort: Some("low".into()),
            ..Default::default()
        };
        let client = p.build_client().expect("build_client ok");
        let msg = contexa_context::Message::user("ping");
        let req = ChatRequest::new(std::slice::from_ref(&msg), &[]);
        let resp = client.chat(req).await.expect("chat ok");
        assert_eq!(resp.content, "hi");
        server.await.unwrap();

        let raw = captured_body.lock().unwrap().clone().expect("captured");
        let raw_str = String::from_utf8_lossy(&raw);
        let body_start = raw_str.rfind("\r\n\r\n").expect("body separator") + 4;
        let body = &raw[body_start..];
        let body_str = String::from_utf8_lossy(body);
        let v: serde_json::Value = serde_json::from_slice(body).expect("body json");
        assert_eq!(
            v["reasoning_effort"], "low",
            "wire body 顶层必须包含 reasoning_effort 字段；raw body 片段：{}",
            body_str.chars().take(400).collect::<String>()
        );
        assert_eq!(
            v["thinking"]["type"], "enabled",
            "wire body 顶层必须包含 thinking.type=\"enabled\"（DeepSeek 推荐用法）"
        );
        assert_eq!(v["model"], "deepseek-v4-flash");
    }

    /// 端到端：level="none" 时 wire body 应只有 `thinking.type="disabled"`，
    /// **不应**出现 `reasoning_effort` 字段（避免污染 + 服务端会忽略）。
    #[tokio::test]
    async fn end_to_end_provider_config_emits_only_thinking_disabled_when_none() {
        use std::sync::Arc as StdArc;
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let captured_body: StdArc<Mutex<Option<Vec<u8>>>> = StdArc::new(Mutex::new(None));
        let cap_for_server = captured_body.clone();

        let response_body = r#"{"id":"x","choices":[{"message":{"role":"assistant","content":"hi"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 32 * 1024];
            let n = sock.read(&mut buf).await.unwrap();
            *cap_for_server.lock().unwrap() = Some(buf[..n].to_vec());
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
        });

        let p = crate::config::ProviderConfig {
            provider: crate::config::LlmProvider::OpenAICompat,
            base_url: Some(format!("http://{addr}/v1")),
            model: "deepseek-v4-flash".into(),
            api_key: Some("k".into()),
            reasoning_effort: Some("none".into()),
            ..Default::default()
        };
        let client = p.build_client().expect("build_client ok");
        let msg = contexa_context::Message::user("ping");
        let req = ChatRequest::new(std::slice::from_ref(&msg), &[]);
        let _ = client.chat(req).await.expect("chat ok");
        server.await.unwrap();

        let raw = captured_body.lock().unwrap().clone().expect("captured");
        let raw_str = String::from_utf8_lossy(&raw);
        let body_start = raw_str.rfind("\r\n\r\n").expect("body separator") + 4;
        let body = &raw[body_start..];
        let v: serde_json::Value = serde_json::from_slice(body).expect("body json");
        eprintln!("DEBUG wire body = {v}");
        assert_eq!(v["thinking"]["type"], "disabled");
        assert!(
            v.get("reasoning_effort").is_none(),
            "level=none 时不应发明 reasoning_effort 字段；wire = {v}"
        );
    }
}
