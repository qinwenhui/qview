//! User annotations attached to log files.
//!
//! An annotation is a selection of file content (one character up to many
//! lines) plus a user note.  This module owns the **data**: the [`Annotation`]
//! model and the [`AnnotationStore`] (load / save / query).  It is pure logic —
//! no UI — so the GUI and any future TUI share the same structure and the same
//! on-disk format.
//!
//! ## Storage
//!
//! All annotations live in ONE JSON file in the app data directory (central
//! store — log files are often in read-only directories, so a sidecar next to
//! the log is unreliable).  Each annotation is keyed by the *canonical* file
//! path ([`AnnotationStore::file_key`]); one file can hold any number of
//! annotations.  Writes are atomic (temp file + rename), so a crash never
//! truncates the store.  A missing or corrupt store simply loads as empty —
//! annotation data must never prevent the viewer from starting.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Upper bound for a `selected_text` snapshot (bytes).  A selection can span
/// thousands of lines; storing it all would bloat the store.  The GUI
/// truncates snapshots above this cap before saving.
pub const MAX_SELECTED_SNAPSHOT: usize = 4 * 1024;

/// One annotation: a content selection + the user's note.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Annotation {
    /// Monotonic global id — stable reference for update/delete.
    pub id: u64,
    /// `file_key` of the file this annotation belongs to (canonical path).
    pub file_key: String,
    /// Byte offset range in the source file (computed at creation from the
    /// selection's line/column via the engine's line model).
    pub start_byte: u64,
    pub end_byte: u64,
    /// 0-based line range — direct display + jump coordinates.
    pub start_line: u64,
    pub end_line: u64,
    /// 0-based character columns (matches the GUI selection model).
    pub start_col: usize,
    pub end_col: usize,
    /// Snapshot of the selected content (survives file changes).
    pub selected_text: String,
    /// The user's note body.
    pub text: String,
    /// Creation time, LOCAL, formatted `2026-08-06 10:23:45.123`.
    pub created_at: String,
    /// Marker color index (0 = default).  Reserved for later.
    pub color: u32,
    /// Set after an edit-save could not re-anchor this annotation to the file
    /// (its selected text is no longer found). The list shows it as stale; the
    /// snapshot still lets the user see what was annotated.
    #[serde(default)]
    pub stale: bool,
}

/// In-memory store of every annotation across every file.
///
/// Per-file annotation lists stay sorted by `start_byte` (insert is a binary
/// search + insert), so rendering and the list panel iterate in file order.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AnnotationStore {
    /// Per-file annotations, keyed by `file_key`.
    files: std::collections::HashMap<String, Vec<Annotation>>,
    /// Next id to hand out (monotonic across the store's lifetime).
    next_id: u64,
    /// Disk path this store was loaded from / saves to.
    #[serde(skip)]
    path: Option<std::path::PathBuf>,
}

