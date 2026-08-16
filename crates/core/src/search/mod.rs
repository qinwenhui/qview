//! Search engine: literal + regex, parallel, streaming.

pub mod block_index;
pub mod literal;
pub mod progress;
pub mod regex_search;
pub mod results;

pub use block_index::{BlockIndex, MAX_SAMPLES, SAMPLE_INTERVAL}; // defaults; tunable via crate::config::SearchConfig
pub use literal::LiteralSearch;
pub use progress::{BackgroundSearch, SearchProgress};
pub use regex_search::RegexSearch;
pub use results::{Match, SearchResults, SearchStats};

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::Result;
use regex::bytes::Regex;

use crate::file::{MmapBackend, WindowStream};

#[derive(Debug, Clone)]
pub enum Query {
    Literal(Vec<u8>),
    Regex(Regex),
}

#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub use_regex: bool,
    pub whole_word: bool,
    /// The file predominantly uses CRLF (`\r\n`) line endings. When set, regex
    /// `$` anchors are rewritten to `(?:\r?$)` so they match CRLF line ends
    /// (the raw bytes hold `\r\n`, so a plain `$` would never match). Set from
    /// [`crate::Engine::uses_crlf`] by frontends.
    pub crlf: bool,
}

pub fn parse_query(input: &str, opts: &SearchOptions) -> Result<Query> {
    if input.is_empty() {
        return Ok(Query::Literal(Vec::new()));
    }
    if opts.use_regex {
        let mut pattern = if opts.case_sensitive {
            input.to_string()
        } else {
            format!("(?i){}", input)
        };
        // CRLF files store '\r\n'; regex `$` matches before '\n' only, so a
        // `$`-anchored pattern could never match (the `\r` sits between the
        // content and the `\n`). Allow an optional `\r` before `$`.
        if opts.crlf {
            pattern = rewrite_dollar_crlf(&pattern);
        }
        let re = Regex::new(&pattern)?;
        return Ok(Query::Regex(re));
    }
    if opts.case_sensitive && !opts.whole_word {
        return Ok(Query::Literal(input.as_bytes().to_vec()));
    }
    let escaped = regex::escape(input);
    let pattern = if opts.whole_word {
        format!(r"\b{}\b", escaped)
    } else {
        escaped
    };
    let pattern = if !opts.case_sensitive {
        format!("(?i){}", pattern)
    } else {
        pattern
    };
    let re = Regex::new(&pattern)?;
    Ok(Query::Regex(re))
}

