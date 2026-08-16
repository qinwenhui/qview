//! Regression tests for regex-hit counting across scan chunk / window
//! boundaries.
//!
//! Background: the parallel scan splits the file into arbitrary-byte chunks.
//! Before the line-alignment fix, a chunk boundary that fell mid-line caused
//! (a) `^`-anchored patterns to overcount (the trailing chunk treats its buffer
//! start as a `^` position) and (b) run patterns (`\S+`, `\d+`, `[a-z]+`, …) to
//! double-count runs straddling the boundary. Both made `total_count()` inexact.
//!
//! Ground truth: `regex::bytes::Regex::find_iter` over the WHOLE mmap has no
//! boundaries, so its count is the true value. Every test here asserts
//! `run_search(...).total_count() == whole-mmap count`, machine-independent and
//! deterministic — it holds for every chunking regardless of where boundaries
//! land. Also covers the CRLF `$`-anchor rewrite (`SearchOptions::crlf`).

use std::path::PathBuf;

use qview_core::config::SearchConfig;
use qview_core::file::MmapBackend;
use qview_core::search::{parse_query, run_search, SearchOptions};

const LEVELS: [&str; 4] = ["INFO", "WARN", "ERROR", "DEBUG"];
const SERVICES: [&str; 6] = ["auth", "api", "db", "cache", "gateway", "job"];
const MESSAGES: [&str; 8] = [
    "request processed",
    "cache miss",
    "deadline exceeded: TIMEOUT",
    "ERROR: request failed",
    "request failed: ERROR-CODE-404",
    "http ERROR-500 recorded",
    "queue full: BUFFER-OVERFLOW",
    "gc pause",
];

/// Deterministic xorshift64* RNG — no `rand` dependency in core tests.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo)
    }
    fn pick(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// One realistic, variable-length line (like the Python `gen_regex_test.py`
/// data): levels, services, worker ids, hex request ids, IPs, durations,
/// status codes, marker messages, and a trailing `seq=NNNN`.
fn gen_line(i: u64, rng: &mut Rng, crlf: bool) -> String {
    let level = LEVELS[rng.pick(LEVELS.len())];
    let svc = SERVICES[rng.pick(SERVICES.len())];
    let worker = rng.range(1, 33);
    let req = format!("{:016x}", rng.next());
    let ip = format!("192.168.{}.{}", rng.range(0, 256), rng.range(1, 255));
    let dur = rng.range(1, 10_000);
    let status = [200, 201, 204, 301, 400, 401, 403, 404, 500, 502, 503][rng.pick(11)];
    let msg = MESSAGES[rng.pick(MESSAGES.len())];
    let eol = if crlf { "\r\n" } else { "\n" };
    format!(
        "[2026-08-05 10:23:45.123] [{level}] [{svc}] worker-{worker:02} \
         req={req} ip={ip} dur={dur}us status={status} \"{msg}\" seq={i:08}{eol}"
    )
}

fn build_file(lines: usize, crlf: bool) -> Vec<u8> {
    let mut rng = Rng(0xC0FFEE_2026);
    let mut data = String::with_capacity(lines * 170);
    for i in 0..lines as u64 {
        data.push_str(&gen_line(i, &mut rng, crlf));
    }
    data.into_bytes()
}

fn temp_file(name: &str, data: &[u8]) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("{name}_{}.log", std::process::id()));
    std::fs::write(&path, data).unwrap();
    path
}

/// Open the file once (no index build — `run_search` needs only the mmap) and
/// assert every pattern equals its whole-mmap count.
fn check_all(path: &PathBuf, win_mb: u32, crlf: bool) {
    let mmap = MmapBackend::open(path).unwrap();
    let scan_window = (win_mb as u64) << 20;
    let cfg = SearchConfig::default();

    let cases: &[(&str, bool)] = &[
        (r"\S+", true),         // run double-count bug
        (r"\d+", true),         // run double-count bug
        (r"(?m)^\[", true),     // `^`-anchor phantom bug
        (r"(?m)^\[20\d{2}", true),
        ("ERROR-CODE-404", false), // literal search path
        (r"status=5\d{2}", true),
        (r"\bERROR\b", true),
        (r"\[(INFO|WARN)\]", true),
    ];
    for (pat, use_regex) in cases {
        let opts = SearchOptions {
            use_regex: *use_regex,
            case_sensitive: true,
            whole_word: false,
            crlf,
        };
        let q = parse_query(pat, &opts).unwrap();
        let got = run_search(&q, &mmap, &cfg, scan_window)
            .unwrap()
            .total_count();
        let re = regex::bytes::Regex::new(pat).unwrap();
        let want = re.find_iter(mmap.as_slice()).count();
        assert_eq!(
            got, want,
            "pattern {pat:?} (regex={use_regex}, window={win_mb}MB, crlf={crlf}): \
             engine={got}, whole={want}"
        );
    }
}

/// Every pattern must equal its whole-mmap count under every window size.
/// (The 1 MiB window forces tiny 512 KiB chunks → many boundaries; 16 MiB
/// exercises the same alignment with larger chunks.)
#[test]
fn counts_are_exact_across_chunk_boundaries() {
    let data = build_file(40_000, false); // ~5.5 MiB
    let path = temp_file("qview_regex_boundary", &data);
    for &win_mb in &[1u32, 16] {
        check_all(&path, win_mb, false);
    }
    let _ = std::fs::remove_file(&path);
}

