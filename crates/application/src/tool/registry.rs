//! `qview::ToolRegistry`：包装 `contexa_tools::ToolRegistry`。
//!
//! 关键能力：
//! - 收集 `LocalTool` 实例；每条配一份 `ToolMetadata`（UI 侧读）。
//! - `effective_tools()` 在 contexa 合并结果上再叠 qview allowlist 二次过滤。
//! - `call_tool()` 拦截不在白名单的调用并返回 `ToolResult { is_error: true }`，
//!   防止 LLM 绕过 schema 调用未授权工具。
//! - 工具结果脱敏（按 `PermissionPolicy::redact_patterns`）。

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use serde_json::Value;

use contexa::prelude::*;
use contexa_tools::{LocalTool, ToolRegistry as CtxRegistry, ToolResult, ToolSource};

use crate::protocol::{PermissionPolicy, SideEffect};

use super::metadata::ToolMetadata;

/// qview 端工具注册表。
///
/// 内部用 `contexa_tools::ToolRegistry` 作为执行视图；额外维护：
/// - 元数据表（按工具名索引）
/// - 当前权限策略（运行时可热替换）
/// - 预编译的脱敏正则
pub struct ToolRegistry {
    inner: CtxRegistry,
    /// 工具名 → 元数据。
    meta: RwLock<HashMap<String, ToolMetadata>>,
    /// 当前权限策略（Arc 共享给 Agent Runtime）。
    policy: RwLock<PermissionPolicy>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("tools", &self.inner.len())
            .field("meta_keys", &self.meta.read().len())
            .finish()
    }
}

impl ToolRegistry {
    /// 创建一个空注册表。
    pub fn new(policy: PermissionPolicy) -> Self {
        Self {
            inner: CtxRegistry::new(),
            meta: RwLock::new(HashMap::new()),
            policy: RwLock::new(policy),
        }
    }

    /// 注册一个工具 + 元数据。
    pub fn register(&mut self, tool: LocalTool, meta: ToolMetadata) {
        let name = tool.name().to_string();
        self.inner.push_local(tool);
        self.meta.write().insert(name, meta);
    }

    /// 一次性注册多个（顺序无关；冲突由 contexa 的 `effective_tools` 负责）。
    pub fn register_many(
        &mut self,
        entries: impl IntoIterator<Item = (LocalTool, ToolMetadata)>,
    ) {
        for (t, m) in entries {
            self.register(t, m);
        }
    }

    /// 当前所有工具元数据的快照。
    pub fn metadata(&self) -> Vec<ToolMetadata> {
        self.meta.read().values().cloned().collect()
    }

    /// 按名查元数据。
    pub fn metadata_of(&self, name: &str) -> Option<ToolMetadata> {
        self.meta.read().get(name).cloned()
    }

    /// 当前策略的快照。
    pub fn policy(&self) -> PermissionPolicy {
        self.policy.read().clone()
    }

    /// 替换权限策略（线程安全；不影响已注册工具的元数据）。
    pub fn set_policy(&self, policy: PermissionPolicy) {
        *self.policy.write() = policy;
    }

    /// 包装为 `Arc<dyn ToolSource>` 注入到 `ReActWorker::instance_sources`。
    pub fn as_arc_source(self: &Arc<Self>) -> Arc<dyn ToolSource> {
        Arc::clone(self) as Arc<dyn ToolSource>
    }

    /// 列出当前所有工具 spec（去 worker_finish 末尾追加；contexa 的 `effective_tools` 会做）。
    pub async fn list_specs(&self) -> Vec<ToolSpec> {
        self.inner
            .list_tools()
            .await
            .unwrap_or_default()
    }

    /// 调用工具。在分发前做两层校验：
    /// 1. 策略允许该工具（`policy.allows(name)`）
    /// 2. 工具副作用级别是否需要审批（qview-agent 端用 GuardedTool 处理；这里仅做只读路径）
    ///
    /// 失败 → 返回 `ToolResult { is_error: true, content: <reason> }`，
    /// 让 LLM 自行决定下一步（**不抛** ContexaError）。
    pub async fn call_tool(&self, name: &str, args: Value) -> ToolResult {
        let policy = self.policy.read().clone();
        if !policy.allows(name) {
            return ToolResult::err(serde_json::json!({
                "error": "tool_not_allowed",
                "tool": name,
                "message": "工具不在当前会话的白名单中",
            }));
        }

        let raw = match self.inner.call_tool(name, args).await {
            Ok(tr) => tr,
            Err(e) => {
                return ToolResult::err(serde_json::json!({
                    "error": "tool_invocation",
                    "tool": name,
                    "message": format!("{e}"),
                }));
            }
        };

        // 脱敏管道（仅替换 content 中的字符串 / 数字字段，保留结构）
        apply_redaction(raw, &policy.redact_patterns)
    }