impl AnnotationStore {
    /// An empty store with no backing path (must call `load` to persist).
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve a file path to its stable store key: the canonical path when the
    /// file exists, else the absolute path, else the raw path.  Never fails.
    pub fn file_key(path: &Path) -> String {
        std::fs::canonicalize(path)
            .or_else(|_| std::path::absolute(path))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned())
    }

    /// Load the store from `path`.  A missing or corrupt file yields an empty
    /// store (already bound to `path`, so the next `save` recreates it).
    pub fn load(path: &Path) -> Self {
        let mut store = match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str::<AnnotationStore>(&s)
                .unwrap_or_default(),
            Err(_) => Self::new(),
        };
        store.path = Some(path.to_path_buf());
        store
    }

    /// Persist atomically: write a temp file in the same directory, then
    /// rename over the target.  Returns an error only on real I/O failure.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension(format!(
            "{}.tmp",
            path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default()
        ));
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Annotations for `path`, in file order (by `start_byte`).
    pub fn for_file(&self, path: &Path) -> &[Annotation] {
        let key = Self::file_key(path);
        self.files.get(&key).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Number of annotations on `path`.
    pub fn count(&self, path: &Path) -> usize {
        self.for_file(path).len()
    }

    /// Add an annotation (without id — one is assigned and returned).
    /// `a.file_key` is ignored; the key comes from `path`.  The per-file list
    /// stays sorted by `(start_byte, id)`.
    pub fn add(&mut self, path: &Path, mut a: Annotation) -> u64 {
        let key = Self::file_key(path);
        a.file_key = key.clone();
        a.id = self.next_id;
        self.next_id += 1;

        let id = a.id;
        let list = self.files.entry(key).or_default();
        let pos = list.partition_point(|e| (e.start_byte, e.id) < (a.start_byte, a.id));
        list.insert(pos, a);
        id
    }

    /// Remove the annotation with `id` on `path`.  Returns `true` if found.
    pub fn remove(&mut self, path: &Path, id: u64) -> bool {
        let key = Self::file_key(path);
        let Some(list) = self.files.get_mut(&key) else {
            return false;
        };
        let before = list.len();
        list.retain(|a| a.id != id);
        if list.len() != before {
            if list.is_empty() {
                self.files.remove(&key);
            }
            true
        } else {
            false
        }
    }

    /// Replace the note body of an annotation.  Returns `false` if not found.
    pub fn set_text(&mut self, path: &Path, id: u64, text: String) -> bool {
        let key = Self::file_key(path);
        let Some(list) = self.files.get_mut(&key) else {
            return false;
        };
        match list.iter_mut().find(|a| a.id == id) {
            Some(a) => {
                a.text = text;
                true
            }
            None => false,
        }
    }

    /// Mark an annotation as stale (its anchor could not be re-located after an
    /// edit-save).  Returns `false` if not found.
    pub fn set_stale(&mut self, path: &Path, id: u64, stale: bool) -> bool {
        let key = Self::file_key(path);
        let Some(list) = self.files.get_mut(&key) else {
            return false;
        };
        match list.iter_mut().find(|a| a.id == id) {
            Some(a) => {
                a.stale = stale;
                true
            }
            None => false,
        }
    }

    /// Update an annotation's anchored position after an edit-save re-anchor.
    /// The per-file list is re-sorted by the new `start_byte`.  Returns false
    /// if not found.
    pub fn update_position(
        &mut self,
        path: &Path,
        id: u64,
        start_byte: u64,
        end_byte: u64,
        start_line: u64,
        end_line: u64,
        start_col: usize,
        end_col: usize,
    ) -> bool {
        let key = Self::file_key(path);
        let Some(list) = self.files.get_mut(&key) else {
            return false;
        };
        let Some(ann) = list.iter_mut().find(|a| a.id == id) else {
            return false;
        };
        ann.start_byte = start_byte;
        ann.end_byte = end_byte;
        ann.start_line = start_line;
        ann.end_line = end_line;
        ann.start_col = start_col;
        ann.end_col = end_col;
        ann.stale = false;
        // Re-sort by the new position.
        list.sort_by_key(|a| (a.start_byte, a.id));
        true
    }
}

