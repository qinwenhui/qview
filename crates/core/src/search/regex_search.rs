//! Regex search via `regex::bytes::Regex` — works on raw bytes, no UTF-8 required.
use regex::bytes::Regex;

pub struct RegexSearch {
    pub regex: Regex,
}

impl RegexSearch {
    pub fn new(pattern: &str) -> Result<Self, regex::Error> {
        Ok(Self {
            regex: Regex::new(pattern)?,
        })
    }

    pub fn find_all(&self, haystack: &[u8]) -> Vec<std::ops::Range<usize>> {
        self.regex.find_iter(haystack).map(|m| m.range()).collect()
    }
}