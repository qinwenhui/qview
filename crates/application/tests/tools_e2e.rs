//! 端到端测试：把 8 个工具注册到 ToolRegistry，逐一调用，验证：
//! - 输入校验
//! - 输出结构
//! - 白名单过滤
//! - 副作用分级
//! - 文档作用域（未知 DocumentId 报错）

use std::sync::Arc;

use serde_json::json;

use qview_application::protocol::{PermissionPolicy, SideEffect};
use qview_application::service::annotation::AnnotationService;
use qview_application::service::{DocumentService, SearchService};
use qview_application::tool::ToolRegistry;
use qview_application::tools::{register_defaults, ALL_TOOL_NAMES, ALL_TOOL_NAMES_WITH_WRITES};

fn fixture_log() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("qview-app-tools-{}.log", uuid::Uuid::new_v4()));
    // 写 1000 行 — 一半带 5xx 错误码
    let mut body = String::with_capacity(1000 * 80);
    for i in 0..1000 {
        if i % 5 == 0 {
            body.push_str(&format!("2026-08-06 10:23:{:02} ERROR 5{} req={}\n", i % 60, (i % 9) + 1, i));
        } else if i % 7 == 0 {
            body.push_str(&format!("2026-08-06 10:23:{:02} WARN  slow req={}\n", i % 60, i));
        } else {
            body.push_str(&format!("2026-08-06 10:23:{:02} INFO  ok req={}\n", i % 60, i));
        }
    }
    std::fs::write(&p, body).unwrap();
    p
}

fn make_registry(docs: Arc<DocumentService>, search: Arc<SearchService>) -> ToolRegistry {
    let policy = PermissionPolicy::with_allowlist(ALL_TOOL_NAMES.iter().map(|s| s.to_string()).collect());
    let mut reg = ToolRegistry::new(policy);
    let ann = Arc::new(AnnotationService::new(docs.clone()));
    register_defaults(&mut reg, docs, search, Some(ann), qview_application::tools::SharedViewport::default(), &[]).unwrap();
    reg
}

