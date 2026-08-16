//! Single-pass adaptive search sampling: sparse results store every hit,
//! dense results sample at `sample_interval` with an exact total, and
//! navigation stays exact across both.

use qview_core::config::SearchConfig;
use qview_core::file::MmapBackend;
use qview_core::search::{parse_query, run_search, SearchOptions};

fn write_file(label: &str, contents: &str) -> (MmapBackend, std::path::PathBuf) {
    let mut path = std::env::temp_dir();
    path.push(format!("qview_search_samp_{}_{}.txt", std::process::id(), label));
    std::fs::write(&path, contents).unwrap();
    (MmapBackend::open(&path).unwrap(), path)
}

/// Few hits → every hit stored (interval == 1), navigation is direct.
#[test]
fn sparse_results_store_every_hit() {
    let (mmap, path) = write_file("sparse", "aaa\nbbb\naaa\nccc\naaa\naaa\n");
    let q = parse_query("aaa", &SearchOptions::default()).unwrap();
    let cfg = SearchConfig { sample_interval: 100, max_samples: 10_000_000 };
    let idx = run_search(&q, &mmap, &cfg, qview_core::file::SCAN_WINDOW).unwrap();

    assert_eq!(idx.total_count(), 4, "exact total");
    assert_eq!(idx.sample_interval(), 1, "sparse → interval 1");
    assert_eq!(idx.stored_count(), 4);
    // Every hit navigable.
    for n in 0..4 {
        assert!(idx.get(n).is_some(), "hit {n}");
    }
    let _ = std::fs::remove_file(&path);
}

/// Many hits with a small cap → dense mode: every `interval`-th hit stored,
/// exact total, navigation still lands on the right hit.
#[test]
fn dense_results_sample_interval_and_navigate_exactly() {
    let contents: String = (0..1000).map(|i| format!("needle{}\n", i)).collect();
    let (mmap, path) = write_file("dense", &contents);
    let q = parse_query("needle", &SearchOptions::default()).unwrap();
    let cfg = SearchConfig { sample_interval: 100, max_samples: 10 };
    let idx = run_search(&q, &mmap, &cfg, qview_core::file::SCAN_WINDOW).unwrap();

    assert_eq!(idx.total_count(), 1000, "exact total");
    assert_eq!(idx.sample_interval(), 100, "dense → interval 100");
    // Samples at hits 0, 100, 200, …, 900 → 10 samples (cap 10).
    assert_eq!(idx.stored_count(), 10);
    for n in [0usize, 1, 50, 99, 100, 500, 999] {
        let pos = idx.get(n).expect("hit found");
        let line = mmap.read_line(pos).0;
        assert_eq!(&line[..6], b"needle", "hit {n} landed on wrong line: {line:?}");
    }
    let _ = std::fs::remove_file(&path);
}

/// The sample buffer never exceeds `max_samples` even when the file has far
/// more hits (bounded memory regardless of total count).
#[test]
fn sample_buffer_respects_cap() {
    let contents: String = (0..50_000).map(|i| format!("x{}\n", i)).collect();
    let (mmap, path) = write_file("cap", &contents);
    let q = parse_query("x", &SearchOptions::default()).unwrap();
    let cfg = SearchConfig { sample_interval: 7, max_samples: 2000 };
    let idx = run_search(&q, &mmap, &cfg, qview_core::file::SCAN_WINDOW).unwrap();

    assert_eq!(idx.total_count(), 50_000);
    assert!(idx.stored_count() <= 2000, "samples capped at max_samples");
    assert!(idx.stored_count() > 0);
    // First sample must be hit #0 (samples[k] = hit #k*interval).
    let first = idx.get(0).unwrap();
    assert_eq!(&mmap.as_slice()[first as usize..first as usize + 1], b"x");
    let _ = std::fs::remove_file(&path);
}
