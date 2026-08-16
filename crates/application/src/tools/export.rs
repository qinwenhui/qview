//! `export_report` 工具：导出分析报告到 `data/reports/`（**GuardedTool**，架构 §7）。
//!
//! 格式：`format` 支持 `json` / `markdown` / `ndjson` 三种。

use std::path::PathBuf;
use std::sync::Arc;

use futures::future::FutureExt;
use serde_json::{json, Value};

use contexa_tools::{boxed_invoke, LocalTool, ToolResult};

use crate::protocol::SideEffect;
use crate::service::annotation::AnnotationService;
use crate::tool::metadata::{ToolGroup, ToolMetadata};

/// 工具元数据。
pub fn export_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "export_report",
        "导出分析报告（把完整报告正文写进 content，另含批注 + 元数据）到 data/reports/（需用户审批）",
        SideEffect::Mutating,
        ToolGroup::Export,
    )
}

/// 工具入参 JSON Schema。
pub fn export_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "format": {
                "type": "string",
                "enum": ["json", "markdown", "ndjson"],
                "default": "markdown",
                "description": "导出格式"
            },
            "name": {
                "type": "string",
                "description": "文件名（不含扩展名）；缺省用时间戳"
            },
            "include_annotations": {"type": "boolean", "default": true},
            "content": {
                "type": "string",
                "description": "报告正文（markdown 文本）——把你的**完整分析**写在这里（结论、数据、上下文、建议等），不要只给骨架；元数据键值对放 extra"
            },
            "extra": {
                "type": "object",
                "description": "附加到报告的键值对（markdown 用 front-matter / 元数据小节）"
            }
        },
        "required": [],
        "additionalProperties": false
    })
}

/// 构造工具。
pub fn export_tool(ann: Arc<AnnotationService>) -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "export_report",
        "导出分析报告（把完整报告正文写进 content，另含批注 + 元数据）到 data/reports/（需用户审批）",
        export_parameters(),
        boxed_invoke(move |args| {
            let ann = ann.clone();
            async move {
                let format = args
                    .get("format")
                    .and_then(|v| v.as_str())
                    .unwrap_or("markdown");
                let name = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let include_annotations = args
                    .get("include_annotations")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                // 报告正文：模型的完整分析（markdown 文本）。缺省为空 → 退回骨架。
                let content = args
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();

                let report_dir = report_dir();
                if let Err(e) = std::fs::create_dir_all(&report_dir) {
                    return Ok(ToolResult::err(json!({
                        "error": "create_dir_failed",
                        "path": report_dir.display().to_string(),
                        "message": format!("{e}")
                    })));
                }

                let filename = name.unwrap_or_else(|| {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    format!("report-{now}")
                });
                let ext = match format {
                    "json" => "json",
                    "ndjson" => "ndjson",
                    _ => "md",
                };
                let path: PathBuf = report_dir.join(format!("{filename}.{ext}"));

                // 组装报告内容
                let snap = if include_annotations {
                    Some(ann.snapshot_json())
                } else {
                    None
                };
                let extra = args.get("extra").cloned().unwrap_or(json!({}));
                let body = match format {
                    "json" => serde_json::to_string_pretty(&json!({
                        "content": content,
                        "extra": extra,
                        "annotations": snap,
                    }))
                    .unwrap_or_default(),
                    "ndjson" => {
                        let mut s = String::new();
                        if !content.is_empty() {
                            s.push_str(&format!("{}\n", serde_json::to_string(&json!({
                                "kind": "content", "value": content
                            })).unwrap_or_default()));
                        }
                        s.push_str(&format!("{}\n", serde_json::to_string(&json!({
                            "kind": "extra", "value": extra
                        })).unwrap_or_default()));
                        if let Some(a) = snap {
                            for (file, list) in a["files"].as_object().cloned().unwrap_or_default() {
                                for ann in list.as_array().cloned().unwrap_or_default() {
                                    s.push_str(&format!("{}\n", serde_json::to_string(&json!({
                                        "kind": "annotation",
                                        "file": file,
                                        "annotation": ann,
                                    })).unwrap_or_default()));
                                }
                            }
                        }
                        s
                    }
                    _ => {
                        // markdown
                        let mut md = String::new();
                        md.push_str(&format!("# qview 报告\n\n"));
                        // 正文：模型的完整分析（content 参数）。缺省时退回骨架。
                        if !content.is_empty() {
                            md.push_str(&content);
                            md.push_str("\n\n");
                        }
                        if !extra.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                            md.push_str("## 元数据\n\n");
                            if let Some(obj) = extra.as_object() {
                                for (k, v) in obj {
                                    md.push_str(&format!("- **{k}**: {v}\n"));
                                }
                            }
                            md.push('\n');
                        }
                        if let Some(a) = snap {
                            md.push_str(&format!("## 批注（{} 个文件）\n\n", a["files"].as_object().map(|o| o.len()).unwrap_or(0)));
                            if let Some(files) = a["files"].as_object() {
                                for (file, list) in files {
                                    md.push_str(&format!("### {file}\n\n"));
                                    if let Some(arr) = list.as_array() {
                                        for ann in arr {
                                            let id = ann["id"].as_u64().unwrap_or(0);
                                            let line = ann["start_line"].as_u64().unwrap_or(0);
                                            let text = ann["text"].as_str().unwrap_or("");
                                            md.push_str(&format!("- L{line} (#{id}): {text}\n"));
                                        }
                                    }
                                    md.push('\n');
                                }
                            }
                        }
                        md
                    }
                };

                if let Err(e) = std::fs::write(&path, &body) {
                    return Ok(ToolResult::err(json!({
                        "error": "write_failed",
                        "path": path.display().to_string(),
                        "message": format!("{e}")
                    })));
                }

                Ok(ToolResult::ok(json!({
                    "path": path.display().to_string(),
                    "bytes": body.len(),
                    "format": format,
                    "annotations_included": include_annotations,
                })))
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}

fn report_dir() -> PathBuf {
    std::env::var("QVIEW_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("reports")
}