    /// 列出允许的（白名单过滤后）工具 spec。
    pub async fn effective_specs(&self) -> Vec<ToolSpec> {
        let policy = self.policy.read().clone();
        let mut out = Vec::new();
        for spec in self.inner.list_tools().await.unwrap_or_default() {
            if policy.allows(&spec.name) {
                out.push(spec);
            }
        }
        // 末尾追加 worker_finish（contexa 也会追加，这里保持一致方便 UI 展示）
        out.push(contexa_core::finish_tool_spec());
        out
    }
}

#[async_trait::async_trait]
impl ToolSource for ToolRegistry {
    async fn list_tools(&self) -> contexa_context::Result<Vec<ToolSpec>> {
        // 这里返回**未过滤**的 spec；过滤在 call_tool 侧做，
        // 让 Agent 的 schema 仍然可见所有"已注册"工具，但实际执行被拦截。
        // 如果想严格按白名单裁剪，调用方用 `effective_specs()`。
        self.inner.list_tools().await
    }

    async fn call_tool(&self, name: &str, args: Value) -> contexa_context::Result<ToolResult> {
        Ok(ToolRegistry::call_tool(self, name, args).await)
    }

    fn name(&self) -> &str {
        "qview-tool-registry"
    }
}

impl ToolRegistry {
    /// 是否某个副作用级别需要走 GuardedTool。
    pub fn requires_guard(&self, side: SideEffect) -> bool {
        self.policy.read().needs_approval(side)
    }
}

/// 对 `ToolResult.content` 做递归字符串脱敏。
///
/// 仅替换 `String` / `Number`；保留对象/数组结构。
fn apply_redaction(mut result: ToolResult, patterns: &[String]) -> ToolResult {
    if patterns.is_empty() {
        return result;
    }
    if let Ok(mut compiled) = compile_patterns(patterns) {
        redact_value(&mut result.content, &mut compiled);
    }
    result
}

/// 预编译的正则列表。构造期做一次；调用期只读。
struct CompiledPatterns(Vec<regex::Regex>);

fn compile_patterns(patterns: &[String]) -> Result<CompiledPatterns, regex::Error> {
    let mut out = Vec::with_capacity(patterns.len());
    for p in patterns {
        match regex::Regex::new(p) {
            Ok(r) => out.push(r),
            // 单个正则编译失败 → 跳过该条，避免整个脱敏瘫痪
            Err(_) => continue,
        }
    }
    Ok(CompiledPatterns(out))
}

fn redact_value(v: &mut Value, patterns: &mut CompiledPatterns) {
    match v {
        Value::String(s) => {
            for p in &patterns.0 {
                *s = p.replace_all(s, "***").into_owned();
            }
        }
        // 数字字段同样脱敏：若用户配置 `\d{16}`，会命中 size_bytes / id 等。
        Value::Number(n) => {
            let s = n.to_string();
            let mut replaced = s.clone();
            let mut any = false;
            for p in &patterns.0 {
                let after = p.replace_all(&replaced, "***");
                if after != replaced {
                    any = true;
                    replaced = after.into_owned();
                }
            }
            if any {
                *v = Value::String(replaced);
            }
        }
        Value::Array(arr) => {
            for item in arr {
                redact_value(item, patterns);
            }
        }
        Value::Object(map) => {
            for (_k, val) in map.iter_mut() {
                redact_value(val, patterns);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dummy_tool(name: &str) -> LocalTool {
        LocalTool::from_async_fn(
            name,
            "x",
            json!({"type":"object"}),
            contexa_tools::boxed_invoke(|_| {
                Box::pin(async { Ok(ToolResult::ok(json!({"hello":"world"}))) })
            }),
        )
        .unwrap()
    }

    #[test]
    fn allowlist_blocks_unauthorized_tool() {
        let mut reg = ToolRegistry::new(PermissionPolicy::with_allowlist(vec![
            "allowed".into(),
        ]));
        reg.register(dummy_tool("allowed"), ToolMetadata::new("allowed", "x", SideEffect::ReadOnly, crate::tool::metadata::ToolGroup::Document));
        reg.register(dummy_tool("blocked"), ToolMetadata::new("blocked", "x", SideEffect::ReadOnly, crate::tool::metadata::ToolGroup::Document));
        let rt = futures::executor::block_on(reg.call_tool("blocked", json!({})));
        assert!(rt.is_error);
        let r = futures::executor::block_on(reg.call_tool("allowed", json!({})));
        assert!(!r.is_error);
    }

    #[test]
    fn redaction_replaces_in_strings() {
        let p = vec![r"\b\d{16}\b".to_string()];
        let mut res = ToolResult::ok(json!({"card": "4111111111111111"}));
        res = apply_redaction(res, &p);
        // 整段 16 位数字被替换为 ***
        assert_eq!(res.content["card"], "***");
    }
}
