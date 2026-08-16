//! Deterministic generator for the 5 standard industry-test log files.
//!
//! Line format (realistic, variable 80–200 B):
//! `[2026-08-05 10:23:45.123] [INFO] [auth-service] [a1b2c3d4] message`
//!
//! Properties that make it a fair industry benchmark:
//! - **Seeded RNG** (`SEED`): the exact same bytes are generated on every
//!   machine and every run → results are reproducible and A/B-comparable.
//! - **Levels weighted** INFO 60 / WARN 25 / ERROR 10 / DEBUG 5 %.
//! - **Searchable markers** with a known target count:
//!   * `ERROR-CODE-404` (~1 % of lines) → literal-search target,
//!     does **not** match the regex below.
//!   * `ERROR-500` / `ERROR-503` → regex target `ERROR-\d{3}` (~1 % combined).
//!   * `TIMEOUT:` → secondary literal marker.
//! - **Variable line length** (fillers of random width) so the data is not
//!   trivially compressible and simulates real logs.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

/// Fixed seed — identical data on every machine, forever.
pub const SEED: u64 = 0xC0_FFEE_1234;

/// The five standard levels: (name, approx. lines). S≈10 MB … XXL≈50 GB.
pub const LEVELS: &[(&str, u64)] = &[
    ("s", 100_000),
    ("m", 1_000_000),
    ("l", 10_000_000),
    ("xl", 100_000_000),
    ("xxl", 500_000_000),
];

/// Benchmark search targets (see module docs).
pub const LITERAL: &str = "ERROR-CODE-404";
pub const REGEX_PAT: &str = r"ERROR-\d{3}";
pub const TIMEOUT: &str = "TIMEOUT:";

pub fn level_file(level: &str) -> String {
    format!("test_{level}.log")
}

pub fn level_selected(level: &str, sel: &Option<Vec<String>>) -> bool {
    match sel {
        Some(v) => v.iter().any(|s| s == level),
        None => true,
    }
}

const MODULES: &[&str] = &[
    "auth-service",
    "api-gateway",
    "user-db",
    "payment-worker",
    "notification-svc",
    "scheduler",
    "cache-proxy",
    "file-storage",
];

const LEVEL_POOL: [&str; 4] = ["INFO", "WARN", "ERROR", "DEBUG"];
const LEVEL_WEIGHTS: [u32; 4] = [60, 25, 10, 5];

/// (template, weight). `{}` placeholders are filled with random values.
const MESSAGES: &[(&str, u32)] = &[
    ("Request processed successfully in {}ms", 300),
    ("Connection pool exhausted, retrying in {}ms", 60),
    ("Cache miss for key session_{}, refilling", 80),
    ("ERROR-CODE-404: Resource not found at /api/v1/orders/{}", 6),
    ("TIMEOUT: Upstream service did not respond in {}ms", 5),
    ("ERROR-500: Internal server error while handling request {}", 3),
    ("ERROR-503: Service temporarily unavailable, retry in {}ms", 3),
    ("Rate limit exceeded for IP 192.168.{}.{}, retrying", 20),
    ("Database query took {}ms, threshold is 200ms", 40),
    ("User login success from device mobile-{}, token refreshed", 30),
    ("Failed to deserialize payload: unexpected EOF at offset {}", 25),
    ("Health check passed, cluster status: green, {} nodes alive", 15),
];

struct Msg {
    weight: u32,
    segs: Vec<&'static str>, // template split on "{}"
}

fn build_msgs() -> Vec<Msg> {
    MESSAGES
        .iter()
        .map(|(t, w)| Msg { weight: *w, segs: t.split("{}").collect() })
        .collect()
}

/// Generate `num_lines` log lines into `w` (used for both files and tests).
pub fn generate_lines<W: Write>(w: &mut W, num_lines: u64, rng: &mut StdRng) -> std::io::Result<()> {
    let msgs = build_msgs();
    let total_weight: u32 = msgs.iter().map(|m| m.weight).sum();
    let mut line = String::with_capacity(200);
    for i in 0..num_lines {
        line.clear();
        line.push('[');
        append_timestamp(i, &mut line);
        line.push_str("] [");
        line.push_str(pick_level(rng));
        line.push_str("] [");
        line.push_str(MODULES[rng.gen_range(0..MODULES.len())]);
        line.push_str("] [");
        append_reqid(&mut line, rng);
        line.push_str("] ");
        let mut t = rng.gen_range(0..total_weight);
        let mut chosen = &msgs[0];
        for m in msgs.iter() {
            if t < m.weight {
                chosen = m;
                break;
            }
            t -= m.weight;
        }
        append_message(&mut line, chosen, rng);
        line.push('\n');
        w.write_all(line.as_bytes())?;
    }
    w.flush()
}

