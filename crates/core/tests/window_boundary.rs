//! Correctness across the 64 MiB scan-window boundary: a line / search hit
//! straddling the boundary must resolve exactly, and the sparse index built by
//! the fused windowed pass must match the full offsets.

use qview_core::config::SearchConfig;
use qview_core::engine::Engine;
use qview_core::file::{IndexBuilder, MmapBackend};
use qview_core::search::{parse_query, run_search, SearchOptions};

const WINDOW: usize = 64 * 1024 * 1024;
const BYTES_PER_LINE: usize = 10; // "L12345678\n"

fn build_data(target: usize) -> Vec<u8> {
    let mut data: Vec<u8> = Vec::with_capacity(target + BYTES_PER_LINE);
    let mut seq: u64 = 0;
    while data.len() < target {
        data.extend_from_slice(format!("L{:08}\n", seq).as_bytes());
        seq += 1;
    }
    data
}

/// Sparse index line resolution stays exact across the window boundary.
#[test]
fn sparse_index_resolves_lines_across_window_boundary() {
    let mut path = std::env::temp_dir();
    path.push(format!("qview_boundary_idx_{}.txt", std::process::id()));
    let data = build_data(WINDOW + 2 * 1024 * 1024); // ~66 MiB
    std::fs::write(&path, &data).unwrap();

    let mmap = MmapBackend::open(&path).unwrap();
    let mut engine = Engine::new(path.clone()).unwrap();
    engine.build_index_blocking().expect("sparse index build");
    assert!(engine.index.is_complete());

    // A few lines around the 64 MiB boundary (a line straddles it).
    let boundary_line = (WINDOW / BYTES_PER_LINE) as u64;
    for n in [
        boundary_line - 2,
        boundary_line - 1,
        boundary_line,
        boundary_line + 1,
        boundary_line + 100_000,
    ] {
        let raw = engine.read_line(n);
        let expect = format!("L{:08}", n);
        assert_eq!(raw.text, expect, "line {n} resolved wrong across window boundary");
    }

    // Sparse and full builds agree across the boundary too. (full.len() is
    // one more than total_lines when the file ends in `\n` — the trailing
    // EOF marker — so compare the sampled offsets directly.)
    let builder = IndexBuilder::new(mmap.clone());
    let sparse = builder.build_sparse_with_progress(|_, _| {}).unwrap();
    let full = builder.build_with_progress(|_, _| {}).unwrap();
    assert_eq!(sparse.total_lines, (full.len() - 1) as u64, "lines = newlines (trailing \\n)");
    for (i, &off) in sparse.sparse.iter().enumerate() {
        assert_eq!(off, full[i * 128], "sparse[{i}] mismatch across boundary");
    }

    let _ = std::fs::remove_file(&path);
}

/// Search finds hits straddling the window boundary (overlap mapping) and
/// navigation stays exact.
#[test]
fn search_finds_hits_across_window_boundary() {
    let mut path = std::env::temp_dir();
    path.push(format!("qview_boundary_srch_{}.txt", std::process::id()));
    let data = build_data(WINDOW + 2 * 1024 * 1024); // ~66 MiB
    std::fs::write(&path, &data).unwrap();

    let mmap = MmapBackend::open(&path).unwrap();
    // Every line contains "L" → ~6.9M hits → dense, interval sampling. Large
    // max_samples so the sampled range covers the boundary hit (keeps get()
    // O(interval) instead of a full-file rescan).
    let q = parse_query("L0", &SearchOptions::default()).unwrap();
    let cfg = SearchConfig { sample_interval: 100, max_samples: 200_000 };
    let idx = run_search(&q, &mmap, &cfg, qview_core::file::SCAN_WINDOW).unwrap();

    // Exactly one "L0" per line (line prefix "L0000000N"), so total = lines.
    let total_lines = (data.len() / BYTES_PER_LINE) as usize;
    assert_eq!(idx.total_count(), total_lines, "exact hit count across window");
    assert!(idx.total_count() > 100);
    assert_eq!(idx.sample_interval(), 100, "dense sampling");

    // Navigate hits near and across the window boundary.
    let boundary_hit = WINDOW / BYTES_PER_LINE; // line at the boundary
    assert!(boundary_hit < idx.total_count(), "boundary hit exists");
    for n in [0usize, boundary_hit - 1, boundary_hit, boundary_hit + 1] {
        let pos = idx.get(n).expect("hit found");
        assert_eq!(
            &mmap.as_slice()[pos as usize..pos as usize + 2],
            b"L0",
            "hit {n} across window boundary"
        );
    }

    let _ = std::fs::remove_file(&path);
}

