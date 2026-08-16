//! 目录扫描工具：`list_directory`。
//!
//! 让器灵能看到磁盘上的目录结构（用户要求「让器灵也能看到目录」），并能在
//! **超多文件的目录**里灵活查找：
//! - `pattern`：glob 名称过滤（`*` / `?`，大小写不敏感），如 `"*.log"` / `"error*"` / `"*test*"`；
//! - `type`：只列文件或只列目录；
//! - `offset` / `limit`：分页，配合 `total` 让器灵知道还剩多少、继续翻页；
//! - `sort`：按名称（默认）/ 大小 / 修改时间排序。
//!
//! 安全约束：
//! - 目标目录命中系统目录黑名单 → 返回 `path_blocked`；
//! - 递归枚举时跳过黑名单子目录（`blocked_skipped` 计数）；
//! - 递归深度上限 3、单次返回上限 500；收集上限 `COLLECT_CAP`（防百万级文件撑爆内存，
//!   命中后 `total_capped=true`，器灵应改用 pattern 缩小范围）；
//! - 权限不足的子目录静默跳过（不把错误当成失败）。

use std::sync::Arc;

use futures::future::FutureExt;
use serde_json::{json, Value};

use contexa_tools::{boxed_invoke, LocalTool, ToolResult};

use crate::protocol::SideEffect;
use crate::service::document::DocumentService;
use crate::tool::metadata::{ToolGroup, ToolMetadata};

/// 递归深度上限（防 agent 一次扫爆目录树）。
const MAX_DEPTH: u32 = 3;
/// 单次返回条目上限。
const MAX_LIMIT: usize = 500;
/// 收集上限：命中即停（内存保护），返回 `total_capped=true`。
const COLLECT_CAP: usize = 200_000;

/// 用户主目录（macOS/Linux `$HOME`，Windows `%USERPROFILE%`）。
fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
}

/// 解析用户给的目录路径（尽力而为，避免"口语相对路径找不到"的硬失败）：
///
/// 1. 展开 `~` / `~/...` 到用户主目录；
/// 2. `canonicalize` 原始路径（相对路径 = 相对当前工作目录）；
/// 3. 相对路径仍失败时，依次在「当前工作目录 / 用户主目录 / `~/Projects`」下拼接兜底。
///
/// 返回已 `canonicalize` 且确认是**目录**的绝对路径。
/// 失败返回 `Err(友好错误描述)`（区分"不存在"与"存在但不是目录"）。
///
/// 背景：路由分类器可能把用户口语的 "Projects/bench_data" 原样塞进 `path`，
/// 不兜底的话直接 `not_found`（用户实际反馈：AI 一路被卡在相对路径找不到上）。
fn resolve_dir_path(path: &str) -> Result<std::path::PathBuf, String> {
    // 1) 展开 `~`
    let expanded = if path == "~" {
        home_dir().ok_or_else(|| "无法确定用户主目录".to_string())?
    } else if let Some(rest) = path.strip_prefix("~/") {
        home_dir()
            .ok_or_else(|| "无法确定用户主目录".to_string())?
            .join(rest)
    } else if let Some(rest) = path.strip_prefix("~\\") {
        home_dir()
            .ok_or_else(|| "无法确定用户主目录".to_string())?
            .join(rest)
    } else {
        std::path::PathBuf::from(path)
    };

    // 2) 直接 canonicalize（相对路径 = 相对当前工作目录）
    if let Ok(c) = std::fs::canonicalize(&expanded) {
        if c.is_dir() {
            return Ok(c);
        }
        return Err(format!("{} 存在但不是目录", c.display()));
    }

    // 绝对路径已经失败 → 不再拼接（避免在根目录下瞎拼）
    if expanded.is_absolute() {
        return Err(format!("目录不存在：{path}（已按绝对路径解析）"));
    }

    // 3) 相对路径 → 在常见根下拼接兜底
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    if let Some(home) = home_dir() {
        roots.push(home.join("Projects"));
        roots.push(home);
    }
    for root in roots {
        let candidate = root.join(&expanded);
        if let Ok(c) = std::fs::canonicalize(&candidate) {
            if c.is_dir() {
                return Ok(c);
            }
        }
    }
    Err(format!(
        "目录不存在：{path}（已相对当前工作目录、用户主目录及 ~/Projects 兜底尝试）"
    ))
}

#[derive(Clone, Copy, PartialEq)]
enum TypeFilter {
    Any,
    File,
    Dir,
}