/// Generate one standard test file. Deterministic for the same `num_lines`.
pub fn gen_file(path: &Path, num_lines: u64) -> std::io::Result<()> {
    let f = File::create(path)?;
    let mut w = BufWriter::with_capacity(64 * 1024 * 1024, f);
    let mut rng = StdRng::seed_from_u64(SEED);
    generate_lines(&mut w, num_lines, &mut rng)?;
    w.flush()
}

fn pick_level(rng: &mut StdRng) -> &'static str {
    let total: u32 = LEVEL_WEIGHTS.iter().sum();
    let mut t = rng.gen_range(0..total);
    for (i, w) in LEVEL_WEIGHTS.iter().enumerate() {
        if t < *w {
            return LEVEL_POOL[i];
        }
        t -= *w;
    }
    LEVEL_POOL[3]
}

fn append_reqid(out: &mut String, rng: &mut StdRng) {
    for _ in 0..8 {
        out.push(char::from_digit(rng.gen_range(0..16), 16).unwrap());
    }
}

fn append_message(out: &mut String, msg: &Msg, rng: &mut StdRng) {
    for (i, seg) in msg.segs.iter().enumerate() {
        out.push_str(seg);
        if i + 1 < msg.segs.len() {
            append_filler(out, rng);
        }
    }
}

fn append_filler(out: &mut String, rng: &mut StdRng) {
    use std::fmt::Write;
    match rng.gen_range(0..4) {
        0 => {
            let v = rng.gen_range(1..999);
            let _ = write!(out, "{v}");
        }
        1 => {
            let v = rng.gen_range(0..0xffff);
            let _ = write!(out, "0x{v:04x}");
        }
        2 => {
            let a = rng.gen_range(1..255);
            let b = rng.gen_range(1..255);
            let _ = write!(out, "{a}.{b}");
        }
        _ => {
            let v = rng.gen_range(100_000..999_999);
            let _ = write!(out, "{v}");
        }
    }
}

/// Append `YYYY-MM-DD HH:MM:SS.mmm` for line `i` (base 2026-01-01, +100 ms/line).
fn append_timestamp(i: u64, out: &mut String) {
    use std::fmt::Write;
    let ms_total = i * 100;
    let days = ms_total / 86_400_000;
    let rem = ms_total % 86_400_000;
    let (y, mo, d) = civil_from_days(days as i64);
    let h = rem / 3_600_000;
    let mi = (rem % 3_600_000) / 60_000;
    let s = (rem % 60_000) / 1_000;
    let ms = rem % 1_000;
    let _ = write!(out, "{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{ms:03}");
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's civil algorithm.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------------------------------------------------------------------------
// Tests (tiny — no large files, just shape/determinism checks)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_days_reference() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 1970-01-01 → 2026-01-01 = 20454 days (56 years, 14 leap days).
        assert_eq!(civil_from_days(20454), (2026, 1, 1));
        // Around the base timestamp of the generated data.
        assert_eq!(civil_from_days(20474), (2026, 1, 21));
    }

    #[test]
    fn deterministic_output() {
        let mut a = StdRng::seed_from_u64(SEED);
        let mut b = StdRng::seed_from_u64(SEED);
        let mut out1 = Vec::new();
        generate_lines(&mut out1, 5000, &mut a).unwrap();
        let mut out2 = Vec::new();
        generate_lines(&mut out2, 5000, &mut b).unwrap();
        assert_eq!(out1, out2, "same seed must produce identical bytes");
    }

    #[test]
    fn line_shape_and_length() {
        let mut rng = StdRng::seed_from_u64(SEED);
        let mut out = Vec::new();
        generate_lines(&mut out, 20_000, &mut rng).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.matches('\n').count(), 20_000);
        for line in text.lines() {
            assert!(
                line.len() >= 80 && line.len() <= 200,
                "line length {} out of [80,200]: {line:?}",
                line.len()
            );
            assert!(line.starts_with('['));
            let has_level = ["[INFO]", "[WARN]", "[ERROR]", "[DEBUG]"]
                .iter()
                .any(|l| line.contains(l));
            assert!(has_level, "no level tag: {line:?}");
        }
    }

    #[test]
    fn marker_quantities() {
        let mut rng = StdRng::seed_from_u64(SEED);
        let mut out = Vec::new();
        generate_lines(&mut out, 100_000, &mut rng).unwrap();
        let text = String::from_utf8(out).unwrap();
        // ERROR-CODE-404 ≈ 1 % of lines (~1000 per 100k). The regex
        // `ERROR-\d{3}` must NOT match it (after "ERROR-" comes "CODE").
        let lit = text.matches(LITERAL).count();
        assert!((500..=2000).contains(&lit), "literal hits = {lit}");
        assert!(text.contains("ERROR-500:") && text.contains("ERROR-503:"), "regex targets present");
        assert!(text.contains(TIMEOUT), "timeout marker present");
        // Sanity: a line that looks like the regex target exists in a form the
        // regex would match (ERROR-<3digits>), verified via string slicing.
        assert!(text.contains("ERROR-500") && text.contains("ERROR-503"));
    }
}
