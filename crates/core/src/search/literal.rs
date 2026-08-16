//! Literal substring search via `memchr::memmem` (SIMD on x86_64).
//! For multi-GB files, see `crate::search::run_search` which chunks and
//! parallelizes over rayon.

use memchr::memmem::Finder;

pub struct LiteralSearch<'a> {
    finder: Finder<'a>,
}

impl<'a> LiteralSearch<'a> {
    pub fn new(needle: &'a [u8]) -> Self {
        Self {
            finder: Finder::new(needle),
        }
    }

    pub fn find_all(&self, haystack: &[u8]) -> Vec<usize> {
        self.finder.find_iter(haystack).collect()
    }
}