#[tokio::test]
async fn get_document_info_returns_metadata() {
    let path = fixture_log();
    let docs = Arc::new(DocumentService::default());
    let id = docs.open(path.clone()).unwrap();
    let search = Arc::new(SearchService::new(docs.clone()));
    let reg = make_registry(docs.clone(), search);

    let r = reg
        .call_tool("get_document_info", json!({"document_id": id.get()}))
        .await;
    assert!(!r.is_error);
    let v = r.content.as_object().unwrap();
    assert_eq!(v["total_lines"], 1000);
    assert!(v["is_indexed"].as_bool().unwrap());
    assert!(v["size_bytes"].as_u64().unwrap() > 0);
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn search_text_returns_paginated_hits() {
    let path = fixture_log();
    let docs = Arc::new(DocumentService::default());
    let id = docs.open(path.clone()).unwrap();
    let search = Arc::new(SearchService::new(docs.clone()));
    let reg = make_registry(docs.clone(), search);

    let r = reg
        .call_tool(
            "search_text",
            json!({"document_id": id.get(), "query": "ERROR", "limit": 5}),
        )
        .await;
    assert!(!r.is_error);
    let v = r.content.as_object().unwrap();
    let total = v["total"].as_u64().unwrap();
    // 1000 / 5 = 200 条 ERROR
    assert_eq!(total, 200);
    let returned = v["returned"].as_u64().unwrap();
    assert_eq!(returned, 5);
    let truncated = v["truncated"].as_bool().unwrap();
    assert!(truncated);
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn concurrent_search_text_are_isolated() {
    // 回归测试：并发多个 search_text 必须各自独立、结果正确。
    // 旧实现共用一个引擎搜索槽，并发搜索会互相覆盖 → 假空结果 / 误报失败。
    let path = fixture_log();
    let docs = Arc::new(DocumentService::default());
    let id = docs.open(path.clone()).unwrap();
    let search = Arc::new(SearchService::new(docs.clone()));
    let reg = Arc::new(make_registry(docs.clone(), search));

    let ra = reg.clone();
    let rb = reg.clone();
    let rc = reg.clone();
    let a = tokio::spawn(async move {
        ra.call_tool("search_text", json!({"document_id": id.get(), "query": "ERROR", "limit": 3}))
            .await
    });
    let b = tokio::spawn(async move {
        rb.call_tool("search_text", json!({"document_id": id.get(), "query": "WARN", "limit": 3}))
            .await
    });
    let c = tokio::spawn(async move {
        rc.call_tool("search_text", json!({"document_id": id.get(), "query": "NEVER_PRESENT", "limit": 3}))
            .await
    });
    let (a, b, c) = (a.await.unwrap(), b.await.unwrap(), c.await.unwrap());

    assert!(!a.is_error, "ERROR search: {:?}", a.content);
    assert!(!b.is_error, "WARN search: {:?}", b.content);
    assert!(!c.is_error, "NEVER_PRESENT search: {:?}", c.content);
    // fixture：1000 行，ERROR 每 5 行 = 200；WARN 每 7 行且非 5 的倍数 = 114。
    assert_eq!(a.content["total"], 200);
    assert_eq!(b.content["total"], 114);
    assert_eq!(c.content["total"], 0);
    // 每个结果的命中文本必须属于自己那个词（没有被别的搜索覆盖）。
    let a_hit = a.content["hits"][0]["text"].as_str().unwrap();
    let b_hit = b.content["hits"][0]["text"].as_str().unwrap();
    assert!(a_hit.contains("ERROR"));
    assert!(b_hit.contains("WARN"));
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn read_context_returns_before_after_window() {
    let path = fixture_log();
    let docs = Arc::new(DocumentService::default());
    let id = docs.open(path.clone()).unwrap();
    let search = Arc::new(SearchService::new(docs.clone()));
    let reg = make_registry(docs.clone(), search);

    let r = reg
        .call_tool(
            "read_context",
            json!({"document_id": id.get(), "line": 100, "before": 3, "after": 3}),
        )
        .await;
    assert!(!r.is_error);
    let v = r.content.as_object().unwrap();
    let lines = v["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 7); // before=3 + center + after=3
    let range = v["line_range"].as_array().unwrap();
    assert_eq!(range[0].as_u64().unwrap(), 97);
    assert_eq!(range[1].as_u64().unwrap(), 104);
    // 每行都带 byte_start / byte_end 字节偏移（供 annotate_create 用），且区间单调不减。
    let mut prev_end = 0u64;
    for (i, l) in lines.iter().enumerate() {
        assert_eq!(l["line"].as_u64().unwrap(), 97 + i as u64);
        let bs = l["byte_start"].as_u64().expect("byte_start present");
        let be = l["byte_end"].as_u64().expect("byte_end present");
        assert!(bs >= prev_end, "byte offsets must be monotonic");
        assert!(be >= bs);
        prev_end = be;
    }
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn read_context_rejects_unknown_document() {
    let path = fixture_log();
    let docs = Arc::new(DocumentService::default());
    let search = Arc::new(SearchService::new(docs.clone()));
    let reg = make_registry(docs, search);

    let r = reg
        .call_tool(
            "read_context",
            json!({"document_id": 9999, "line": 10}),
        )
        .await;
    assert!(r.is_error);
    assert_eq!(r.content["error"], "unknown_document");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn inspect_matches_returns_distribution() {
    let path = fixture_log();
    let docs = Arc::new(DocumentService::default());
    let id = docs.open(path.clone()).unwrap();
    let search = Arc::new(SearchService::new(docs.clone()));
    let reg = make_registry(docs.clone(), search);

    let r = reg
        .call_tool(
            "inspect_matches",
            json!({"document_id": id.get(), "query": "ERROR", "buckets": 4, "sample_size": 3}),
        )
        .await;
    assert!(!r.is_error);
    let v = r.content.as_object().unwrap();
    assert_eq!(v["total"], 200);
    let dist = v["line_distribution"].as_array().unwrap();
    assert_eq!(dist.len(), 4);
    let sample = v["sample"].as_array().unwrap();
    assert_eq!(sample.len(), 3);
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn summarize_range_truncates_by_max_tokens() {
    let path = fixture_log();
    let docs = Arc::new(DocumentService::default());
    let id = docs.open(path.clone()).unwrap();
    let search = Arc::new(SearchService::new(docs.clone()));
    let reg = make_registry(docs.clone(), search);

    let r = reg
        .call_tool(
            "summarize_range",
            json!({"document_id": id.get(), "start": 0, "end": 500, "max_tokens": 50}),
        )
        .await;
    assert!(!r.is_error);
    let v = r.content.as_object().unwrap();
    let used = v["used_chars"].as_u64().unwrap();
    // 50 tokens × 4 chars = 200 chars 上限
    assert!(used <= 200);
    assert!(v["truncated"].as_bool().unwrap());
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn view_tools_emit_view_intents() {
    let path = fixture_log();
    let docs = Arc::new(DocumentService::default());
    let id = docs.open(path.clone()).unwrap();
    let search = Arc::new(SearchService::new(docs.clone()));
    let reg = make_registry(docs.clone(), search);

    let r = reg
        .call_tool("navigate_to_line", json!({"line": 42}))
        .await;
    assert!(!r.is_error);
    let intents = r.content["view_intents"].as_array().unwrap();
    assert_eq!(intents.len(), 1);
    assert_eq!(intents[0]["intent"], "focus_line");
    assert_eq!(intents[0]["line"], 42);

    let r = reg
        .call_tool(
            "highlight_range",
            json!({"start": 0, "end": 10, "kind": "agent_match"}),
        )
        .await;
    assert!(!r.is_error);
    let intents = r.content["view_intents"].as_array().unwrap();
    assert_eq!(intents[0]["intent"], "highlight_range");
    assert_eq!(intents[0]["kind"], "agent_match");

    let r = reg
        .call_tool(
            "create_filter",
            json!({"type": "error_level", "min": 500, "max": 599}),
        )
        .await;
    assert!(!r.is_error);
    let intents = r.content["view_intents"].as_array().unwrap();
    assert_eq!(intents[0]["intent"], "apply_filter");
    assert_eq!(intents[0]["filter"]["type"], "error_level");

    // 引用一下 id 抑制 unused 警告
    let _ = id;
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn allowlist_blocks_unauthorized_tool() {
    let path = fixture_log();
    let docs = Arc::new(DocumentService::default());
    docs.open(path.clone()).unwrap(); // 至少打开一次（后续只验证权限）
    let search = Arc::new(SearchService::new(docs.clone()));

    // 白名单只放 2 个工具
    let mut reg = ToolRegistry::new(PermissionPolicy::with_allowlist(vec![
        "get_document_info".into(),
        "navigate_to_line".into(),
    ]));
    register_defaults(&mut reg, docs, search, None, qview_application::tools::SharedViewport::default(), &[]).unwrap();

    let r = reg
        .call_tool("search_text", json!({"document_id": 1, "query": "x"}))
        .await;
    assert!(r.is_error);
    assert_eq!(r.content["error"], "tool_not_allowed");

    let r = reg.call_tool("navigate_to_line", json!({"line": 1})).await;
    assert!(!r.is_error);
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn metadata_carries_side_effect() {
    let path = fixture_log();
    let docs = Arc::new(DocumentService::default());
    let search = Arc::new(SearchService::new(docs.clone()));
    let reg = make_registry(docs.clone(), search);

    // search_text 应是 ReadOnly
    let meta = reg.metadata_of("search_text").expect("registered");
    assert_eq!(meta.side_effect, SideEffect::ReadOnly);
    assert_eq!(meta.group.as_str(), "search");
    assert!(!meta.summary.is_empty());

    // navigate_to_line 应是 ViewOnly
    let meta = reg.metadata_of("navigate_to_line").expect("registered");
    assert_eq!(meta.side_effect, SideEffect::ViewOnly);

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn worker_finish_passes_through_allowlist() {
    // 即便 allowlist 为空，worker_finish 也允许（架构 §11.1）
    let docs = Arc::new(DocumentService::default());
    let search = Arc::new(SearchService::new(docs.clone()));
    let mut reg = ToolRegistry::new(PermissionPolicy::with_allowlist(vec![]));
    register_defaults(&mut reg, docs, search, None, qview_application::tools::SharedViewport::default(), &[]).unwrap();

    let r = reg
        .call_tool(
            contexa_context::FINISH_TOOL_NAME,
            json!({"status": "success", "result": null}),
        )
        .await;
    // 由于 worker_finish 没注册为 LocalTool，registry 找不到 → ToolNotFound
    // （架构约定 worker_finish 由 contexa::effective_tools 末尾追加，不进入 qview registry）
    // 这里只验证：不返回 tool_not_allowed
    assert_ne!(r.content["error"], "tool_not_allowed");
}

#[tokio::test]
async fn redaction_replaces_in_tool_output() {
    let path = fixture_log();
    let docs = Arc::new(DocumentService::default());
    let id = docs.open(path.clone()).unwrap();
    let search = Arc::new(SearchService::new(docs.clone()));
    let mut policy = PermissionPolicy::with_allowlist(ALL_TOOL_NAMES.iter().map(|s| s.to_string()).collect());
    // 屏蔽所有 4 位以上连续数字
    policy.redact_patterns = vec![r"\d{4,}".into()];
    let mut reg = ToolRegistry::new(policy);
    register_defaults(&mut reg, docs.clone(), search, None, qview_application::tools::SharedViewport::default(), &[]).unwrap();

    // get_document_info 输出包含 size_bytes（≥ 4 位数字）— 应被屏蔽
    let r = reg
        .call_tool("get_document_info", json!({"document_id": id.get()}))
        .await;
    assert!(!r.is_error);
    let size = r.content["size_bytes"].as_u64();
    assert!(size.is_none(), "数字字段应被脱敏为字符串，实际: {size:?}");
    // 路径里如果有连续 4 位以上数字也应被屏蔽
    let path_str = r.content["path"].as_str().unwrap();
    for token in path_str.split(|c: char| !c.is_ascii_digit()) {
        if token.len() >= 4 {
            panic!("路径里残留 ≥4 位连续数字: {token:?} in {path_str:?}");
        }
    }
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn list_directory_lists_entries_and_recurses() {
    let root = temp_dir_tree();
    let docs = Arc::new(DocumentService::default());
    let search = Arc::new(SearchService::new(docs.clone()));
    let reg = make_registry(docs.clone(), search);

    // depth=1：只看直接子项
    let r = reg
        .call_tool("list_directory", json!({"path": root.to_string_lossy()}))
        .await;
    assert!(!r.is_error, "list_directory err: {r:?}");
    let v = r.content.as_object().unwrap();
    assert_eq!(v["truncated"], false);
    let names: Vec<&str> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"a.log"));
    assert!(names.contains(&"sub"));
    assert!(!names.contains(&"b.log")); // depth=1 不进子目录

    // depth=2：递归进子目录
    let r = reg
        .call_tool(
            "list_directory",
            json!({"path": root.to_string_lossy(), "depth": 2}),
        )
        .await;
    assert!(!r.is_error);
    let names: Vec<&str> = r.content["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"b.log"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn list_directory_filters_pattern_type_and_paginates() {
    let root = temp_dir_tree();
    std::fs::write(root.join("a.txt"), b"txt").unwrap();
    std::fs::write(root.join("c.log"), b"hello").unwrap();
    let docs = Arc::new(DocumentService::default());
    let search = Arc::new(SearchService::new(docs.clone()));
    let reg = make_registry(docs.clone(), search);

    // pattern 过滤：只列 .log
    let r = reg
        .call_tool("list_directory", json!({"path": root.to_string_lossy(), "pattern": "*.log"}))
        .await;
    assert!(!r.is_error, "err: {r:?}");
    let names: Vec<&str> = r.content["entries"]
        .as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"a.log") && names.contains(&"c.log"));
    assert!(!names.contains(&"a.txt") && !names.contains(&"sub"));

    // type=dir：只列目录
    let r = reg
        .call_tool("list_directory", json!({"path": root.to_string_lossy(), "type": "dir"}))
        .await;
    assert!(!r.is_error);
    let names: Vec<&str> = r.content["entries"]
        .as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["sub"]);

    // 分页：offset 翻页 + total 精确
    let r = reg
        .call_tool("list_directory", json!({"path": root.to_string_lossy(), "limit": 2, "offset": 2}))
        .await;
    assert!(!r.is_error);
    let v = r.content.as_object().unwrap();
    assert_eq!(v["total"], 4); // a.log, a.txt, c.log, sub
    assert_eq!(v["count"], 2);
    assert_eq!(v["truncated"], false);

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn list_directory_rejects_blacklisted_system_dir() {
    let docs = Arc::new(DocumentService::default());
    let search = Arc::new(SearchService::new(docs.clone()));
    let reg = make_registry(docs.clone(), search);

    // 用各平台真实存在的系统目录验证 path_blocked
    let blocked = if cfg!(windows) { r"C:\Windows".to_string() } else { "/etc".to_string() };
    let r = reg.call_tool("list_directory", json!({"path": blocked})).await;
    assert!(r.is_error);
    assert_eq!(r.content["error"], "path_blocked");
}

#[tokio::test]
async fn open_document_rejects_blacklisted_path() {
    let docs = Arc::new(DocumentService::default());
    let search = Arc::new(SearchService::new(docs.clone()));
    let reg = make_registry(docs.clone(), search);

    // 跨平台构造黑名单命中路径：C:\Windows 是 Windows 系统目录规则；
    // 在 Unix 上该字符串不存在 → canonicalize 失败退回原路径 → 分段匹配仍命中（大小写不敏感）。
    let r = reg
        .call_tool("open_document", json!({"path": r"C:\Windows\system.ini"}))
        .await;
    assert!(r.is_error);
    assert_eq!(r.content["error"], "path_blocked");
    assert!(r.content["rule"].as_str().unwrap().contains("Windows"));
}

#[tokio::test]
async fn write_document_rejects_blacklisted_path() {
    let docs = Arc::new(DocumentService::default());
    let search = Arc::new(SearchService::new(docs.clone()));
    // write_document 不在 ALL_TOOL_NAMES（写工具需允许列表显式放行）→ 单独建注册表
    let ann = Arc::new(AnnotationService::new(docs.clone()));
    let mut reg = ToolRegistry::new(PermissionPolicy::with_allowlist(
        ALL_TOOL_NAMES_WITH_WRITES.iter().map(|s| s.to_string()).collect(),
    ));
    register_defaults(
        &mut reg,
        docs.clone(),
        search,
        Some(ann),
        qview_application::tools::SharedViewport::default(),
        &[],
    )
    .unwrap();

    let r = reg
        .call_tool(
            "write_document",
            json!({"path": r"C:\Windows\system.ini", "text": "evil"}),
        )
        .await;
    assert!(r.is_error);
    // path_blocked 在 std::fs::write 之前拦截 → 系统路径绝不会被写入
    assert_eq!(r.content["error"], "path_blocked");
}

#[tokio::test]
async fn export_report_writes_full_content_into_markdown() {
    // 回归：第一次导出报告"太简单"是因为工具没有 content 字段，模型只能传元数据。
    // 现在 content 参数必须落进 markdown 正文；缺省 content 时退回元数据骨架。
    let path = fixture_log();
    let docs = Arc::new(DocumentService::default());
    let _id = docs.open(path.clone()).unwrap();
    let search = Arc::new(SearchService::new(docs.clone()));
    let ann = Arc::new(AnnotationService::new(docs.clone()));
    let mut reg = ToolRegistry::new(PermissionPolicy::with_allowlist(
        ALL_TOOL_NAMES_WITH_WRITES.iter().map(|s| s.to_string()).collect(),
    ));
    register_defaults(
        &mut reg,
        docs.clone(),
        search,
        Some(ann),
        qview_application::tools::SharedViewport::default(),
        &[],
    )
    .unwrap();

    let name = format!("qview-app-report-{}", uuid::Uuid::new_v4());
    let full_body = "## 完整分析\n\n这里是我的详细分析正文，包含结论、数据与建议。";
    let r = reg
        .call_tool(
            "export_report",
            json!({
                "name": name,
                "format": "markdown",
                "content": full_body,
                "extra": {"document": path.to_string_lossy().to_string()},
            }),
        )
        .await;
    assert!(!r.is_error, "export_report failed: {:?}", r.content);
    let out_path = r.content["path"].as_str().unwrap().to_string();
    let body = std::fs::read_to_string(&out_path).unwrap();
    assert!(body.contains("## 完整分析"), "正文缺失:\n{body}");
    assert!(body.contains("这里是我的详细分析正文"), "正文内容缺失:\n{body}");
    assert!(body.contains("document"), "元数据缺失:\n{body}");

    // 缺省 content → 退回骨架（不崩，也不含正文）
    let r2 = reg
        .call_tool("export_report", json!({"name": format!("{name}-skeleton")}))
        .await;
    assert!(!r2.is_error, "skeleton export failed: {:?}", r2.content);
    let sk_path = r2.content["path"].as_str().unwrap().to_string();
    let sk = std::fs::read_to_string(&sk_path).unwrap();
    assert!(!sk.contains("完整分析"), "骨架不应含正文:\n{sk}");

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&out_path);
    let _ = std::fs::remove_file(&sk_path);
}

#[tokio::test]
async fn system_info_returns_rich_payload() {
    let docs = Arc::new(DocumentService::default());
    let search = Arc::new(SearchService::new(docs.clone()));
    let reg = make_registry(docs.clone(), search);

    // scope 缺省（all）：应包含 os / memory / cpu / disk / network 五块
    let r = reg.call_tool("system_info", json!({})).await;
    assert!(!r.is_error, "system_info err: {r:?}");
    let v = r.content.as_object().unwrap();
    let os = v["os"].as_object().unwrap();
    assert!(os["os_type"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(os["os_version"].as_str().is_some_and(|s| !s.is_empty()));

    let mem = v["memory"].as_object().unwrap();
    assert!(mem["total_bytes"].as_u64().unwrap() > 0);

    let cpu = v["cpu"].as_object().unwrap();
    assert!(cpu["logical_cores"].as_u64().unwrap() >= 1);
    assert!(
        cpu["physical_cores"].as_u64().is_some_and(|p| p >= 1),
        "physical_cores 应 ≥1，实际: {:?}",
        cpu["physical_cores"]
    );

    let disk = v["disk"].as_object().unwrap();
    let mounts = disk["mounts"].as_array().unwrap();
    assert!(!mounts.is_empty());
    assert!(mounts.iter().any(|m| m["total_bytes"].as_u64().unwrap() > 0));

    // scope=os：应只含 os，不含 memory
    let r = reg.call_tool("system_info", json!({"scope": "os"})).await;
    assert!(!r.is_error, "scope=os err: {r:?}");
    let v = r.content.as_object().unwrap();
    assert!(v.contains_key("os"));
    assert!(!v.contains_key("memory"));

    // 非法 scope：返回错误
    let r = reg.call_tool("system_info", json!({"scope": "bogus"})).await;
    assert!(r.is_error);
    assert_eq!(r.content["error"], "invalid_scope");
}

fn temp_dir_tree() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("qview-app-ls-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.log"), b"hello").unwrap();
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("b.log"), b"world").unwrap();
    root
}

// 注册表要求：每个工具都有 ToolMetadata；保证 ALL_TOOL_NAMES 与实际注册的元数据一致。
#[test]
fn all_tool_names_are_registered_with_metadata() {
    let docs = Arc::new(DocumentService::default());
    let search = Arc::new(SearchService::new(docs.clone()));
    let reg = make_registry(docs, search);
    for name in ALL_TOOL_NAMES {
        let meta = reg
            .metadata_of(name)
            .unwrap_or_else(|| panic!("missing metadata for {name}"));
        // 每个 name 都应与 ToolMetadata.name 一致
        assert_eq!(meta.name, *name);
        // 每个 metadata 的 summary 不应为空
        assert!(!meta.summary.is_empty(), "{name} summary 为空");
    }
}
