//! 一次性 helper：把「需要审批」的写工具包成 GuardedTool，
//! 连同只读工具一起注入到 `ReActWorker`（P4 集成）。

use std::sync::Arc;

use anyhow::Context as _;

use contexa_core::ReActWorker;
use contexa_tools::{LocalTool, ToolSource};

use qview_application::protocol::SideEffect;
use qview_application::service::annotation::AnnotationService;
use qview_application::service::document::DocumentService;
use qview_application::tools::{
    annotate_delete_tool, annotate_tool, annotate_update_tool, export_tool, write_document_tool,
    ALL_TOOL_NAMES_WITH_WRITES,
};

use crate::approval::ApprovalRegistry;
use crate::guarded_tool::{GuardedTool, GuardedToolMeta, InnerInvokeFn};
use crate::sink_hook::WeakSinks;

/// 把任意 `LocalTool` 包成 GuardedTool 并挂共享 WeakSinks（生产必需，审批事件才到 UI）。
fn guarded_source(
    tool: LocalTool,
    side_effect: SideEffect,
    reason: &str,
    approvals: Arc<ApprovalRegistry>,
    shared: &WeakSinks,
) -> anyhow::Result<Arc<dyn ToolSource>> {
    let name = tool.name().to_string();
    let spec = tool.spec().clone();
    let source: Arc<dyn ToolSource> = Arc::new(tool);
    let meta = GuardedToolMeta {
        name: name.clone(),
        spec,
        side_effect,
        reason: reason.to_string(),
    };
    let inner: InnerInvokeFn = Arc::new(move |args| {
        let source = source.clone();
        let name = name.clone();
        Box::pin(async move { source.call_tool(&name, args).await })
    });
    let g = GuardedTool::new(meta, approvals, inner);
    g.set_shared_sinks(shared.clone());
    Ok(Arc::new(g) as Arc<dyn ToolSource>)
}

/// 构造一个 `annotate_create` GuardedTool 包装器（向后兼容，不挂共享 sink）。
pub fn make_annotate_guarded(
    ann: Arc<AnnotationService>,
    approvals: Arc<ApprovalRegistry>,
) -> anyhow::Result<GuardedTool> {
    let tool = annotate_tool(ann)?;
    let name = tool.name().to_string();
    let spec = tool.spec().clone();
    let meta = GuardedToolMeta {
        name: name.clone(),
        spec,
        side_effect: qview_application::protocol::SideEffect::Reversible,
        reason: "将在选中范围创建批注（写入 AnnotationStore）".into(),
    };
    let source: Arc<dyn ToolSource> = Arc::new(tool);
    let inner: InnerInvokeFn = Arc::new(move |args| {
        let source = source.clone();
        let name = name.clone();
        Box::pin(async move { source.call_tool(&name, args).await })
    });
    Ok(GuardedTool::new(meta, approvals, inner))
}

/// 构造一个 `export_report` GuardedTool 包装器（向后兼容，不挂共享 sink）。
pub fn make_export_guarded(
    ann: Arc<AnnotationService>,
    approvals: Arc<ApprovalRegistry>,
) -> anyhow::Result<GuardedTool> {
    let tool = export_tool(ann)?;
    let name = tool.name().to_string();
    let spec = tool.spec().clone();
    let meta = GuardedToolMeta {
        name: name.clone(),
        spec,
        side_effect: qview_application::protocol::SideEffect::Mutating,
        reason: "将导出分析报告到 data/reports/".into(),
    };
    let source: Arc<dyn ToolSource> = Arc::new(tool);
    let inner: InnerInvokeFn = Arc::new(move |args| {
        let source = source.clone();
        let name = name.clone();
        Box::pin(async move { source.call_tool(&name, args).await })
    });
    Ok(GuardedTool::new(meta, approvals, inner))
}

/// 把「需要审批」的写工具 GuardedTool 包装器聚合成 `Vec<Arc<dyn ToolSource>>`。
///
/// `guard_names` 为空（require_approval 不覆盖任何写工具）→ 返回空，所有写工具
/// 已在 registry 里以普通 LocalTool 形式注册（自动放行）。
pub fn make_guarded_sources(
    ann: Arc<AnnotationService>,
    docs: Arc<DocumentService>,
    approvals: Arc<ApprovalRegistry>,
    guard_names: &[&str],
    shared: WeakSinks,
) -> anyhow::Result<Vec<Arc<dyn ToolSource>>> {
    let mut out = Vec::new();
    if guard_names.contains(&"annotate_create") {
        out.push(guarded_source(
            annotate_tool(ann.clone())?,
            SideEffect::Reversible,
            "将在选中范围创建批注（写入 AnnotationStore）",
            approvals.clone(),
            &shared,
        )?);
    }
    if guard_names.contains(&"annotate_update") {
        out.push(guarded_source(
            annotate_update_tool(ann.clone())?,
            SideEffect::Reversible,
            "将修改批注文本（写入 AnnotationStore）",
            approvals.clone(),
            &shared,
        )?);
    }
    if guard_names.contains(&"annotate_delete") {
        out.push(guarded_source(
            annotate_delete_tool(ann.clone())?,
            SideEffect::Reversible,
            "将删除一条批注（从 AnnotationStore 移除）",
            approvals.clone(),
            &shared,
        )?);
    }
    if guard_names.contains(&"export_report") {
        out.push(guarded_source(
            export_tool(ann)?,
            SideEffect::Mutating,
            "将导出分析报告到 data/reports/",
            approvals.clone(),
            &shared,
        )?);
    }
    if guard_names.contains(&"write_document") {
        out.push(guarded_source(
            write_document_tool(docs)?,
            SideEffect::Mutating,
            "将写入 / 覆写一个文件到磁盘",
            approvals,
            &shared,
        )?);
    }
    Ok(out)
}

/// 构造允许全部工具的 `PermissionPolicy::allow_tools`（含写工具）。
pub fn allow_all_with_writes() -> Vec<String> {
    ALL_TOOL_NAMES_WITH_WRITES.iter().map(|s| s.to_string()).collect()
}

/// 给 worker 注入 instance_sources：包含只读 LocalTool + GuardedTool 写工具。
pub fn attach_sources(
    worker: &mut ReActWorker,
    read_only_tools: Vec<LocalTool>,
    write_sources: Vec<Arc<dyn ToolSource>>,
) -> anyhow::Result<()> {
    let mut sources: Vec<Arc<dyn ToolSource>> = Vec::new();
    for t in read_only_tools {
        sources.push(Arc::new(t) as Arc<dyn ToolSource>);
    }
    sources.extend(write_sources);
    worker.instance_sources = sources;
    worker.validate().context("validate worker")?;
    Ok(())
}
