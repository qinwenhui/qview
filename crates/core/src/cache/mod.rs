//! Cache layer: two-tier line cache + page prefetch placeholder.

pub mod line_cache;
pub mod page_cache;

pub use line_cache::{DisplayKey, DisplayLine, LineCache, RawLine};
pub use page_cache::PageCache;