//! Sampled block index for memory-efficient exact search hit navigation.
//!
//! ## Design: Sampled Storage + Local Rescan
//! - Pass 1 counts the exact total of hits (parallel).
//! - Pass 2 stores ONE file offset every `SAMPLE_INTERVAL` hits into `samples`.
//! - Navigation to the n-th hit: jump to sample `n / SAMPLE_INTERVAL`, then
//!   rescan forward, counting hits, until we land on hit n.
//!
//! ## Memory
//! samples.len() × 8 bytes, plus the query pattern (small). With
//! SAMPLE_INTERVAL=100 and 402M hits, samples ≈ 32MB — independent of total.
//!
//! ## Trade-off
//! O(1) memory, O(SAMPLE_INTERVAL) work per lookup. Worst-case a few KB of
//! scan per navigation — sub-millisecond on real files.

use crate::file::MmapBackend;
use crate::search::Query;

/// How many hits between stored samples. Larger = less memory, slower lookup.
pub const SAMPLE_INTERVAL: u32 = 100;

/// Maximum samples to keep regardless of total_count. Caps memory at ~80MB
/// for the default interval. With SAMPLE_INTERVAL=100 this is 10M samples =
/// 80MB, supporting up to 1 billion hits. The `total_count` is always exact,
/// so navigating beyond the sampled range just falls back to a fresh scan.
pub const MAX_SAMPLES: usize = 10_000_000;

pub struct BlockIndex {
    mmap: Option<MmapBackend>,
    /// Sampled file offsets: one every `sample_interval` hits.
    samples: Vec<u64>,
    sample_interval: u32,
    /// Exact total number of hits in the file.
    total_count: usize,
    /// Cached query so `get(n)` can rescan without re-parsing.
    query: Option<Query>,
}

impl std::fmt::Debug for BlockIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockIndex")
            .field("samples", &self.samples.len())
            .field("sample_interval", &self.sample_interval)
            .field("total_count", &self.total_count)
            .field("has_mmap", &self.mmap.is_some())
            .finish()
    }
}

impl BlockIndex {
    /// Build from sampled offsets + exact count. Takes ownership of `samples`.
    pub fn from_samples(
        mmap: MmapBackend,
        samples: Vec<u64>,
        sample_interval: u32,
        total_count: usize,
        query: Query,
    ) -> Self {
        debug_assert!(sample_interval > 0);
        Self { mmap: Some(mmap), samples, sample_interval, total_count, query: Some(query) }
    }

    /// Build an empty index (no hits). Pattern is unknown until a search runs.
    pub fn empty() -> Self {
        Self {
            mmap: None,
            samples: Vec::new(),
            sample_interval: SAMPLE_INTERVAL,
            total_count: 0,
            query: None,
        }
    }

    #[inline]
    pub fn total_count(&self) -> usize { self.total_count }

    #[inline]
    pub fn is_empty(&self) -> bool { self.total_count == 0 }

    /// With sampling, total_count is always exact — there is nothing to truncate.
    #[inline]
    pub fn is_truncated(&self) -> bool { false }

    #[inline]
    pub fn stored_count(&self) -> usize { self.samples.len() }

    #[inline]
    pub fn sample_interval(&self) -> u32 { self.sample_interval }

