//! 意图分类 Router（架构 §22.x — P1「意图层」落地，**LLM 驱动版**）。
//!
//! ## 设计原则（用户明确要求）
//!
//! **路由层永远是模型判断，不是代码死板匹配。**
//!
//! - 不用正则 / 关键词 / `starts_with` 来理解模糊的用户文本。
//! - 代码只做**结构化协议解析**：LLM 对用户输入输出一个 JSON 对象
//!   （`kind / confidence / params / plan / reply`），代码把它解析成 [`Intent`]。
//! - LLM 无法分类（JSON 解析失败 / 调用失败 / 输出非预期）→ fallback
//!   [`Intent::unknown`] → runtime 走完整 ReAct 兜底。
//!
//! ## 与旧版（正则匹配）的差异
//!
//! 旧版 `patterns` 模块用 `contains("打开")` / `starts_with("帮我")` 等硬规则，
//! 已证明脆弱：`帮我打开start文件看看` 抽错路径、`帮我打开xxx` 完全不命中。
//! 新版让 LLM 自己理解语义、自己抽参数——代码不猜。
//!
//! ## 工具筛选
//!
//! `suggested_tools` 仍由 [`IntentKind`] → 工具组的**领域映射**得出
//! （`application::tools::tools_for`），这是"意图→最小工具集"的固定领域知识，
//! 不属于"理解文本"，保持不变。

use std::collections::HashMap;
use std::sync::Arc;

use contexa_llm::{ChatRequest, LLMClient};
use serde::Deserialize;

/// 意图分类。
///
/// ## 设计原则
/// - 枚举值与"用户动作"对齐（不是工具名），便于未来加新意图而不破坏 ABI。
/// - `Unknown` 是兜底：router 不确定时回退全集工具、走完整 ReAct 多轮。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntentKind {
    /// 闲聊 / 招呼 / 不需要工具。
    Chat,
    /// "打开 xx 文件"。
    OpenFile,
    /// 查生产日志 / 搜索文件内容。
    SearchLog,
    /// 读某段上下文。
    ReadContext,
    /// 打批注。
    AnnotateFile,
    /// 编辑文件（写操作）。
    EditFile,
    /// 出报告。
    ExportReport,
    /// 列出目录文件。
    ListDir,
    /// 列出当前文件批注。
    ListAnnotations,
    /// 跳转 / 定位到某行或某个批注（"跳到第 100 行"、"跳到批注②"）。
    NavigateToLine,
    /// 改 AI 设置（"切换模型"、"关掉思考"等）。
    ConfigureAgent,
    /// 查看系统信息（OS / 内存 / CPU / 磁盘 / 网络）。
    SystemInfo,
    /// 兜底，走完整 ReAct。
    Unknown,
}

impl IntentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::OpenFile => "OpenFile",
            Self::SearchLog => "SearchLog",
            Self::ReadContext => "ReadContext",
            Self::AnnotateFile => "AnnotateFile",
            Self::EditFile => "EditFile",
            Self::ExportReport => "ExportReport",
            Self::ListDir => "ListDir",
            Self::ListAnnotations => "ListAnnotations",
            Self::NavigateToLine => "NavigateToLine",
            Self::ConfigureAgent => "ConfigureAgent",
            Self::SystemInfo => "SystemInfo",
            Self::Unknown => "Unknown",
        }
    }

    /// 协议容错：LLM 可能输出 `"open_file"` / `"OpenFile"` / `"OPENFILE"` 等变体。
    /// 这里做大小写不敏感 + 下划线归一化匹配——是**枚举解析容错**，不是文本理解。
    fn from_str_fuzzy(s: &str) -> Option<Self> {
        let norm: String = s
            .trim()
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '_' && *c != '-')
            .collect::<String>()
            .to_ascii_lowercase();
        match norm.as_str() {
            "chat" | "greeting" | "smalltalk" => Some(Self::Chat),
            "openfile" | "open" | "opendocument" => Some(Self::OpenFile),
            "searchlog" | "search" | "searchtext" => Some(Self::SearchLog),
            "readcontext" | "read" => Some(Self::ReadContext),
            "annotatefile" | "annotate" | "annotation" => Some(Self::AnnotateFile),
            "editfile" | "edit" | "write" => Some(Self::EditFile),
            "exportreport" | "export" | "report" => Some(Self::ExportReport),
            "listdir" | "listdirectory" | "list" => Some(Self::ListDir),
            "listannotations" | "listannot" => Some(Self::ListAnnotations),
            "navigatetoline" | "navigate" | "jump" | "goto" | "jumptoline" => Some(Self::NavigateToLine),
            "configureagent" | "configure" | "settings" => Some(Self::ConfigureAgent),
            "systeminfo" | "system" | "sysinfo" | "hardware" => Some(Self::SystemInfo),
            _ => None,
        }
    }
}

