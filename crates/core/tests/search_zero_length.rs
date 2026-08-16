//! Regression test: zero-length regex matches must not wedge navigation.
//!
//! `regex`'s `find_iter` advances one byte past an empty match; the manual
//! rescan in `BlockIndex::get`/`find_hit_after` must do the same, otherwise
//! `byte` never advances and hit positions are wrong (or `find_hit_after`
//! degenerates to O(total_count)).

use qview_core::config::SearchConfig;
use qview_core::file::MmapBackend;
use qview_core::search::{Query, run_search};

#[test]
fn zero_length_regex_navigation_is_exact() {
    // "baa" with `a*`: at byte 0 the pattern matches EMPTY (zero-length match),
    // then "aa" at bytes 1..=2.  Force sparse sampling (interval = 2) so the
    // rescan loop in get()/find_hit_after() actually runs past the empty match
    // — without the `m.end().max(1)` guard, byte 0 never advances and every
    // lookup past the first hit returns the wrong byte (or None).
    let path = std::env::temp_dir().join("qview_zerolen_test.txt");
    std::fs::write(&path, b"baa").unwrap();
    let mmap = MmapBackend::open(&path).unwrap();
    let q = Query::Regex(regex::bytes::Regex::new("a*").unwrap());
    let cfg = SearchConfig {
        sample_interval: 2,
        max_samples: 1,
        ..Default::default()
    };

    let idx = run_search(&q, &mmap, &cfg, qview_core::file::SCAN_WINDOW).unwrap();
    assert_eq!(idx.total_count(), 2, "empty match at byte 0, then 'aa' at byte 1");
    assert_eq!(idx.sample_interval(), 2, "adaptive: 2 > max_samples=1 → sparse");
    assert_eq!(idx.snapshot(), &[0], "every 2nd hit: only byte 0");

    // get(n) must land on the exact n-th hit byte.  get(1) rescans from the
    // byte-0 sample past the EMPTY match — the guard's whole point.
    assert_eq!(idx.get(0), Some(0), "hit 0 = empty match at byte 0");
    assert_eq!(idx.get(1), Some(1), "hit 1 = 'aa' at byte 1");
    assert_eq!(idx.get(2), None, "only 2 hits");

    // find_hit_after must find the first hit START at/after the target byte.
    assert_eq!(idx.find_hit_after(0), Some(0));
    assert_eq!(idx.find_hit_after(1), Some(1));
    assert_eq!(idx.find_hit_after(2), None, "hit 1 starts at byte 1 < 2");

    std::fs::remove_file(&path).ok();
}