/// Rewrite unescaped `$` anchors to `(?:\r?$)` so they match CRLF line ends.
/// `\$` (escaped literal) and `$` inside `[...]` are left untouched. Operates
/// on chars, so non-ASCII (e.g. Chinese) literals pass through intact.
fn rewrite_dollar_crlf(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut it = pattern.chars();
    let mut in_class = false;
    while let Some(c) = it.next() {
        match c {
            '\\' => {
                // Copy the escape and its escaped char verbatim (`\$` is a literal dollar).
                out.push('\\');
                if let Some(esc) = it.next() {
                    out.push(esc);
                }
            }
            '[' => {
                in_class = true;
                out.push('[');
            }
            ']' => {
                in_class = false;
                out.push(']');
            }
            '$' if !in_class => out.push_str("(?:\\r?$)"),
            _ => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Windowed single-pass adaptive hit scan
// ---------------------------------------------------------------------------

/// Backward line-alignment look-back (bytes). Matches `MAX_LEAD` in the scan
/// reader: the window's prepended lead, and the furthest we search back for the
/// previous `\n` to align a chunk scan to a line start. Real log lines are far
/// below this; a longer line degrades to the pre-fix behavior for that chunk
/// (a rare phantom), which is documented.
const MAX_LINE: usize = crate::file::MAX_LEAD;

/// Align one chunk's scan to a real line start.
///
/// The window slice is `[lead][owned][overlap]`. A chunk's owned region is
/// `[owned_start, owned_end)` where `owned_start = max(chunk_start, lead)`. To
/// make `^`-anchors and run patterns (`\S+`, `\d+`, …) behave correctly at the
/// chunk boundary, the scan starts at the previous `\n` (`s ≤ owned_start`),
/// so the engine sees the true preceding bytes. Hits in the alignment prefix
/// `[s, owned_start)` were already counted by the previous chunk (the prefix is
/// strictly inside its owned region because `chunk_size ≥ 512 KiB > MAX_LINE`),
/// so they are skipped via `lead_skip`.
///
/// Returns `(s, lead_skip, owned_limit, scan_end)` or `None` for an empty chunk.
#[inline]
fn chunk_scan(
    slice: &[u8],
    lead: usize,
    len_us: usize,
    overlap: usize,
    chunk_start: usize,
    chunk_len: usize,
) -> Option<(usize, usize, usize, usize)> {
    let owned_start = chunk_start.max(lead);
    // The chunk's end is its actual par_chunk boundary — NOT `owned_start +
    // chunk_len`, which for chunk 0 would extend past the chunk and overlap the
    // next chunk's region (double-counting when a lead is present).
    let owned_end = (chunk_start + chunk_len).min(lead + len_us);
    if owned_start >= owned_end {
        return None; // chunk outside the window's owned region
    }
    let lo = owned_start.saturating_sub(MAX_LINE);
    let s = match memchr::memrchr(b'\n', &slice[lo..owned_start]) {
        Some(p) => lo + p + 1, // byte after the previous newline = line start
        None => owned_start,   // no newline within the bound → scan from owned start
    };
    let scan_end = (owned_end + overlap).min(slice.len());
    Some((s, owned_start - s, owned_end - s, scan_end))
}

/// Number of hits in `scan` that fall inside the chunk's owned region
/// (`[lead_skip, owned_limit)`; before `lead_skip` is the alignment prefix,
/// after `owned_limit` is overlap into the next chunk).
fn count_chunk_hits(
    query: &Query,
    scan: &[u8],
    lead_skip: usize,
    owned_limit: usize,
) -> usize {
    match query {
        Query::Literal(pat) => {
            let mut n = 0;
            for m in memchr::memmem::find_iter(scan, pat) {
                if m < lead_skip {
                    continue;
                }
                if m >= owned_limit {
                    break;
                }
                n += 1;
            }
            n
        }
        Query::Regex(re) => {
            let mut n = 0;
            for m in re.find_iter(scan) {
                let m = m.start();
                if m < lead_skip {
                    continue;
                }
                if m >= owned_limit {
                    break;
                }
                n += 1;
            }
            n
        }
    }
}

/// Collect every hit's absolute byte offset from the chunk's owned region.
fn collect_chunk_hits(
    query: &Query,
    scan: &[u8],
    lead_skip: usize,
    owned_limit: usize,
    base: usize,
) -> Vec<u64> {
    match query {
        Query::Literal(pat) => memchr::memmem::find_iter(scan, pat)
            .skip_while(|&m| m < lead_skip)
            .take_while(|&m| m < owned_limit)
            .map(|m| (base + m) as u64)
            .collect(),
        Query::Regex(re) => re
            .find_iter(scan)
            .map(|m| m.start())
            .skip_while(|&m| m < lead_skip)
            .take_while(|&m| m < owned_limit)
            .map(|m| (base + m) as u64)
            .collect(),
    }
}

/// Collect hits whose GLOBAL hit index is a multiple of `interval`, where
/// `start_hit` is the global index of this chunk's first hit. Countdown instead
/// of per-match modulo (hot path).
fn collect_chunk_interval(
    query: &Query,
    scan: &[u8],
    lead_skip: usize,
    owned_limit: usize,
    base: usize,
    interval: usize,
    start_hit: usize,
) -> Vec<u64> {
    let first_sample = ((start_hit + interval - 1) / interval) * interval;
    let mut skip = first_sample - start_hit;
    let mut out = Vec::new();
    match query {
        Query::Literal(pat) => {
            for m in memchr::memmem::find_iter(scan, pat) {
                if m < lead_skip {
                    continue; // alignment prefix — counted by the previous chunk
                }
                if m >= owned_limit {
                    break;
                }
                if skip == 0 {
                    out.push((base + m) as u64);
                    skip = interval - 1;
                } else {
                    skip -= 1;
                }
            }
        }
        Query::Regex(re) => {
            for m in re.find_iter(scan) {
                let m = m.start();
                if m < lead_skip {
                    continue; // alignment prefix — counted by the previous chunk
                }
                if m >= owned_limit {
                    break;
                }
                if skip == 0 {
                    out.push((base + m) as u64);
                    skip = interval - 1;
                } else {
                    skip -= 1;
                }
            }
        }
    }
    out
}

/// Single-pass adaptive hit scan over the whole file, windowed.
///
/// Iterates the file in [`SCAN_WINDOW`] streamed windows, one at a time
/// ([`ScanReader`]): sub-pass A counts hits per chunk (parallel), then the
/// window's hits are either ALL buffered (sparse results, ≤ `max_samples`) or
/// sampled at `sample_interval` intervals (dense results). The file is read
/// from disk ONCE; the sampling sub-pass reads the window from RAM while it is
/// still buffered.
///
/// Returns `(samples, sample_interval, total_count)`. Samples are byte offsets
/// of hits at global hit indices 0, interval, 2·interval, … (or every hit when
/// the result is sparse, interval == 1), so `BlockIndex::get` stays exact.
pub(crate) fn scan_hits(
    query: &Query,
    path: &Path,
    scan_window: u64,
    sample_interval: u32,
    max_samples: usize,
    cancel: Option<&AtomicBool>,
    scanned: Option<&AtomicUsize>,
) -> Result<(Vec<u64>, u32, usize)> {
    use rayon::prelude::*;

    // Parallel sub-chunk: ~one per scan-pool thread, clamped to [512 KiB, 8 MiB].
    let chunk_size = {
        let threads = crate::parallel::scan_pool().current_num_threads().max(1);
        ((scan_window as usize) / threads).clamp(512 * 1024, 8 * 1024 * 1024)
    };
    let interval = sample_interval.max(1) as usize;
    let max_samp = max_samples.max(1);
    let pat_len = match query {
        Query::Literal(p) => p.len(),
        Query::Regex(_) => 0,
    };
    let overlap = pat_len.saturating_sub(1).max(256);

    let stream = WindowStream::open(path, overlap, scan_window)?;
    let mut samples: Vec<u64> = Vec::new();
    let mut dense = false;
    let mut running = 0usize; // hits before the current window
    let is_cancel = || cancel.map_or(false, |c| c.load(Ordering::Relaxed));

    while let Some(win) = stream.next()? {
        let slice = win.as_slice();
        let wstart = win.start() as usize;
        let len_us = win.owned();
        let lead = win.lead();
        // `as_slice()` is `[lead][owned][overlap]`, so slice position `p`
        // maps to file offset `start - lead + p`.  Base for a hit found at
        // scan position `m` (offset from line start `s`) must therefore be
        // `origin + s`, NOT `wstart + s` — the latter over-reports by `lead`
        // and stores the last window's hits past EOF (get() then fails).
        let origin = wstart.saturating_sub(lead);

        // Sub-pass A: count hits per chunk (parallel).
        let counts: Vec<usize> = crate::parallel::scan_pool().install(|| {
            slice
                .par_chunks(chunk_size)
                .enumerate()
                .map(|(i, chunk)| {
                    if let Some(s) = scanned {
                        s.fetch_add(chunk.len(), Ordering::Relaxed);
                    }
                    if is_cancel() {
                        return 0;
                    }
                    let chunk_start = i * chunk_size;
                    match chunk_scan(slice, lead, len_us, overlap, chunk_start, chunk.len()) {
                        Some((s, lead_skip, owned_limit, scan_end)) => {
                            count_chunk_hits(query, &slice[s..scan_end], lead_skip, owned_limit)
                        }
                        None => 0,
                    }
                })
                .collect()
        });
        let window_hits: usize = counts.iter().sum();
        if is_cancel() {
            anyhow::bail!("cancelled");
        }

        // prefix[i] = hits before chunk i within this window.
        let mut prefix = Vec::with_capacity(counts.len() + 1);
        let mut acc = 0usize;
        for &c in &counts {
            prefix.push(acc);
            acc += c;
        }

        if !dense && running + window_hits <= max_samp {
            // Sparse phase: buffer every hit (parallel, order preserved).
            let parts: Vec<Vec<u64>> = crate::parallel::scan_pool().install(|| {
                slice
                    .par_chunks(chunk_size)
                    .enumerate()
                    .map(|(i, chunk)| {
                        if is_cancel() {
                            return Vec::new();
                        }
                        let chunk_start = i * chunk_size;
                        match chunk_scan(slice, lead, len_us, overlap, chunk_start, chunk.len()) {
                            Some((s, lead_skip, owned_limit, scan_end)) => collect_chunk_hits(
                                query,
                                &slice[s..scan_end],
                                lead_skip,
                                owned_limit,
                                origin + s,
                            ),
                            None => Vec::new(),
                        }
                    })
                    .collect()
            });
            for v in parts {
                samples.extend(v);
            }
        } else if !dense {
            // Crossing window: total exceeds max_samples here. Buffer the hits
            // up to the cap, then switch to interval sampling. Processed
            // sequentially so the running hit index stays exact (rare: once per
            // search, one window).
            dense = true;
            let mut n = 0usize;
            for (i, chunk) in slice.chunks(chunk_size).enumerate() {
                if is_cancel() {
                    anyhow::bail!("cancelled");
                }
                let chunk_start = i * chunk_size;
                let Some((s, lead_skip, owned_limit, scan_end)) =
                    chunk_scan(slice, lead, len_us, overlap, chunk_start, chunk.len())
                else {
                    continue;
                };
                let scan = &slice[s..scan_end];
                let base = origin + s;
                for m in chunk_hits_iter(query, scan, lead_skip, owned_limit) {
                    let g = running + n;
                    let byte = (base + m) as u64;
                    if g < max_samp {
                        samples.push(byte);
                    } else if g == max_samp {
                        // Thin the buffered hits to every `interval`-th.
                        samples = (0..samples.len())
                            .step_by(interval)
                            .map(|k| samples[k])
                            .collect();
                        if g % interval == 0 && samples.len() < max_samp {
                            samples.push(byte);
                        }
                    } else if g % interval == 0 && samples.len() < max_samp {
                        samples.push(byte);
                    }
                    n += 1;
                }
            }
        } else {
            // Dense phase: interval-sample (parallel). Once the sample buffer
            // is full, skip the sampling pass (sub-pass A still counts so the
            // total stays exact); navigation past the sampled range falls back
            // to a rescan, same as the legacy design.
            if samples.len() < max_samp {
                let parts: Vec<Vec<u64>> = crate::parallel::scan_pool().install(|| {
                    slice
                        .par_chunks(chunk_size)
                        .enumerate()
                        .map(|(i, chunk)| {
                            if is_cancel() {
                                return Vec::new();
                            }
                            let chunk_start = i * chunk_size;
                            match chunk_scan(slice, lead, len_us, overlap, chunk_start, chunk.len())
                            {
                                Some((s, lead_skip, owned_limit, scan_end)) => {
                                    collect_chunk_interval(
                                        query,
                                        &slice[s..scan_end],
                                        lead_skip,
                                        owned_limit,
                                        origin + s,
                                        interval,
                                        running + prefix[i],
                                    )
                                }
                                None => Vec::new(),
                            }
                        })
                        .collect()
                });
                for v in parts {
                    samples.extend(v);
                }
                samples.truncate(max_samp);
            }
        }

        running += window_hits;
    }

    let total_count = running;
    let reported = if dense { interval as u32 } else { 1 };
    Ok((samples, reported, total_count))
}

/// Iterator over a chunk's owned hits (offsets relative to the scan start).
/// Used only by the (rare) crossing window, so a small heap box is fine.
fn chunk_hits_iter<'a>(
    query: &'a Query,
    scan: &'a [u8],
    lead_skip: usize,
    owned_limit: usize,
) -> Box<dyn Iterator<Item = usize> + 'a> {
    match query {
        Query::Literal(pat) => Box::new(
            memchr::memmem::find_iter(scan, pat)
                .skip_while(move |&m| m < lead_skip)
                .take_while(move |&m| m < owned_limit),
        ),
        Query::Regex(re) => Box::new(
            re.find_iter(scan)
                .map(|m| m.start())
                .skip_while(move |&m| m < lead_skip)
                .take_while(move |&m| m < owned_limit),
        ),
    }
}

/// Synchronous search. Returns a BlockIndex with accurate total_count and
/// bounded samples. For interactive use, prefer `BackgroundSearch::spawn` to
/// avoid blocking the UI.
pub fn run_search(
    query: &Query,
    mmap: &MmapBackend,
    cfg: &crate::config::SearchConfig,
    scan_window: u64,
) -> Result<BlockIndex> {
    if mmap.size() == 0 {
        return Ok(BlockIndex::empty());
    }
    let (samples, interval, total_count) = scan_hits(
        query,
        mmap.path(),
        scan_window,
        cfg.sample_interval,
        cfg.max_samples,
        None,
        None,
    )?;
    Ok(BlockIndex::from_samples(mmap.clone(), samples, interval, total_count, query.clone()))
}