/// Same guarantees on a CRLF file (alignment is to `\n`, so it works for both).
#[test]
fn counts_are_exact_on_crlf_too() {
    let data = build_file(40_000, true);
    let path = temp_file("qview_regex_boundary_crlf", &data);
    check_all(&path, 1, true);
    let _ = std::fs::remove_file(&path);
}

/// Stored hit byte offsets must EXACTLY equal whole-mmap match positions.
///
/// Counting only catches boundary over/under-counting; it cannot see WRONG
/// positions. The search window's `as_slice()` is `[lead][owned][overlap]`
/// (lead = ≤64 KiB look-back for `^`/run alignment), so a hit at slice offset
/// `p` maps to file offset `start - lead + p`. A past bug used `start + p`,
/// over-reporting every hit in windows after the first by `MAX_LEAD`, pushing
/// the last window's samples past EOF — `get(n)` then returned `None` for the
/// tail (breaking `上一个/下一个` navigation to the last few hits) even though
/// `total_count` was exact.
///
/// A 1 MiB window on this ~5.5 MiB file yields several windows (lead > 0 from
/// the second one on), so every sample is checked against the whole-mmap truth.
#[test]
fn stored_positions_exact_across_windows() {
    let data = build_file(40_000, false);
    let path = temp_file("qview_regex_position", &data);
    let mmap = MmapBackend::open(&path).unwrap();
    let win = 1u64 << 20; // 1 MiB window → many windows, lead > 0 after the first
    let cfg = SearchConfig::default();

    let cases: &[(&str, bool)] = &[
        ("ERROR-CODE-404", false), // literal search path
        (r"\bERROR\b", true),
        (r"seq=\d{8}", true),
    ];
    for (pat, use_regex) in cases {
        let opts = SearchOptions {
            use_regex: *use_regex,
            case_sensitive: true,
            whole_word: false,
            crlf: false,
        };
        let q = parse_query(pat, &opts).unwrap();
        let idx = run_search(&q, &mmap, &cfg, win).unwrap();
        let re = regex::bytes::Regex::new(pat).unwrap();
        let truth: Vec<u64> = re.find_iter(mmap.as_slice()).map(|m| m.start() as u64).collect();

        assert_eq!(idx.total_count(), truth.len(), "{pat}: total");
        assert_eq!(idx.sample_interval(), 1, "{pat}: expected sparse (all hits stored)");
        let snap = idx.snapshot();
        assert_eq!(snap.len(), truth.len(), "{pat}: stored count");
        for (k, (&got, &want)) in snap.iter().zip(&truth).enumerate() {
            assert_eq!(got, want, "{pat}: sample[{k}] byte");
        }
        // Every sampled hit must resolve via get() — tail indices failed before
        // the origin fix (samples sat past EOF).
        for &k in &[0usize, 1, truth.len() / 2, truth.len() - 2, truth.len() - 1] {
            assert_eq!(idx.get(k), Some(truth[k]), "{pat}: get({k})");
        }
    }
    let _ = std::fs::remove_file(&path);
}

/// CRLF `$`-anchor: with the flag set the engine rewrites `$` to `(?:\r?$)` so
/// `(?m)seq=\d+$` matches every line; without it, the raw `\r` blocks `$`.
#[test]
fn crlf_dollar_anchor_rewrite() {
    let lines = 30_000u64;
    let data = build_file(lines as usize, true); // CRLF
    let path = temp_file("qview_regex_crlf_dollar", &data);
    let mmap = MmapBackend::open(&path).unwrap();
    let scan_window = 1u64 << 20;
    let cfg = SearchConfig::default();

    let count = |crlf: bool| -> usize {
        let opts = SearchOptions {
            use_regex: true,
            case_sensitive: true,
            whole_word: false,
            crlf,
        };
        let q = parse_query(r"(?m)seq=\d+$", &opts).unwrap();
        run_search(&q, &mmap, &cfg, scan_window).unwrap().total_count()
    };

    // crlf-aware: every line ends with `seq=NNNN\r\n` → all lines match.
    assert_eq!(count(true), lines as usize);
    // raw-byte (LF assumption): `\r` sits before `\n`, so `$` never matches.
    assert_eq!(count(false), 0);
    let _ = std::fs::remove_file(&path);
}

/// The `$`-rewrite must not touch escaped or in-class dollars.
#[test]
fn dollar_rewrite_escapes_and_classes() {
    let path = temp_file("qview_regex_dollar_rewrite", b"[a$] x $5 y \\$z $ q\n");
    let mmap = MmapBackend::open(&path).unwrap();
    let scan_window = 64u64 << 20;
    let cfg = SearchConfig::default();

    // crlf:true must still parse & match exactly like the unrewritten regex
    // for patterns whose `$` is escaped or inside a class.
    for &pat in &[r"\$z", r"[a$]"] {
        let opts = SearchOptions {
            use_regex: true,
            case_sensitive: true,
            whole_word: false,
            crlf: true,
        };
        let q = parse_query(pat, &opts).unwrap();
        let got = run_search(&q, &mmap, &cfg, scan_window)
            .unwrap()
            .total_count();
        let re = regex::bytes::Regex::new(pat).unwrap();
        let want = re.find_iter(mmap.as_slice()).count();
        assert_eq!(got, want, "pattern {pat:?} must be unchanged by crlf rewrite");
    }
    let _ = std::fs::remove_file(&path);
}
