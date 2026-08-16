//! Placeholder for page-level prefetch. The kernel already does readahead on
//! sequential access; this is a seam for future `madvise`-based tuning.

pub struct PageCache;

impl PageCache {
    pub fn new() -> Self {
        Self
    }

    pub fn prefetch_ahead(&self, _mmap_base: *const u8, _offset: u64, _bytes: usize) {
        // TODO: madvise(MADV_WILLNEED) on the next few MB
    }
}