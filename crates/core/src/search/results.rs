//! Search results with bounded exact storage.

use std::sync::Arc;
use parking_lot::RwLock;

use super::block_index::BlockIndex;

#[derive(Debug, Clone, Copy)]
pub struct Match { pub byte: u64 }

#[derive(Debug, Clone, Default)]
pub struct SearchStats { pub hits: u64, pub elapsed_ms: u64 }

pub struct SearchResults {
    inner: Arc<RwLock<SearchResultsInner>>,
}

struct SearchResultsInner {
    index: Option<Arc<BlockIndex>>,
    cursor: usize,
    query: String,
    /// Cached (cursor, byte) pair so `current()` doesn't call the expensive
    /// `BlockIndex::get()` every frame.  Invalidated whenever `cursor` changes.
    cached: Option<(usize, u64)>,
}

impl SearchResults {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(SearchResultsInner {
                index: None,
                cursor: 0,
                query: String::new(),
                cached: None,
            })),
        }
    }

    pub fn set_results(&self, index: Arc<BlockIndex>, query: String) {
        let mut w = self.inner.write();
        w.index = Some(index);
        w.cursor = 0;
        w.query = query;
        w.cached = None;
    }

    pub fn clear(&self) {
        let mut w = self.inner.write();
        w.index = None;
        w.cursor = 0;
        w.query.clear();
        w.cached = None;
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inner.read().index.as_ref().map(|i| i.is_empty()).unwrap_or(true)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.inner.read().index.as_ref().map(|i| i.total_count()).unwrap_or(0)
    }

    #[inline]
    pub fn stored_count(&self) -> usize {
        self.inner.read().index.as_ref().map(|i| i.stored_count()).unwrap_or(0)
    }

    #[inline]
    pub fn sample_interval(&self) -> u32 {
        self.inner.read().index.as_ref().map(|i| i.sample_interval()).unwrap_or(0)
    }

    pub fn query(&self) -> String { self.inner.read().query.clone() }

    #[inline]
    pub fn cursor(&self) -> usize { self.inner.read().cursor }

    pub fn current(&self) -> Option<Match> {
        let g = self.inner.read();
        // Use cached byte if cursor hasn't changed since last lookup.
        if let Some((cached_cursor, cached_byte)) = g.cached {
            if cached_cursor == g.cursor {
                return Some(Match { byte: cached_byte });
            }
        }
        let index = g.index.as_ref()?;
        let byte = index.get(g.cursor)?;
        drop(g);
        // Update cache.
        let mut w = self.inner.write();
        w.cached = Some((w.cursor, byte));
        Some(Match { byte })
    }

    pub fn jump(&self, n: usize) -> Option<Match> {
        let mut w = self.inner.write();
        let index = w.index.as_ref().map(Arc::clone)?;
        let total = index.total_count();
        if total == 0 { return None; }
        w.cursor = n.min(total.saturating_sub(1));
        w.cached = None; // invalidated by cursor change
        index.get(w.cursor).map(|byte| Match { byte })
    }

    pub fn next(&self) -> Option<Match> {
        let mut w = self.inner.write();
        let index = w.index.as_ref().map(Arc::clone)?;
        let total = index.total_count();
        if total == 0 { return None; }
        w.cursor = (w.cursor + 1) % total;
        w.cached = None;
        index.get(w.cursor).map(|byte| Match { byte })
    }

    pub fn prev(&self) -> Option<Match> {
        let mut w = self.inner.write();
        let index = w.index.as_ref().map(Arc::clone)?;
        let total = index.total_count();
        if total == 0 { return None; }
        w.cursor = if w.cursor == 0 { total.saturating_sub(1) } else { w.cursor - 1 };
        w.cached = None;
        index.get(w.cursor).map(|byte| Match { byte })
    }

    pub fn jump_by(&self, delta: i64) -> Option<Match> {
        let mut w = self.inner.write();
        let index = w.index.as_ref().map(Arc::clone)?;
        let total = index.total_count() as i64;
        if total == 0 { return None; }
        let new = (w.cursor as i64 + delta).rem_euclid(total);
        w.cursor = new as usize;
        w.cached = None;
        index.get(w.cursor).map(|byte| Match { byte })
    }

    pub fn snapshot_hits(&self) -> Vec<u64> {
        self.inner.read().index.as_ref().map(|i| i.snapshot().to_vec()).unwrap_or_default()
    }

    /// Reposition the cursor to the first hit whose byte offset is >= `byte`.
    /// Returns `true` if a suitable hit was found, `false` otherwise.
    pub fn seek_to_byte(&self, byte: u64) -> bool {
        let mut w = self.inner.write();
        let index = match w.index.as_ref() {
            Some(i) => i,
            None => return false,
        };
        match index.find_hit_after(byte) {
            Some(n) => {
                w.cursor = n;
                w.cached = None;
                true
            }
            None => false,
        }
    }

    pub fn first(&self) -> Option<Match> { self.jump(0) }

    pub fn last(&self) -> Option<Match> {
        let total = {
            let g = self.inner.read();
            let index = g.index.as_ref()?;
            if index.is_empty() { return None; }
            index.total_count()
        };
        self.jump(total - 1)
    }
}

impl Default for SearchResults {
    fn default() -> Self { Self::new() }
}
