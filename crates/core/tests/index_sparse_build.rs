//! Regression test: the inline sparse index build must produce exactly the
//! same sparse offsets and max-line metadata as the old full-offset build,
//! and every sampled offset must be the real start byte of that line.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use qview_core::file::index::IndexBuildOutcome;
use qview_core::file::{IndexBuilder, LineIndex, MmapBackend, SPARSE_FACTOR};

/// Each call gets a unique file so tests running in parallel never share (and
/// delete) each other's temp file.
static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn build_file(ending_newline: bool) -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "qview_idx_{}_{}_{}.txt",
        std::process::id(),
        n,
        ending_newline
    ));
    let mut content = String::new();
    for k in 0..500u32 {
        // Line 37 is deliberately the longest; the rest vary so max-line
        // detection is non-trivial.
        let fill = if k == 37 {
            "Z".repeat(300)
        } else {
            "y".repeat(k as usize % 11)
        };
        content.push_str(&format!("line{:03}:{}", k, fill));
        content.push('\n');
    }
    if !ending_newline {
        content.pop(); // unterminated last line
    }
    std::fs::write(&path, &content).unwrap();
    path
}

#[test]
fn sparse_build_matches_full_build() {
    for ending_newline in [true, false] {
        let path = build_file(ending_newline);
        let mmap = MmapBackend::open(&path).unwrap();
        let slice = mmap.as_slice();
        let builder = IndexBuilder::new(mmap.clone());

        // New inline-sparse build.
        let outcome: IndexBuildOutcome =
            builder.build_sparse_with_progress(|_, _| {}).unwrap();

        // Ground truth: old full build → from_vec.
        let full = builder.build_with_progress(|_, _| {}).unwrap();
        let idx = LineIndex::from_vec(full, mmap.size());

        assert_eq!(
            outcome.sparse,
            idx.snapshot_offsets(),
            "sparse offsets (end=\\n={ending_newline})"
        );
        assert_eq!(
            outcome.max_line_bytes, idx.max_line_bytes(),
            "max_line_bytes (end=\\n={ending_newline})"
        );
        assert_eq!(
            outcome.max_line_index, idx.max_line_index(),
            "max_line_index (end=\\n={ending_newline})"
        );

        // Exact total line count (compute_line_count semantics).
        let newlines = memchr::memchr_iter(b'\n', slice).count() as u64;
        let expected_total = if slice.last() == Some(&b'\n') {
            newlines
        } else {
            newlines + 1
        };
        assert_eq!(outcome.total_lines, expected_total, "total_lines");

        // Every sampled offset is the real start byte of line i*SPARSE_FACTOR.
        for (i, &off) in outcome.sparse.iter().enumerate() {
            let line = i as u64 * SPARSE_FACTOR as u64;
            assert!(
                off == 0 || slice[off as usize - 1] == b'\n',
                "sparse[{i}] (byte {off}) is not a line start"
            );
            if line > 0 && line < outcome.total_lines {
                let before =
                    memchr::memchr_iter(b'\n', &slice[..off as usize]).count() as u64;
                assert_eq!(before, line, "sparse[{i}] should point at line {line}");
            }
        }
        std::fs::remove_file(&path).ok();
    }
}

#[test]
fn build_aborts_on_cancel() {
    let path = build_file(true);
    let mmap = MmapBackend::open(&path).unwrap();
    let mut builder = IndexBuilder::new(mmap.clone());
    builder.set_cancel(Arc::new(AtomicBool::new(true)));

    // Cancel is pre-set: the build must bail out with a "cancelled" error
    // instead of producing an index (a background indexer maps that to a
    // clean Cancelled instead of persisting a stale .qli).
    let err = builder.build_sparse_with_progress(|_, _| {}).unwrap_err();
    assert!(
        err.to_string().contains("cancelled"),
        "expected cancelled error, got: {err}"
    );

    std::fs::remove_file(&path).ok();
}

#[test]
fn sparse_and_full_strategies_are_interchangeable() {
    let path = build_file(true);
    let mmap = MmapBackend::open(&path).unwrap();
    let builder = IndexBuilder::new(mmap.clone());

    let sparse = builder.build_sparse_with_progress(|_, _| {}).unwrap();
    let full = builder.build_full_with_progress(|_, _| {}).unwrap();

    assert_eq!(sparse.sparse, full.sparse, "sparse offsets identical");
    assert_eq!(sparse.total_lines, full.total_lines, "total_lines identical");
    assert_eq!(
        sparse.max_line_bytes, full.max_line_bytes,
        "max_line_bytes identical"
    );
    assert_eq!(
        sparse.max_line_index, full.max_line_index,
        "max_line_index identical"
    );

    std::fs::remove_file(&path).ok();
}