/// 一次意图分类的结果。
///
/// ## 字段语义
/// - `kind`：见 [`IntentKind`]
/// - `confidence`：0.0 - 1.0
/// - `params`：LLM 从用户 query 里抽出的参数（file path、关键字等）
/// - `suggested_tools`：本轮推给 LLM 的工具 schema 子集；空 Vec = 全集
/// - `plan`：项目经理对用户需求的分步执行计划（拆解成可执行步骤）；非任务类为 `None`
/// - `reply`：**仅 `Chat` 意图**——LLM 直接给出的对用户回复正文。
///   runtime 见 `Some(reply)` 时直接广播它作为最终回复，不再调 ReAct。
#[derive(Debug, Clone)]
pub struct Intent {
    pub kind: IntentKind,
    pub confidence: f32,
    pub params: HashMap<String, String>,
    pub suggested_tools: Vec<&'static str>,
    pub plan: Option<String>,
    pub reply: Option<String>,
}

impl Intent {
    /// 构造 Unknown 兜底（不推工具子集）。
    pub fn unknown() -> Self {
        Self {
            kind: IntentKind::Unknown,
            confidence: 0.0,
            params: HashMap::new(),
            suggested_tools: Vec::new(),
            plan: None,
            reply: None,
        }
    }

    /// 构造 Chat 闲聊（reply 是 LLM 给的直接回复）。
    pub fn chat(confidence: f32, reply: impl Into<String>) -> Self {
        Self {
            kind: IntentKind::Chat,
            confidence,
            params: HashMap::new(),
            suggested_tools: Vec::new(),
            plan: None,
            reply: Some(reply.into()),
        }
    }

    /// `IntentKind` → `qview_application::tools::IntentKindTag` 映射。
    ///
    /// 两套枚举独立存在是为了避免循环依赖（application 是 agent 的依赖，
    /// 反向引用会破 crate 关系）；这里手动保持一一对应。
    pub fn tag_for(kind: IntentKind) -> qview_application::tools::IntentKindTag {
        use qview_application::tools::IntentKindTag;
        match kind {
            IntentKind::Chat => IntentKindTag::Chat,
            IntentKind::OpenFile => IntentKindTag::OpenFile,
            IntentKind::SearchLog => IntentKindTag::SearchLog,
            IntentKind::ReadContext => IntentKindTag::ReadContext,
            IntentKind::AnnotateFile => IntentKindTag::AnnotateFile,
            IntentKind::EditFile => IntentKindTag::EditFile,
            IntentKind::ExportReport => IntentKindTag::ExportReport,
            IntentKind::ListDir => IntentKindTag::ListDir,
            IntentKind::ListAnnotations => IntentKindTag::ListAnnotations,
            IntentKind::NavigateToLine => IntentKindTag::NavigateToLine,
            IntentKind::ConfigureAgent => IntentKindTag::ConfigureAgent,
            IntentKind::SystemInfo => IntentKindTag::SystemInfo,
            IntentKind::Unknown => IntentKindTag::Unknown,
        }
    }

    /// 该意图配套的最小可用工具集（领域映射，非文本理解）。
    pub fn suggested_tools_for(kind: IntentKind) -> Vec<&'static str> {
        qview_application::tools::tools_for(Self::tag_for(kind))
    }
}

/// 意图 Router：**LLM 驱动**。所有"理解用户意图"都交给模型，代码只解析 JSON。
pub struct IntentRouter;

/// LLM 分类的 JSON 中间形态（`serde` 解析用）。
#[derive(Debug, Deserialize)]
struct ClassifyJson {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    params: HashMap<String, String>,
    #[serde(default)]
    reply: Option<String>,
    #[serde(default)]
    plan: Option<String>,
}

/// 分类 system prompt：指引 LLM 用 `route_intent` 工具返回结构化意图。
/// 工具的 JSON Schema 已约束枚举，这里只做语义引导。
///
/// **重要**：`tool_choice` 强制 LLM 必须调用 `route_intent` 工具——它不能 text 回复
/// （否则纯问候会被 LLM 直接答话而不是结构化分类）。reply 字段里 LLM 填的是
/// 给用户的最终回复正文，口吻应像 qview 器灵「小Q」本人，**绝不自称"路由"**。
const CLASSIFY_SYSTEM: &str = r#"你是 qview 的项目经理「小Q」。qview 用 Rust 编写，打开几十 GB 的日志毫秒级、不卡顿、内存占用与文件大小无关；能读/搜/分析任意文本文件，且自带 read_context / search_text / inspect_matches / summarize_range / annotate_create / export_report 等工具。用户就是在 qview 里向你提问，文件可能已经打开。你不仅做意图路由，更是整个任务的**项目经理**：接需求 → 分析 → 制定计划 → 安排工具/员工执行 → 检查结果 → 满意提交 / 不满意返工 → 中途向用户汇报进度。

