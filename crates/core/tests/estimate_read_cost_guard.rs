//! Regression test for the deep-read guard during background indexing:
//! `Engine::estimate_read_cost_bytes` returns an estimate while the index is
//! incomplete (so tools can refuse to linear-scan deep lines) and `None` once
//! the index is complete (fast sparse-index path).

use std::path::PathBuf;

use qview_core::config::EngineConfig;
use qview_core::engine::Engine;

const GUARD_THRESHOLD: u64 = 32 * 1024 * 1024; // matches MAX_INDEXING_SCAN_BYTES

fn build_large_file() -> PathBuf {
    // ~40 MB of short lines, so a "deep line" (line_no * 80) exceeds the
    // 32 MiB guard threshold while the file stays cheap to generate.
    let mut p = std::env::temp_dir();
    p.push(format!("qview_est_guard_{}.log", std::process::id()));
    let line = b"xxxxxxxxxx\n"; // 11 bytes per line
    let mut content = Vec::with_capacity(40 * 1024 * 1024);
    for _ in 0..(40 * 1024 * 1024 / line.len()) {
        content.extend_from_slice(line);
    }
    std::fs::write(&p, &content).unwrap();
    p
}

fn open_incomplete(path: &PathBuf) -> (Engine, PathBuf) {
    let index_dir = std::env::temp_dir().join(format!(
        "qview_est_guard_idx_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&index_dir).unwrap();
    let cfg = EngineConfig {
        // 0 → any non-empty file is treated as "large", so a cache-miss leaves
        // the index incomplete (background build not yet run).
        small_file_threshold: 0,
        index_cache_enabled: true,
        index_dir: Some(index_dir.clone()),
        ..EngineConfig::default()
    };
    let engine = Engine::with_config(path.clone(), cfg).unwrap();
    (engine, index_dir)
}

#[test]
fn estimate_returns_some_while_building_and_none_when_complete() {
    let path = build_large_file();
    let (mut engine, index_dir) = open_incomplete(&path);

    // 索引未完成：深行估算代价应超过护栏阈值，浅行应低于阈值。
    assert!(!engine.index.is_complete());
    let deep = engine.estimate_read_cost_bytes(10_000_000).unwrap();
    assert!(
        deep > GUARD_THRESHOLD,
        "深行估算代价 {} 应超过护栏阈值 {GUARD_THRESHOLD}",
        deep
    );
    let shallow = engine.estimate_read_cost_bytes(1).unwrap();
    assert!(shallow < GUARD_THRESHOLD);

    // 建完索引后：走稀疏索引快路径，无需估算 → None。
    engine.build_index_blocking().unwrap();
    assert!(engine.index.is_complete());
    assert!(
        engine.estimate_read_cost_bytes(10_000_000).is_none(),
        "索引完成后不应再做线性扫描代价估算"
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(&index_dir);
}
