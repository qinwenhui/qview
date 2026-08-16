//! `inspect_matches` 工具：对一组搜索命中做聚合 / 采样统计。

use std::sync::Arc;

use futures::future::FutureExt;
use serde_json::{json, Value};

use contexa_tools::{boxed_invoke, LocalTool, ToolResult};

use qview_core::search::SearchOptions;

use crate::protocol::SideEffect;
use crate::service::search::SearchService;
use crate::tool::metadata::{ToolGroup, ToolMetadata};

use super::info::parse_doc_id;

/// 工具元数据。
pub fn inspect_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "inspect_matches",
        "对当前文档的搜索命中做聚合（总数 / 抽样 / 行号分布）",
        SideEffect::ReadOnly,
        ToolGroup::Search,
    )
}

/// 工具入参 JSON Schema。
pub fn inspect_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "document_id": {"type": "integer", "minimum": 1},
            "query": {"type": "string", "minLength": 1, "description": "重跑查询以获得命中"},
            "regex": {"type": "boolean", "default": false},
            "case_sensitive": {"type": "boolean", "default": false},
            "sample_size": {"type": "integer", "minimum": 1, "maximum": 200, "default": 10},
            "buckets": {"type": "integer", "minimum": 2, "maximum": 100, "default": 10}
        },
        "required": ["document_id", "query"],
        "additionalProperties": false
    })
}

/// 构造工具。
pub fn inspect_tool(search: Arc<SearchService>) -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "inspect_matches",
        "对当前文档的搜索命中做聚合（总数 / 抽样 / 行号分布）",
        inspect_parameters(),
        boxed_invoke(move |args| {
            let search = search.clone();
            async move {
                let Some(id) = parse_doc_id(&args) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"document_id"})));
                };
                let Some(query) = args.get("query").and_then(|v| v.as_str()) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"query"})));
                };
                let regex = args.get("regex").and_then(|v| v.as_bool()).unwrap_or(false);
                let case = args.get("case_sensitive").and_then(|v| v.as_bool()).unwrap_or(false);
                let sample_size = args
                    .get("sample_size")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10) as usize;
                let buckets = args
                    .get("buckets")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10) as usize;

                let opts = SearchOptions {
                    case_sensitive: case,
                    use_regex: regex,
                    whole_word: false,
                    crlf: false,
                };

                match search.search(id, query, opts, 5000, 0).await {
                    Ok(summary) => {
                        let total_lines_f = (summary.hits.last().map(|h| h.line + 1).unwrap_or(0)
                            .max(summary.hits.first().map(|h| h.line + 1).unwrap_or(0)))
                            as f64;
                        let bucket_size = if summary.hits.is_empty() {
                            1.0
                        } else {
                            (total_lines_f / buckets as f64).max(1.0)
                        };
                        let mut bucket_counts = vec![0u64; buckets];
                        for hit in &summary.hits {
                            let b =
                                ((hit.line as f64 / bucket_size) as usize).min(buckets - 1);
                            bucket_counts[b] += 1;
                        }
                        let distribution: Vec<Value> = bucket_counts
                            .iter()
                            .enumerate()
                            .map(|(i, c)| {
                                json!({
                                    "bucket": i,
                                    "range": [
                                        (i as f64 * bucket_size) as u64,
                                        ((i + 1) as f64 * bucket_size) as u64,
                                    ],
                                    "count": c,
                                })
                            })
                            .collect();

                        let sample: Vec<Value> = summary
                            .hits
                            .iter()
                            .take(sample_size)
                            .map(|h| {
                                json!({
                                    "line": h.line,
                                    "text": truncate_text(&h.text, 256),
                                })
                            })
                            .collect();

                        Ok(ToolResult::ok(json!({
                            "total": summary.total,
                            "returned": summary.returned,
                            "truncated": summary.truncated,
                            "sample": sample,
                            "line_distribution": distribution,
                            "elapsed_ms": summary.elapsed_ms,
                        })))
                    }
                    Err(e) => Ok(ToolResult::err(json!({
                        "error": "inspect_failed",
                        "message": format!("{e}")
                    }))),
                }
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}

fn truncate_text(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…[+{} chars]", &s[..max], s.len() - max)
    }
}