/// The index is identical no matter the scan window size — the window only
/// trades memory (two windows resident) against boundary overhead. A 64 KiB
/// window on a 66 MiB file forces ~1000 boundaries; the result must match
/// the default 64 MiB window exactly.
#[test]
fn sparse_index_is_independent_of_window_size() {
    let mut path = std::env::temp_dir();
    path.push(format!("qview_boundary_win_{}.txt", std::process::id()));
    let data = build_data(WINDOW + 2 * 1024 * 1024);
    std::fs::write(&path, &data).unwrap();

    let mmap = MmapBackend::open(&path).unwrap();
    let reference = {
        let b = IndexBuilder::new(mmap.clone());
        b.build_sparse_with_progress(|_, _| {}).unwrap()
    };

    for window in [64u64 * 1024, 256 * 1024, 4 * 1024 * 1024] {
        let mut b = IndexBuilder::new(mmap.clone());
        b.set_scan_window(window);
        let got = b.build_sparse_with_progress(|_, _| {}).unwrap();
        assert_eq!(got.sparse, reference.sparse, "sparse offsets, window={window}");
        assert_eq!(got.total_lines, reference.total_lines, "total_lines, window={window}");
        assert_eq!(got.max_line_bytes, reference.max_line_bytes, "max_line_bytes, window={window}");
        assert_eq!(got.max_line_index, reference.max_line_index, "max_line_index, window={window}");
    }

    let _ = std::fs::remove_file(&path);
}

/// Search with a tiny 64 KiB window: line (64 KiB / 10) = 6553 occupies bytes
/// [65530, 65540) as "L00006553\n", so the 64 KiB boundary falls mid-line.
/// Pattern "L0000655" starts at byte 65530 (< 65536, owned by window 0) and
/// ends at byte 65538 (> 65536) — it can only be found via the overlap tail.
/// Without the overlap the window-0 scan would be truncated at the boundary
/// and the hit silently missed.
#[test]
fn search_overlap_holds_with_small_window() {
    let mut path = std::env::temp_dir();
    path.push(format!("qview_boundary_ovl_{}.txt", std::process::id()));
    let data = build_data(WINDOW + 2 * 1024 * 1024);
    std::fs::write(&path, &data).unwrap();

    let mmap = MmapBackend::open(&path).unwrap();
    let q = parse_query("L0000655", &SearchOptions::default()).unwrap();
    let cfg = SearchConfig { sample_interval: 100, max_samples: 200_000 };
    let idx = run_search(&q, &mmap, &cfg, 64u64 * 1024).unwrap();

    // Lines 6550–6559 are "L0000655X\n"; all 10 contain the pattern. Line 6553
    // (bytes [65530, 65540)) is the one straddling the 64 KiB boundary — its
    // match [65530, 65538) extends past 65536 and can only be found via the
    // overlap tail. Missing it would drop the count to 9 and misplace get(3).
    assert_eq!(idx.total_count(), 10, "all 10 hits found incl. the straddling one");
    let pos = idx.get(3).expect("boundary-straddling hit found");
    assert_eq!(pos, 65530, "4th hit = the line straddling the boundary");
    assert_eq!(
        &mmap.as_slice()[pos as usize..pos as usize + 8],
        b"L0000655",
        "boundary-straddling hit resolved wrong"
    );

    let _ = std::fs::remove_file(&path);
}