/// Find the occurrence of `needle` in `haystack` that is CLOSEST to `near`
/// (byte offset).  Searches a bounded window around `near` so a re-anchor over
/// many annotations never scans the whole file.  Returns `None` when the
/// needle isn't found in the window (the caller marks the annotation stale).
pub fn find_nearest(haystack: &[u8], needle: &[u8], near: u64) -> Option<u64> {
    const WINDOW: usize = 8 * 1024 * 1024; // 8 MiB each side
    if needle.is_empty() || haystack.is_empty() {
        return None;
    }
    let n = near as usize;
    let lo = n.saturating_sub(WINDOW);
    let hi = (n + WINDOW + needle.len()).min(haystack.len());
    if lo >= hi {
        return None;
    }
    let window = &haystack[lo..hi];
    let mut best: Option<u64> = None;
    for m in memchr::memmem::find_iter(window, needle) {
        let abs = (lo + m) as u64;
        let d = abs.abs_diff(near);
        match best {
            Some(b) if b.abs_diff(near) <= d => {}
            _ => best = Some(abs),
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ann(start_byte: u64, start_line: u64) -> Annotation {
        Annotation {
            id: 0,
            file_key: String::new(),
            start_byte,
            end_byte: start_byte + 10,
            start_line,
            end_line: start_line,
            start_col: 0,
            end_col: 10,
            selected_text: "selected".into(),
            text: "note".into(),
            created_at: "2026-08-06 10:00:00.000".into(),
            color: 0,
            stale: false,
        }
    }

    fn tmp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("qview_annot_{}_{}.json", name, std::process::id()))
    }

    #[test]
    fn add_sorts_by_start_byte() {
        let mut s = AnnotationStore::new();
        let p = PathBuf::from("C:/logs/app.log");
        let id0 = s.add(&p, ann(500, 50));
        let id1 = s.add(&p, ann(100, 10));
        let id2 = s.add(&p, ann(300, 30));
        assert_ne!(id0, id1);
        assert_ne!(id1, id2);
        let list = s.for_file(&p);
        let bytes: Vec<u64> = list.iter().map(|a| a.start_byte).collect();
        assert_eq!(bytes, vec![100, 300, 500]);
        // ids assigned monotonically in insertion order.
        assert_eq!(list[0].id, id1);
        assert_eq!(list[2].id, id0);
    }

    #[test]
    fn files_are_isolated() {
        let mut s = AnnotationStore::new();
        s.add(&PathBuf::from("a.log"), ann(1, 1));
        s.add(&PathBuf::from("b.log"), ann(2, 2));
        assert_eq!(s.count(&PathBuf::from("a.log")), 1);
        assert_eq!(s.count(&PathBuf::from("b.log")), 1);
        assert_eq!(s.count(&PathBuf::from("c.log")), 0);
    }

    #[test]
    fn remove_and_set_text() {
        let mut s = AnnotationStore::new();
        let p = PathBuf::from("x.log");
        let id = s.add(&p, ann(10, 1));
        assert!(s.set_text(&p, id, "updated".into()));
        assert_eq!(s.for_file(&p)[0].text, "updated");
        assert!(!s.set_text(&p, 999, "nope".into()));

        assert!(s.remove(&p, id));
        assert_eq!(s.count(&p), 0);
        assert!(!s.remove(&p, id));
    }

    #[test]
    fn persist_round_trip() {
        let path = tmp_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        let p = PathBuf::from("r.log");
        {
            let mut s = AnnotationStore::load(&path);
            s.add(&p, ann(1, 1));
            s.add(&p, ann(2, 2));
            s.save().unwrap();
        }
        let loaded = AnnotationStore::load(&path);
        assert_eq!(loaded.count(&p), 2);
        let ids: Vec<u64> = loaded.for_file(&p).iter().map(|a| a.id).collect();
        assert_eq!(ids, vec![0, 1]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_missing_or_corrupt_is_empty() {
        let path = tmp_path("corrupt");
        std::fs::write(&path, b"{ not valid json ]").unwrap();
        let s = AnnotationStore::load(&path);
        assert_eq!(s.files.len(), 0);
        // Save over the corrupt file must recover it.
        assert!(s.save().is_ok());
        let _ = std::fs::remove_file(&path);

        let gone = tmp_path("missing");
        let _ = std::fs::remove_file(&gone);
        let s = AnnotationStore::load(&gone);
        assert_eq!(s.files.len(), 0);
    }

    #[test]
    fn find_nearest_picks_the_closest_match() {
        let hay = b"alpha NEEDLE middle NEEDLE omega";
        // Byte positions of the two occurrences:
        //   "alpha " = 6, so first at 6..12; second after "alpha NEEDLE middle "
        let first = 6u64;
        assert_eq!(find_nearest(hay, b"NEEDLE", first), Some(first));
        assert_eq!(find_nearest(hay, b"NEEDLE", 30), Some(first + 14));
        assert_eq!(find_nearest(hay, b"MISSING", 0), None);
        assert_eq!(find_nearest(hay, b"", 0), None);
    }

    #[test]
    fn update_position_resorts_and_clears_stale() {
        let mut s = AnnotationStore::new();
        let p = PathBuf::from("s.log");
        let id = s.add(&p, ann(100, 10));
        s.set_stale(&p, id, true);
        assert!(s.for_file(&p)[0].stale);

        assert!(s.update_position(&p, id, 500, 510, 50, 50, 1, 5));
        let a = &s.for_file(&p)[0];
        assert_eq!(a.start_byte, 500);
        assert_eq!(a.start_line, 50);
        assert!(!a.stale);
        assert!(!s.update_position(&p, 999, 0, 0, 0, 0, 0, 0));
    }

    #[test]
    fn file_key_never_fails() {
        // Non-existent path → absolute fallback, still a usable key.
        let k = AnnotationStore::file_key(Path::new("no_such_dir/no_such_file.log"));
        assert!(!k.is_empty());
        // Two references to the same existing file key identically.
        let dir = std::env::temp_dir();
        let a = dir.join("qview_key_test.log");
        let _ = std::fs::write(&a, b"hi");
        let b = dir.join(".").join("qview_key_test.log");
        assert_eq!(AnnotationStore::file_key(&a), AnnotationStore::file_key(&b));
        let _ = std::fs::remove_file(&a);
    }
}
