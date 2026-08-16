//! Two-tier line cache for the viewport.
//!
//! Tier 1 (`RawLine`): decoded UTF-8 for a given physical/logical line number.
//! Invalidated when file content changes (e.g. tail -f appends).
//!
//! Tier 2 (`DisplayLine`): the truncated + highlighted version for a specific
//! (width, horiz_offset, search_hash) tuple. Invalidated on resize or search
//! query change — tier 1 stays warm.
//!
//! Scrolling hot path:
//!   1. hit tier-2 → done
//!   2. miss, hit tier-1 → truncate + highlight → fill tier-2
//!   3. miss both → read from mmap → fill tier-1 → fill tier-2

use std::num::NonZeroUsize;

use lru::LruCache;

/// One decoded line. `text` is lossy UTF-8, `byte_len` excludes the trailing \n.
#[derive(Debug, Clone, Default)]
pub struct RawLine {
    pub text: String,
    pub byte_len: usize,
    /// Set when this line is from an in-memory edit (gutter shows `[+]`).
    pub modified: bool,
    /// Exact byte offset of this line's start in the source file (0 when line is
    /// inserted or not applicable).
    pub start_byte: u64,
}

/// One line ready to draw: truncated to terminal width with search highlights.
#[derive(Debug, Clone, Default)]
pub struct DisplayLine {
    pub text: String,
    /// Byte ranges in `text` to highlight (from search).
    pub matches: Vec<(usize, usize)>,
    pub truncated_right: bool,
    pub truncated_left: bool,
    pub modified: bool,
}

/// Key for tier-2 cache: (width, horiz_offset, search_hash).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DisplayKey {
    pub width: u16,
    pub horiz: u16,
    /// xxhash64 of the active search query, 0 if no search.
    pub search_hash: u64,
}

pub struct LineCache {
    raw: LruCache<u64, RawLine>,
    display: LruCache<(u64, DisplayKey), DisplayLine>,
    raw_cap: usize,
    display_cap: usize,
}

impl LineCache {
    pub fn new(raw_capacity: usize, display_capacity: usize) -> Self {
        Self {
            raw: LruCache::new(NonZeroUsize::new(raw_capacity.max(1)).unwrap()),
            display: LruCache::new(NonZeroUsize::new(display_capacity.max(1)).unwrap()),
            raw_cap: raw_capacity.max(1),
            display_cap: display_capacity.max(1),
        }
    }

    #[inline]
    pub fn get_raw(&mut self, line: u64) -> Option<&RawLine> {
        self.raw.get(&line)
    }

    pub fn put_raw(&mut self, line: u64, value: RawLine) {
        self.raw.put(line, value);
    }

    #[inline]
    pub fn get_display(&mut self, line: u64, key: DisplayKey) -> Option<&DisplayLine> {
        self.display.get(&(line, key))
    }

    pub fn put_display(&mut self, line: u64, key: DisplayKey, value: DisplayLine) {
        self.display.put((line, key), value);
    }

    /// Drop all tier-2 entries. Call when search query or terminal size changes.
    pub fn invalidate_display(&mut self) {
        self.display.clear();
    }

    /// Drop all tier-1 and tier-2 entries. Call after file content changes.
    pub fn invalidate_raw(&mut self) {
        self.raw.clear();
        self.display.clear();
    }

    pub fn raw_len(&self) -> usize {
        self.raw.len()
    }

    pub fn display_len(&self) -> usize {
        self.display.len()
    }

    /// Rough memory estimate (LRU doesn't expose entry sizes).
    pub fn approx_bytes(&self) -> usize {
        let raw_avg = 200;
        let display_avg = 200;
        self.raw.len() * raw_avg + self.display.len() * display_avg
    }

    pub fn raw_cap(&self) -> usize {
        self.raw_cap
    }

    pub fn display_cap(&self) -> usize {
        self.display_cap
    }
}

impl Default for LineCache {
    fn default() -> Self {
        // 8K raw (~1.6 MB), 4K display (~0.8 MB).
        Self::new(8192, 4096)
    }
}