根据用户输入，调用 route_intent 工具返回结构化分类结果。你必须调用该工具，不要直接回答用户。

route_intent 参数要点：
- kind：从枚举里选最贴合的意图类型。
  - Chat —— 纯闲聊 / 问候 / 知识问答，当前输入不需要任何工具动作。
  - OpenFile —— 打开 / 查看某个文件。
  - SearchLog —— 在文件或日志里搜索关键词 / 查错误 / 找内容。
  - ReadContext —— 读取文件的某一段内容。
  - AnnotateFile —— 给文件打批注 / 标注疑点。
  - EditFile —— 编辑 / 修改 / 写入文件内容。
  - ExportReport —— 生成 / 导出 / 保存报告。
  - ListDir —— 列出某个目录下的文件。
  - ListAnnotations —— 列出某个文件的批注。
  - NavigateToLine —— 跳转 / 定位到某一行或某个批注（"跳到第 X 行"、"跳到批注②"、"定位到那一行"）。
  - ConfigureAgent —— 修改器灵的设置（切换模型、调整思考强度等）。
  - SystemInfo —— 查看当前系统信息（什么系统 / 系统版本 / 内存 / CPU / 磁盘 / 网络）。
  - Unknown —— 以上都不匹配，需要完整工具能力综合处理。
- confidence：置信度（0.0 ~ 1.0）。
- params：从输入里提取关键参数。文件/目录路径 → "path"；搜索关键词 → "query"；文档 ID → "document_id"；行号 → "line"。
- reply：**只有 kind=Chat 时**填对用户的直接回复正文。用 qview 器灵「小Q」的亲切口吻，1~2 句，不要提到"路由"或"分类器"身份。如果问题涉及"看文件 / 大文件 / 日志"，回复要体现 qview 能毫秒级打开大文件、能直接帮用户分析，**不要说**让用户去终端用 tail / head / less / grep。其他 kind 留空字符串 ""。
- plan：项目经理的分步执行计划。先**分析**（需要什么、文件/参数从哪来），再**拆解成可执行步骤**填进 plan（如 "1 打开文件 2 抽样看格式 3 搜关键词 4 看上下文 5 汇总结论"）。计划会注入执行上下文供你参考，可随执行调整。纯闲聊 / 不需要工具动作的任务填 null。suggested_tools 由代码按 kind 映射，你不用填。

多轮对话与 Chat 判定（重要）：
- "最近对话上下文"里若有**尚未完成的任务**（上一轮是 ListDir / OpenFile / SearchLog / SystemInfo 等），
  用户后续输入是对该任务的**补充、澄清、催促或追问**（如"项目叫 qview"、"帮我列出来"、
  "怎么还没弄完"、"你知道完整路径吗"、"内存呢"、"那 CPU 呢"），kind **必须继承上一轮的任务意图**
  （如 ListDir / SystemInfo），并在 params 里结合上下文修正 / 补全参数，**绝不能判成 Chat**。
- Chat **仅限**当前输入确实不需要任何工具动作的纯闲聊 / 问候 / 知识问答（如"你好"、"你能做什么"）。
  只要用户在推进某个实际任务（找文件 / 列目录 / 搜日志 / 看报错 / **问大文件能不能看**），就不是 Chat，
  而应路由到 OpenFile / ReadContext / SearchLog 等文件意图。
- 用户本轮没提新任务时，默认延续最近对话上下文里的任务意图，而不是退化成 Chat。

路径填写（重要）：
- path 尽量填**绝对路径**（如 /Users/用户名/Projects/qview/bench_data）。优先结合"最近对话上下文"
  里已提到的文件 / 目录 / 项目名拼出完整绝对路径；推断不出时，保留用户给出的字面路径，不要编造。
- 用户说"Projects 目录"等模糊说法时，通常指用户主目录下的 Projects（macOS: ~/Projects；
  Windows: C:\Users\用户名\Projects）；项目名（如"qview"）往往是其中一级子目录名。
- 上一轮已提取出 path 而本轮没给新路径信息时，**沿用上一轮的 path**，不要留空。"#;

