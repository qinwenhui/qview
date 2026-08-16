//! Smoke tests for tokenization and search.

use qview_core::cache::DisplayLine;
use qview::tui::tokenize;

fn dl(s: &str) -> DisplayLine {
    DisplayLine {
        text: s.to_string(),
        matches: vec![],
        truncated_left: false,
        truncated_right: false,
        modified: false,
    }
}

#[test]
fn tokenize_timestamp() {
    let s = "2026-07-31 12:34:56.789 [INFO] hello";
    let spans = tokenize::style_spans(&dl(s));
    assert!(spans.iter().any(|sp| sp.text.starts_with("2026-07-31")));
    // The "[INFO]" should produce a styled span (the inner level word "INFO").
    assert!(spans.iter().any(|sp| sp.text.contains("INFO")));
}

#[test]
fn tokenize_error_red() {
    let s = "2026-07-31 [ERROR] boom";
    let spans = tokenize::style_spans(&dl(s));
    // [ERROR] should be red+bold.
    let err_span = spans.iter().find(|sp| sp.text == "[ERROR]").unwrap();
    let fg = err_span.style.fg.unwrap();
    assert_eq!(format!("{:?}", fg), "Red");
}

#[test]
fn tokenize_ip() {
    // IP not preceded by `key=` so it's detected as a free IP, not a key-value value.
    let s = "client 10.0.0.1 connected";
    let spans = tokenize::style_spans(&dl(s));
    let ip = spans.iter().find(|sp| sp.text == "10.0.0.1").unwrap();
    let fg = ip.style.fg.unwrap();
    assert_eq!(format!("{:?}", fg), "Magenta");
}

#[test]
fn tokenize_uuid() {
    let s = "req=a9eab706-619f-4e2a-9c41-b6c5d2e8f910 ok";
    let spans = tokenize::style_spans(&dl(s));
    assert!(spans.iter().any(|sp| sp.text.starts_with("a9eab706-619f-")));
}

#[test]
fn search_runs() {
    use qview_core::search::run_search;
    use qview_core::search::Query;

    // Build a temp file.
    let path = std::env::temp_dir().join("qview-test.log");
    std::fs::write(&path, b"alpha\nbeta\ngamma alpha\ndelta\nalpha zeta\n").unwrap();

    let mmap = qview_core::file::MmapBackend::open(&path).unwrap();
    let q = Query::Literal(b"alpha".to_vec());
    let hits = run_search(
        &q,
        &mmap,
        &qview_core::config::SearchConfig::default(),
        qview_core::file::SCAN_WINDOW,
    )
    .unwrap();
    assert_eq!(hits.total_count(), 3);

    let _ = std::fs::remove_file(&path);
}