#[derive(Clone, Copy, PartialEq)]
enum SortKey {
    Name,
    Size,
    Time,
}

/// 收集阶段用轻量结构（避免先建 json 再排序 / 分页的反复分配）。
struct Entry {
    name: String,
    path: String,
    kind: &'static str,
    size: u64,
    modified_ms: u64,
}

pub fn list_directory_metadata() -> ToolMetadata {
    ToolMetadata::new(
        "list_directory",
        "列出目录下的文件 / 子目录：支持 glob 名称过滤（pattern）、类型过滤（type）、分页（offset/limit）、排序（sort）；受系统目录黑名单约束",
        SideEffect::ReadOnly,
        ToolGroup::Document,
    )
}

pub fn list_directory_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "要列出的目录绝对路径"},
            "depth": {"type": "integer", "description": "递归深度（1=只列直接子项，默认 1，最大 3）"},
            "pattern": {"type": "string", "description": "名称 glob 过滤（大小写不敏感）：\"*.log\" / \"error*\" / \"*test*\"；不填=全部"},
            "type": {"type": "string", "enum": ["file", "dir"], "description": "只列文件（file）或只列目录（dir）；不填=全部"},
            "offset": {"type": "integer", "description": "跳过前 N 个匹配（分页用，默认 0）"},
            "limit": {"type": "integer", "description": "最多返回条目数（默认 100，最大 500）"},
            "sort": {"type": "string", "enum": ["name", "size", "time"], "description": "排序：name=名称升序（默认），size=大小降序，time=修改时间降序"}
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

pub fn list_directory_tool(docs: Arc<DocumentService>) -> anyhow::Result<LocalTool> {
    let tool = LocalTool::from_async_fn(
        "list_directory",
        "列出目录下的文件 / 子目录：支持 glob 名称过滤（pattern）、类型过滤（type）、分页（offset/limit）、排序（sort）；受系统目录黑名单约束",
        list_directory_parameters(),
        boxed_invoke(move |args| {
            let docs = docs.clone();
            async move {
                let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
                    return Ok(ToolResult::err(json!({"error":"missing_argument","argument":"path"})));
                };
                let depth = args
                    .get("depth")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1)
                    .clamp(1, MAX_DEPTH as u64) as u32;
                let pattern = args
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_lowercase());
                let type_filter = match args.get("type").and_then(|v| v.as_str()) {
                    Some("file") => TypeFilter::File,
                    Some("dir") => TypeFilter::Dir,
                    _ => TypeFilter::Any,
                };
                let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(100)
                    .clamp(1, MAX_LIMIT as u64) as usize;
                let sort = match args.get("sort").and_then(|v| v.as_str()) {
                    Some("size") => SortKey::Size,
                    Some("time") => SortKey::Time,
                    _ => SortKey::Name,
                };

                let canonical = match resolve_dir_path(path) {
                    Ok(c) => c,
                    Err(message) => {
                        return Ok(ToolResult::err(json!({
                            "error": "not_found",
                            "path": path,
                            "message": format!(
                                "{message}。请提供绝对路径（如 /Users/用户名/Projects/qview），\
                                 或用 ~ 开头（~ 会展开为用户主目录）。"
                            ),
                        })));
                    }
                };
                if let Some(rule) = docs.is_blocked(&canonical) {
                    return Ok(ToolResult::err(json!({
                        "error": "path_blocked",
                        "path": canonical.display().to_string(),
                        "rule": rule,
                        "message": format!("系统目录黑名单：{}（命中规则 {rule}），器灵不允许列出", canonical.display()),
                    })));
                }

                let mut all: Vec<Entry> = Vec::new();
                let mut blocked_skipped = 0usize;
                let mut capped = false;
                collect(
                    &canonical,
                    depth,
                    pattern.as_deref(),
                    type_filter,
                    &mut all,
                    &mut blocked_skipped,
                    &docs,
                    &mut capped,
                );

                // 排序（稳定：同键时再按名称，保证分页顺序确定）
                match sort {
                    SortKey::Name => all.sort_by(|a, b| a.name.cmp(&b.name)),
                    SortKey::Size => all.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name))),
                    SortKey::Time => all.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms).then_with(|| a.name.cmp(&b.name))),
                }

                let total = all.len();
                let page: Vec<Value> = all
                    .into_iter()
                    .skip(offset)
                    .take(limit)
                    .map(|e| {
                        json!({
                            "name": e.name,
                            "kind": e.kind,
                            "size": e.size,
                            "modified": e.modified_ms,
                            "path": e.path,
                        })
                    })
                    .collect();
                let truncated = offset + page.len() < total;

                Ok(ToolResult::ok(json!({
                    "path": canonical.display().to_string(),
                    "entries": page,
                    "count": page.len(),
                    "total": total,
                    "total_capped": capped,
                    "truncated": truncated,
                    "blocked_skipped": blocked_skipped,
                })))
            }
            .boxed()
        }),
    )?;
    Ok(tool)
}