impl IntentRouter {
    /// 路由工具的 schema（contexa `ToolSpec`）。LLM 通过**调用这个工具**返回结构化意图，
    /// 而不是在 content 里输出 JSON——contexa 框架负责解析 `tool_calls`（arguments JSON →
    /// [`serde_json::Value`]），代码只读参数。
    pub fn route_tool() -> contexa_context::ToolSpec {
        contexa_context::ToolSpec::new_unchecked(
            "route_intent",
            "判断用户意图，返回结构化路由参数（意图类型 / 置信度 / 关键参数 / 执行计划 / 闲聊回复）。每次用户提问都调用它。",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": [
                            "Chat", "OpenFile", "SearchLog", "ReadContext", "AnnotateFile",
                            "EditFile", "ExportReport", "ListDir", "ListAnnotations",
                            "NavigateToLine", "ConfigureAgent", "SystemInfo", "Unknown"
                        ],
                        "description": "用户意图类型"
                    },
                    "confidence": {
                        "type": "number",
                        "minimum": 0.0,
                        "maximum": 1.0,
                        "description": "该判断的置信度 0.0-1.0"
                    },
                    "params": {
                        "type": "object",
                        "description": "关键参数：path(文件/目录路径)、query(搜索关键词)、document_id、line",
                        "additionalProperties": { "type": "string" }
                    },
                    "reply": {
                        "type": "string",
                        "description": "仅 kind=Chat 时填对用户的直接回复正文；其他意图留空字符串"
                    },
                    "plan": {
                        "type": ["string", "null"],
                        "description": "项目经理对用户需求的分步执行计划（拆解成可执行步骤）；非任务类填 null"
                    }
                },
                "required": ["kind", "confidence", "params", "plan", "reply"],
                "additionalProperties": false
            }),
        )
    }

    /// 主入口：让 LLM 对用户 query 做结构化分类。
    ///
    /// - **一次 LLM 调用**，只暴露 `route_intent` 一个工具（schema 极小，分类快）。
    ///   `thinking.type=disabled`（分类是轻任务，不需深度思考）。
    /// - **contexa 解析工具调用**：从 `resp.tool_calls` 找 `route_intent`，
    ///   用框架的 [`ToolCall::parsed_arguments`] 把 arguments 转成 JSON，代码再读参数。
    /// - **任何失败**（调用 Err / 没调工具 / 参数非预期 / kind 不认识）→ [`Intent::unknown`]
    ///   兜底走完整 ReAct，绝不让分类错误卡死用户。
    /// - **参数 / 回复全部来自模型**：`path` / `query` / `reply` 是 LLM 从语义里抽的，
    ///   代码不做任何文本清洗。
    /// - **`context`**：可选的多轮上下文（最近对话文本）。LLM 据此理解用户所指的
    ///   文件/目录——例如刚列过目录（里面是 `test_xl.log`）后说"打开10G那个测试文件"，
    ///   模型能结合上下文填对路径，而不是从字面猜 `10G测试文件`。
    /// 组装分类 prompt：系统分类说明 + 可选本机环境（真实主目录）+ 用户输入。
    /// 拆成独立函数便于测试注入是否生效。
    fn classify_messages(query: &str, context: Option<&str>) -> Vec<contexa_context::Message> {
        let query = query.trim();
        // 带上下文时，让模型"先看历史再判断当前输入"
        let user_msg = match context {
            Some(ctx) if !ctx.trim().is_empty() => format!(
                "## 最近对话上下文（供你理解用户所指的文件/目录，这是历史内容）\n{}\n\n## 用户当前输入\n{}\n\n请调用 route_intent 工具返回路由结果。",
                ctx.trim(),
                query
            ),
            _ => format!("## 用户输入\n{}\n\n请调用 route_intent 工具返回路由结果。", query),
        };
        let mut messages = vec![contexa_context::Message::system(CLASSIFY_SYSTEM.to_string())];
        // 注入本机环境：告诉模型**真实**主目录（运行时解析，跨平台），拼绝对路径时别用
        // 主机名/昵称猜用户名。
        if let Some(hint) = crate::runtime::home_env_hint() {
            messages.push(contexa_context::Message::system(hint));
        }
        messages.push(contexa_context::Message::user(user_msg));
        messages
    }

    pub async fn classify(llm: &Arc<dyn LLMClient>, query: &str, context: Option<&str>) -> Intent {
        let messages = Self::classify_messages(query, context);
        let tool = Self::route_tool();
        let tools = [tool];
        // tool_choice 强制模型必须调用 route_intent（不能 text 回复）——
        // 否则纯问候（"你好"）模型会直接答话而不是结构化分类。
        let req = ChatRequest::new(&messages, &tools)
            .with_tool_choice("route_intent")
            .with_extra("thinking", serde_json::json!({"type": "disabled"}));

        match llm.chat(req).await {
            Ok(resp) => {
                // contexa 已解析 tool_calls；找 route_intent 调用
                let tc = resp.tool_calls.iter().find(|t| t.function.name == "route_intent");
                match tc {
                    Some(tc) => match tc.parsed_arguments() {
                        Ok(args) => Self::parse(&args),
                        Err(e) => {
                            tracing::warn!(
                                target: "qview_agent",
                                "route_intent arguments 解析失败（{e}），回退 Unknown"
                            );
                            Intent::unknown()
                        }
                    },
                    None => {
                        tracing::warn!(
                            target: "qview_agent",
                            "LLM 未调用 route_intent（{} 个 tool_calls），回退 Unknown",
                            resp.tool_calls.len()
                        );
                        Intent::unknown()
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    target: "qview_agent",
                    "意图分类 LLM 调用失败，回退 Unknown：{e}"
                );
                Intent::unknown()
            }
        }
    }

    /// 把 `route_intent` 工具的参数（已由 contexa 解析为 `Value`）解析成 [`Intent`]。
    /// 只读结构化参数，不做文本匹配。
    fn parse(args: &serde_json::Value) -> Intent {
        let parsed: ClassifyJson = match serde_json::from_value(args.clone()) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    target: "qview_agent",
                    "意图分类参数解析失败（{e}），回退 Unknown；参数：{}",
                    args
                );
                return Intent::unknown();
            }
        };

        // kind 容错映射（枚举解析，非文本理解）
        let Some(kind) = parsed.kind.as_deref().and_then(IntentKind::from_str_fuzzy) else {
            tracing::warn!(
                target: "qview_agent",
                "意图分类 kind 未知（{}），回退 Unknown",
                parsed.kind.as_deref().unwrap_or("<none>")
            );
            return Intent::unknown();
        };

        let confidence = parsed.confidence.unwrap_or(0.0).clamp(0.0, 1.0);

        // 只有 Chat 才带 reply；其余忽略 reply（LLM 可能在非 Chat 时也给 reply）
        let reply = if kind == IntentKind::Chat {
            parsed.reply.filter(|r| !r.trim().is_empty())
        } else {
            None
        };

        // 项目经理的分步执行计划（空字符串 / 全空白视为无计划）
        let plan = parsed.plan.filter(|p| !p.trim().is_empty());

        Intent {
            kind,
            confidence,
            params: parsed.params,
            suggested_tools: Intent::suggested_tools_for(kind),
            plan,
            reply,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 固定返回一个 `route_intent` 工具调用的 LLM 客户端（模拟 contexa 解析后的结果）。
    struct FixedRouteLlm {
        /// arguments JSON 字符串（contexa 会解析成 Value；这里模拟 LLM 返回的 tool_call）。
        args_json: Mutex<String>,
    }

    #[async_trait::async_trait]
    impl LLMClient for FixedRouteLlm {
        async fn chat(&self, _req: ChatRequest<'_>) -> contexa_context::Result<contexa_llm::LLMResponse> {
            let args = self.args_json.lock().unwrap().clone();
            // 构造一个 route_intent 工具调用；arguments 是 JSON 字符串，
            // 复刻 contexa `translate_response` 的产物（Value::String(arguments)）。
            let tool_call = contexa_llm::ToolCall::new("call_1", "route_intent", serde_json::Value::String(args));
            let content: String = String::new();
            Ok(contexa_llm::LLMResponse::full(
                content,
                vec![tool_call],
                Default::default(),
            ))
        }
    }

    fn llm_route(args_json: &str) -> Arc<dyn LLMClient> {
        Arc::new(FixedRouteLlm {
            args_json: Mutex::new(args_json.to_string()),
        })
    }

    /// 返回无工具调用（LLM 没调 route_intent）的 LLM。
    struct NoToolLlm;
    #[async_trait::async_trait]
    impl LLMClient for NoToolLlm {
        async fn chat(&self, _req: ChatRequest<'_>) -> contexa_context::Result<contexa_llm::LLMResponse> {
            Ok(contexa_llm::LLMResponse::new("我在思考"))
        }
    }

    /// 抓拍 classify 发出的请求（验证 tool_choice + 工具 schema）。
    struct CaptureRequestLlm {
        captured: Mutex<Option<(Vec<String>, String)>>, // (tool names, tool_choice name)
    }
    #[async_trait::async_trait]
    impl LLMClient for CaptureRequestLlm {
        async fn chat(&self, req: ChatRequest<'_>) -> contexa_context::Result<contexa_llm::LLMResponse> {
            let tools: Vec<String> = req.tools.iter().map(|t| t.name.clone()).collect();
            let choice = req
                .extra
                .get("tool_choice")
                .and_then(|v| v.get("function"))
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            *self.captured.lock().unwrap() = Some((tools, choice));
            // 返回一个正常的 route_intent 调用让 classify 不报错
            let args = r#"{"kind":"Chat","confidence":0.9,"params":{},"reply":"你好呀","flow":null}"#;
            let tc = contexa_llm::ToolCall::new("c", "route_intent", serde_json::Value::String(args.into()));
            let content: String = String::new();
            Ok(contexa_llm::LLMResponse::full(content, vec![tc], Default::default()))
        }
    }

    // ── 路由工具 schema 单测 ────────────────────────────────────────────

    #[test]
    fn route_tool_schema_is_valid() {
        let spec = IntentRouter::route_tool();
        assert_eq!(spec.name, "route_intent");
        assert_eq!(spec.parameters["type"], "object");
        let props = spec.parameters["properties"].as_object().unwrap();
        for required in ["kind", "confidence", "params", "plan", "reply"] {
            assert!(props.contains_key(required), "schema 缺字段 {required}");
        }
        assert!(!props.contains_key("flow"), "flow 已废弃，schema 不应再含 flow");
        // plan 是可空字符串
        let plan_ty = spec.parameters["properties"]["plan"]["type"].as_array().unwrap();
        assert!(plan_ty.iter().any(|v| v == "string"));
        assert!(plan_ty.iter().any(|v| v == "null"));
        // kind 枚举覆盖所有意图
        let kinds = spec.parameters["properties"]["kind"]["enum"].as_array().unwrap();
        let kinds: Vec<&str> = kinds.iter().filter_map(|v| v.as_str()).collect();
        assert!(kinds.contains(&"Chat"));
        assert!(kinds.contains(&"OpenFile"));
        assert!(kinds.contains(&"NavigateToLine"));
        assert!(kinds.contains(&"Unknown"));
    }

    // ── 解析层单测（contexa 已解析的 Value → Intent）────────────────────

    #[test]
    fn parse_chat_with_reply() {
        let args: serde_json::Value = serde_json::json!({
            "kind": "Chat", "confidence": 0.97, "params": {},
            "reply": "在呢主人～有什么可以帮你的？", "flow": null
        });
        let intent = IntentRouter::parse(&args);
        assert_eq!(intent.kind, IntentKind::Chat);
        assert!(intent.confidence > 0.9);
        assert_eq!(
            intent.reply.as_deref(),
            Some("在呢主人～有什么可以帮你的？")
        );
        assert_eq!(intent.plan, None);
    }

    #[test]
    fn parse_open_file_with_path() {
        let args: serde_json::Value = serde_json::json!({
            "kind": "OpenFile", "confidence": 0.95,
            "params": {"path": r"C:\logs\start.txt"}, "reply": "", "flow": "OpenFile"
        });
        let intent = IntentRouter::parse(&args);
        assert_eq!(intent.kind, IntentKind::OpenFile);
        assert_eq!(
            intent.params.get("path").map(|s| s.as_str()),
            Some(r"C:\logs\start.txt")
        );
        assert!(intent.plan.is_none(), "无 plan 时该字段应为 None");
        // OpenFile 应该只推少数字符工具
        assert!(!intent.suggested_tools.is_empty());
        assert!(intent.suggested_tools.contains(&"open_document"));
    }

    #[test]
    fn parse_search_log_with_query() {
        let args: serde_json::Value = serde_json::json!({
            "kind": "SearchLog", "confidence": 0.9,
            "params": {"path": "prod.log", "query": "ERROR"},
            "reply": "", "flow": "SearchLogAndReport"
        });
        let intent = IntentRouter::parse(&args);
        assert_eq!(intent.kind, IntentKind::SearchLog);
        assert_eq!(intent.params.get("query").map(|s| s.as_str()), Some("ERROR"));
    }

    #[test]
    fn parse_plan_is_extracted() {
        // 任务类：LLM 给分步计划 → 应解析进 plan
        let args: serde_json::Value = serde_json::json!({
            "kind": "SearchLog", "confidence": 0.9,
            "params": {"path": "prod.log", "query": "ERROR"},
            "reply": "",
            "plan": "1 打开文件 2 抽样看格式 3 搜关键词 4 看上下文 5 汇总结论"
        });
        let intent = IntentRouter::parse(&args);
        assert_eq!(
            intent.plan.as_deref(),
            Some("1 打开文件 2 抽样看格式 3 搜关键词 4 看上下文 5 汇总结论")
        );
    }

    #[test]
    fn parse_blank_plan_is_none() {
        // 空白 / 空 plan → None
        for plan in [Some(""), Some("   "), None] {
            let mut m = serde_json::Map::new();
            m.insert("kind".into(), serde_json::json!("SystemInfo"));
            m.insert("confidence".into(), serde_json::json!(0.9));
            m.insert("params".into(), serde_json::json!({}));
            m.insert("reply".into(), serde_json::json!(""));
            m.insert("plan".into(), plan.map(|p| serde_json::json!(p)).unwrap_or(serde_json::Value::Null));
            let args: serde_json::Value = serde_json::Value::Object(m);
            let intent = IntentRouter::parse(&args);
            assert_eq!(intent.plan, None, "plan={plan:?} 应为 None");
        }
    }

    #[test]
    fn parse_kind_fuzzy_variants() {
        // 协议容错：下划线 / 大小写变体
        let args: serde_json::Value = serde_json::json!({
            "kind": "open_file", "confidence": 0.8, "params": {}, "reply": "", "flow": "open_file"
        });
        let intent = IntentRouter::parse(&args);
        assert_eq!(intent.kind, IntentKind::OpenFile);

        let args2: serde_json::Value = serde_json::json!({
            "kind": "  SearchLog  ", "confidence": 0.8, "params": {}, "reply": "", "flow": null
        });
        assert_eq!(IntentRouter::parse(&args2).kind, IntentKind::SearchLog);

        // NavigateToLine 变体（jump / navigate / goto）
        for kind_str in ["NavigateToLine", "navigate_to_line", "JumpToLine", "goto"] {
            let args: serde_json::Value = serde_json::json!({
                "kind": kind_str, "confidence": 0.9, "params": {"line": "100"}, "reply": "", "flow": null
            });
            let intent = IntentRouter::parse(&args);
            assert_eq!(intent.kind, IntentKind::NavigateToLine, "kind={kind_str}");
            assert_eq!(intent.params.get("line").map(|s| s.as_str()), Some("100"));
        }
    }

    #[test]
    fn parse_unknown_kind_falls_back_unknown() {
        let args: serde_json::Value = serde_json::json!({
            "kind": "Squirrel", "confidence": 0.9, "params": {}, "reply": "", "flow": null
        });
        assert_eq!(IntentRouter::parse(&args).kind, IntentKind::Unknown);
    }

    #[test]
    fn parse_non_object_falls_back_unknown() {
        assert_eq!(IntentRouter::parse(&serde_json::Value::Null).kind, IntentKind::Unknown);
        assert_eq!(IntentRouter::parse(&serde_json::json!("hi")).kind, IntentKind::Unknown);
    }

    #[test]
    fn parse_non_chat_ignores_reply() {
        // 非 Chat 时 reply 应被忽略（防止 LLM 乱填）
        let args: serde_json::Value = serde_json::json!({
            "kind": "OpenFile", "confidence": 0.9, "params": {},
            "reply": "顺便说点什么", "flow": "OpenFile"
        });
        let intent = IntentRouter::parse(&args);
        assert_eq!(intent.kind, IntentKind::OpenFile);
        assert!(intent.reply.is_none());
    }

    // ── classify 集成测试（LLM tool_call → contexa 解析 → Intent）──────

    #[tokio::test]
    async fn classify_chat_returns_reply() {
        let client = llm_route(
            r#"{"kind":"Chat","confidence":0.98,"params":{},"reply":"你好呀主人～","flow":null}"#,
        );
        let intent = IntentRouter::classify(&client, "你好", None).await;
        assert_eq!(intent.kind, IntentKind::Chat);
        assert_eq!(intent.reply.as_deref(), Some("你好呀主人～"));
    }

    #[tokio::test]
    async fn classify_open_file_extracts_path() {
        let client = llm_route(
            r#"{"kind":"OpenFile","confidence":0.93,"params":{"path":"start.txt"},"reply":"","flow":"OpenFile"}"#,
        );
        // "帮我打开start文件看看" —— 旧正则匹配不出的模糊输入，模型该能懂
        let intent = IntentRouter::classify(&client, "帮我打开start文件看看", None).await;
        assert_eq!(intent.kind, IntentKind::OpenFile);
        assert_eq!(intent.params.get("path").map(|s| s.as_str()), Some("start.txt"));
    }

    #[tokio::test]
    async fn classify_llm_error_falls_back_unknown() {
        struct ErrLlm;
        #[async_trait::async_trait]
        impl LLMClient for ErrLlm {
            async fn chat(&self, _: ChatRequest<'_>) -> contexa_context::Result<contexa_llm::LLMResponse> {
                Err(contexa_context::ContexaError::Llm("boom".into()))
            }
        }
        let client: Arc<dyn LLMClient> = Arc::new(ErrLlm);
        let intent = IntentRouter::classify(&client, "打开文件", None).await;
        assert_eq!(intent.kind, IntentKind::Unknown);
    }

    #[tokio::test]
    async fn classify_no_tool_call_falls_back_unknown() {
        let client: Arc<dyn LLMClient> = Arc::new(NoToolLlm);
        let intent = IntentRouter::classify(&client, "随便", None).await;
        assert_eq!(intent.kind, IntentKind::Unknown);
    }

    #[tokio::test]
    async fn classify_search_log_with_query() {
        let client = llm_route(
            r#"{"kind":"SearchLog","confidence":0.91,"params":{"path":"prod.log","query":"ERROR"},"reply":"","flow":"SearchLogAndReport"}"#,
        );
        let intent = IntentRouter::classify(&client, "帮我查一下 prod.log 里的 ERROR", None).await;
        assert_eq!(intent.kind, IntentKind::SearchLog);
        assert_eq!(intent.params.get("query").map(|s| s.as_str()), Some("ERROR"));
    }

    #[tokio::test]
    async fn parse_systeminfo_kind() {
        let client = llm_route(
            r#"{"kind":"SystemInfo","confidence":0.92,"params":{},"reply":"","flow":null}"#,
        );
        let intent = IntentRouter::classify(&client, "你是什么系统？", None).await;
        assert_eq!(intent.kind, IntentKind::SystemInfo);
        // SystemInfo 只推 system_info 一个工具，走通用 ReAct 单工具闭环
        assert_eq!(intent.suggested_tools, vec!["system_info"]);
    }

    #[test]
    fn classify_messages_injects_real_home_dir() {
        let msgs = IntentRouter::classify_messages("看看 Projects 里的东西", None);
        // 结构：分类 system + [本机环境 system] + user
        assert!(msgs.len() >= 3, "应有 分类/本机环境/用户 三段，实际 {} 段", msgs.len());
        // 本机环境段存在且含真实主目录
        let env = &msgs[1].content;
        assert!(env.contains("## 本机环境"), "应注入本机环境段");
        assert!(env.contains("用户主目录："), "应包含用户主目录");
        assert!(env.contains("不要") && env.contains("猜用户名"), "应警告别猜用户名");
        // 用户消息是最后一段，且仍是用户输入
        assert!(msgs.last().unwrap().content.contains("看看 Projects 里的东西"));
    }

    #[test]
    fn parse_systeminfo_fuzzy_variants() {
        // 协议容错：sysinfo / system / hardware 等变体都应落到 SystemInfo
        for kind_str in ["SystemInfo", "system_info", "sysinfo", "hardware"] {
            let args: serde_json::Value = serde_json::json!({
                "kind": kind_str, "confidence": 0.9, "params": {}, "reply": "", "flow": null
            });
            let intent = IntentRouter::parse(&args);
            assert_eq!(intent.kind, IntentKind::SystemInfo, "kind={kind_str}");
            assert_eq!(intent.suggested_tools, vec!["system_info"]);
        }
    }

    /// classify 带上下文：模型能引用之前对话里提到的文件名（如刚列过目录）。
    #[tokio::test]
    async fn classify_uses_context_for_path_resolution() {
        // 上下文里提到目录有 test_xl.log（10.5GB）→ "打开10G那个测试文件"应填对路径
        let client = llm_route(
            r#"{"kind":"OpenFile","confidence":0.97,"params":{"path":"D:\\data\\test_xl.log"},"reply":"","flow":"OpenFile"}"#,
        );
        let context = "用户：帮我看看 D:\\data 目录有啥\n器灵：目录里有 test_xl.log（10.5 GB）、config.json、qview.log";
        let intent = IntentRouter::classify(&client, "打开10G那个测试文件我看看", Some(context)).await;
        assert_eq!(intent.kind, IntentKind::OpenFile);
        assert_eq!(
            intent.params.get("path").map(|s| s.as_str()),
            Some(r"D:\data\test_xl.log"),
            "模型应结合上下文把模糊的'10G测试文件'解析成实际文件名"
        );
    }

    /// classify 必须强制模型调用 route_intent 工具（tool_choice）且只暴露这一个工具。
    /// 这是"纯问候不会让模型 text 回复、而是走结构化分类"的关键。
    #[tokio::test]
    async fn classify_forces_route_intent_tool() {
        let inner = Arc::new(CaptureRequestLlm {
            captured: Mutex::new(None),
        });
        let client: Arc<dyn LLMClient> = inner.clone();
        let _ = IntentRouter::classify(&client, "你好", None).await;

        let (tools, choice) = inner.captured.lock().unwrap().clone().expect("request captured");
        assert_eq!(tools, vec!["route_intent".to_string()], "只暴露 route_intent 一个工具");
        assert_eq!(choice, "route_intent", "tool_choice 强制 route_intent");
    }
}