    /// Look up the n-th hit (0-indexed). Returns None if n >= total_count.
    ///
    /// Strategy:
    /// 1. Pick the closest stored sample at or before n.
    /// 2. Rescan forward from that sample, skipping `n - sample_n` matches.
    ///
    /// Cost: O(n - sample_n) = at most `sample_interval` matches to count
    /// past. For a typical log line that's a few KB of scanning.
    pub fn get(&self, n: usize) -> Option<u64> {
        if n >= self.total_count { return None; }
        if self.samples.is_empty() { return None; }
        let mmap = self.mmap.as_ref()?;
        let query = self.query.as_ref()?;

        let interval = self.sample_interval as usize;
        let sample_idx = (n / interval).min(self.samples.len() - 1);
        let n_after_sample = n - sample_idx * interval;
        let mut byte = self.samples[sample_idx];

        let slice = mmap.as_slice();
        let total = slice.len();
        if byte as usize >= total { return None; }

        // Skip past `n_after_sample` matches, starting AT the sample (the
        // sample itself is match #0).
        for _ in 0..n_after_sample {
            let at = byte as usize;
            if at >= total { return None; }
            // Advance past current match.
            let mlen = match_len(query, &slice[at..]);
            byte += mlen as u64;
            if byte as usize >= total { return None; }
            // Find the next match.
            match next_match(query, &slice[byte as usize..]) {
                Some(off) => byte += off as u64,
                None => return None,
            }
        }
        Some(byte)
    }

    /// Find the smallest hit index `n` such that `get(n) >= target_byte`.
    /// Returns `None` if no hit exists at or after `target_byte`.
    ///
    /// When the index stores every hit (`interval == 1`), this is a simple
    /// binary search (O(log N)).  Otherwise falls back to binary search on
    /// stored samples + forward rescan (at most `sample_interval` matches).
    pub fn find_hit_after(&self, target_byte: u64) -> Option<usize> {
        if self.total_count == 0 || self.samples.is_empty() {
            return None;
        }

        // ── Fast path: full storage (interval == 1) ─────────────────
        // Every hit is stored in `samples`, so `samples[n]` is the byte
        // offset of hit #n.  Binary search gives the answer directly.
        if self.sample_interval == 1 {
            return match self.samples.binary_search(&target_byte) {
                Ok(i) => Some(i),
                Err(i) if i < self.samples.len() => Some(i),
                _ => None,
            };
        }

        // ── Slow path: sparse samples, walk forward ────────────────
        let mmap = self.mmap.as_ref()?;
        let query = self.query.as_ref()?;
        let interval = self.sample_interval as usize;

        let (sample_idx, start_hit_idx) = match self.samples.binary_search(&target_byte) {
            Ok(i) => (i, i * interval),
            Err(0) => (0, 0),
            Err(i) => (i - 1, (i - 1) * interval),
        };

        let mut byte = self.samples[sample_idx];
        let mut hit_idx = start_hit_idx;
        let slice = mmap.as_slice();
        let total = slice.len();

        loop {
            if byte as usize >= total {
                return None;
            }
            if byte >= target_byte && hit_idx < self.total_count {
                return Some(hit_idx);
            }
            let mlen = match_len(query, &slice[byte as usize..]);
            byte += mlen as u64;
            if byte as usize >= total {
                return None;
            }
            match next_match(query, &slice[byte as usize..]) {
                Some(off) => {
                    byte += off as u64;
                    hit_idx += 1;
                    if hit_idx >= self.total_count {
                        return None;
                    }
                }
                None => return None,
            }
        }
    }

    /// Snapshot of raw samples (for serialization / debug).
    #[inline]
    pub fn snapshot(&self) -> &[u64] { &self.samples }
}

/// Length of the match starting at the start of `haystack`.
fn match_len(query: &Query, haystack: &[u8]) -> usize {
    match query {
        Query::Literal(p) => p.len(),
        Query::Regex(re) => {
            // For variable-length regex, find the match itself.  Guard against
            // zero-length matches (e.g. `a*`, `.*?`, `^`): `find_iter` advances
            // one byte past an empty match, so navigation must too, otherwise
            // `byte` never advances and hit positions are wrong (or the
            // `find_hit_after` loop degenerates to O(total_count)).
            re.find(haystack).map(|m| m.end().max(1)).unwrap_or(1)
        }
    }
}

/// Find the next match in `haystack`, returning its offset.
fn next_match(query: &Query, haystack: &[u8]) -> Option<usize> {
    match query {
        Query::Literal(p) => memchr::memmem::find(haystack, p),
        Query::Regex(re) => re.find(haystack).map(|m| m.start()),
    }
}