/// 递归收集匹配条目。命中黑名单的子路径整体跳过；权限错误静默忽略。
///
/// `pattern` / `type_filter` 只决定「是否把这条放进返回列表」，**不影响是否递归**
/// 进子目录（否则 `*.log` 会漏掉不匹配名字的子目录里的日志文件）。
fn collect(
    dir: &std::path::Path,
    depth: u32,
    pattern: Option<&str>,
    type_filter: TypeFilter,
    out: &mut Vec<Entry>,
    blocked_skipped: &mut usize,
    docs: &DocumentService,
    capped: &mut bool,
) {
    if *capped {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        if docs.is_blocked(&p).is_some() {
            *blocked_skipped += 1;
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let ft = e.file_type().ok();
        let is_dir = ft.as_ref().map(|t| t.is_dir()).unwrap_or(false);
        let is_symlink = ft.as_ref().map(|t| t.is_symlink()).unwrap_or(false);
        let kind = if is_dir {
            "dir"
        } else if is_symlink {
            "symlink"
        } else {
            "file"
        };

        // 名称过滤（glob，已转小写）+ 类型过滤：只影响「是否返回这条」
        let name_ok = match pattern {
            Some(pat) => glob_match(pat, &name.to_lowercase()),
            None => true,
        };
        let type_ok = match type_filter {
            TypeFilter::Any => true,
            TypeFilter::File => !is_dir && !is_symlink,
            TypeFilter::Dir => is_dir,
        };
        if name_ok && type_ok {
            let md = e.metadata().ok();
            let size = md.as_ref().map(|m| m.len()).unwrap_or(0);
            let modified_ms = md
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            out.push(Entry {
                name,
                path: p.to_string_lossy().into_owned(),
                kind,
                size,
                modified_ms,
            });
            if out.len() >= COLLECT_CAP {
                *capped = true;
                return;
            }
        }
        // 递归进子目录（不管子目录本身是否命中 pattern/type）
        if is_dir && depth > 1 {
            collect(&p, depth - 1, pattern, type_filter, out, blocked_skipped, docs, capped);
        }
    }
}

/// 简单 glob 匹配（`*` 任意串、`?` 单字符），经典双指针 O(n+m)。
/// 调用方需把 `pattern` 与 `name` 统一转成小写再比。
fn glob_match(pat: &str, name: &str) -> bool {
    let p = pat.as_bytes();
    let s = name.as_bytes();
    let (mut pi, mut si) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while si < s.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = pi;
            mark = si;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 包装：pattern 与 name 都转小写再比（与 collect 内调用一致）。
    fn gm(pat: &str, name: &str) -> bool {
        glob_match(&pat.to_lowercase(), &name.to_lowercase())
    }

    #[test]
    fn glob_matches_common_patterns() {
        assert!(gm("*.log", "a.log"));
        assert!(!gm("*.log", "a.txt"));
        assert!(gm("error*", "error123"));
        assert!(!gm("error*", "err"));
        assert!(gm("*test*", "my_test_file.txt"));
        assert!(gm("*.log", "X.LOG")); // 大小写不敏感（"x.log" 匹配 "*.log"）
        assert!(gm("?", "a"));
        assert!(!gm("?", "ab"));
        assert!(gm("a*b*c", "axxbyyc"));
        assert!(gm("*", "anything"));
        assert!(gm("", ""));
        assert!(!gm("", "a"));
        assert!(!gm("*.log", ""));
    }

    #[test]
    fn glob_star_backtracks_correctly() {
        // `*` 需要回溯才能匹配（经典陷阱用例）
        assert!(gm("*a*a*a*", "aaaa"));
        assert!(gm("*a*a*a*", "baaab"));
        assert!(!gm("*a*a*a*", "bbb"));
        assert!(gm("a*a", "aXa"));
        assert!(gm("a*a", "aa"));
    }
}
