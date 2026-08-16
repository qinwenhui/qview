//! Line number resolution after edits. Combines mmap content with EditBuffer
//! to produce post-edit bytes for any logical line number.

use std::collections::{BTreeMap, BTreeSet};

use crate::file::{LineIndex, MmapBackend};

use super::{EditBuffer, EditOp, LineBytes};

/// Maps logical → physical line numbers in O(log B) via sorted breakpoints.
///
/// `breakpoints[i]` is a physical line where the mapping changes.
/// `cumulative_offset[i]` is the net offset just before that breakpoint.
/// `pre_count` is lines inserted before physical line 0.
#[derive(Debug, Default, Clone)]
pub struct EditMapping {
    pub pre_count: u64,
    pub breakpoints: Vec<u64>,
    /// cumulative_offset[i] = sum_{j<i} delta[j] = offset just before
    /// breakpoint[i]. For phys k where breakpoints[idx-1] < k < breakpoints[idx],
    /// `offset(k) = cumulative_offset[idx]`.
    pub cumulative_offset: Vec<i64>,
}

impl EditMapping {
    pub fn compute(
        inserted: &BTreeMap<u64, Vec<LineBytes>>,
        deleted: &BTreeSet<u64>,
    ) -> Self {
        let pre_count = inserted.get(&u64::MAX).map(|v| v.len() as u64).unwrap_or(0);

        // Collect breakpoints (physical line numbers where offset changes):
        // - deleted[k]: at k, the offset jumps by -1 (we skip physical k).
        // - inserted[anchor]: at k = anchor + 1, the offset jumps by +count.
        //
        // We use BTreeMap to dedup breakpoints and accumulate deltas.
        let mut bp_deltas: BTreeMap<u64, i64> = BTreeMap::new();
        for &d in deleted {
            // Deletion of physical line `d`: starting from phys d+1, every
            // physical line has its logical position shifted by -1 (because
            // the deleted line is gone). So the offset jumps at bp = d+1.
            *bp_deltas.entry(d + 1).or_insert(0) -= 1;
        }
        for (&anchor, lines) in inserted {
            if anchor == u64::MAX {
                continue;
            }
            // Insertion after phys `anchor`: starting from phys anchor+1, the
            // logical positions get shifted by +count. So the offset jumps
            // at bp = anchor+1.
            let count = lines.len() as i64;
            *bp_deltas.entry(anchor + 1).or_insert(0) += count;
        }

        let mut breakpoints: Vec<u64> = Vec::with_capacity(bp_deltas.len());
        let mut cumulative_offset: Vec<i64> = Vec::with_capacity(bp_deltas.len() + 1);
        // offset_at_phys_just_before_first_breakpoint = 0
        cumulative_offset.push(0);
        for (bp, delta) in &bp_deltas {
            breakpoints.push(*bp);
            let last = *cumulative_offset.last().unwrap();
            cumulative_offset.push(last + delta);
        }

        Self {
            pre_count,
            breakpoints,
            cumulative_offset,
        }
    }

    /// Total number of inserted lines (across all anchors, including pre-block).
    pub fn total_inserted(&self, inserted: &BTreeMap<u64, Vec<LineBytes>>) -> u64 {
        inserted.values().map(|v| v.len() as u64).sum()
    }

    /// Resolve `logical n -> Option<phys>`. Returns None for inserted lines
    /// (caller should use `resolve` to get inserted-block info instead).
    pub fn logical_to_physical(
        &self,
        inserted: &BTreeMap<u64, Vec<LineBytes>>,
        n: u64,
        max_phys: u64,
    ) -> Option<u64> {
        let (phys, blk) = self.resolve(inserted, n, max_phys)?;
        if blk.is_some() {
            None
        } else {
            phys
        }
    }

    /// Resolve logical line `n` to physical. Returns:
    /// - `(Some(phys), None)` for an unmodified physical line.
    /// - `(None, Some((anchor, idx)))` for a line inside an inserted block.
    /// - `None` if past EOF.
    ///
    /// O(log B) via binary search over breakpoints.
    pub fn resolve(
        &self,
        inserted: &BTreeMap<u64, Vec<LineBytes>>,
        n: u64,
        max_phys: u64,
    ) -> Option<(Option<u64>, Option<(u64, usize)>)> {
        // Handle pre-block (inserted at u64::MAX).
        if n < self.pre_count {
            return Some((None, Some((u64::MAX, n as usize))));
        }
        let logical = n - self.pre_count;

        // No breakpoints? Then logical = physical 1:1 (modulo max_phys).
        if self.breakpoints.is_empty() {
            if logical >= max_phys {
                return None;
            }
            return Some((Some(logical), None));
        }

        let n_bp = self.breakpoints.len();
        // Binary search for `interval_idx`:
        // interval_idx is the largest i (in [0, n_bp]) such that
        // logical_pos(bp[i]) <= logical. (With bp[n_bp] = infinity, always true.)
        let mut lo: usize = 0;
        let mut hi: usize = n_bp + 1;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let pos = if mid < n_bp {
                self.breakpoints[mid] as i128 + self.cumulative_offset[mid + 1] as i128
            } else {
                i128::MAX
            };
            if pos <= logical as i128 {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let interval_idx = lo;
        let offset_in_interval = self.cumulative_offset[interval_idx];
        let bp_start: u64 = if interval_idx == 0 {
            0
        } else {
            self.breakpoints[interval_idx - 1]
        };
        let bp_end_exclusive: u64 = if interval_idx < n_bp {
            self.breakpoints[interval_idx]
        } else {
            max_phys
        };
        let logical_at_start = bp_start as i128 + offset_in_interval as i128;
        let mut cur_logical = logical_at_start;
        let mut k = bp_start;
        // Walk physical lines in [bp_start, bp_end_exclusive).
        while k < bp_end_exclusive {
            if logical == cur_logical as u64 {
                return Some((Some(k), None));
            }
            if logical < cur_logical as u64 {
                return None;
            }
            cur_logical += 1;
            if let Some(lines) = inserted.get(&k) {
                let count = lines.len() as i128;
                if (logical as i128) < cur_logical + count {
                    let idx = (logical as i128 - cur_logical) as usize;
                    return Some((None, Some((k, idx))));
                }
                cur_logical += count;
            }
            k += 1;
        }
        None
    }
}

/// Read-only resolver: takes a logical line number and returns its raw bytes.
pub struct LineView<'a> {
    pub mmap: &'a MmapBackend,
    pub index: &'a LineIndex,
    pub edits: &'a EditBuffer,
    pub mapping: &'a EditMapping,
}

impl<'a> LineView<'a> {
    pub fn new(
        mmap: &'a MmapBackend,
        index: &'a LineIndex,
        edits: &'a EditBuffer,
        mapping: &'a EditMapping,
    ) -> Self {
        Self {
            mmap,
            index,
            edits,
            mapping,
        }
    }

    /// Compute the post-edit total line count.
    pub fn line_count(&self, original_lines: u64) -> u64 {
        let delta = self.edits.net_line_delta();
        if delta >= 0 {
            original_lines + delta as u64
        } else {
            original_lines.saturating_sub((-delta) as u64)
        }
    }

    /// Get the bytes for logical line `n`. Returns None only if `n` is past EOF.
    pub fn resolve(&self, n: u64) -> Option<LineBytes> {
        let (phys, inserted_block) = self.logical_to_physical_with_block(n)?;
        if let Some((anchor, idx)) = inserted_block {
            return self.edits.inserted.get(&anchor).map(|v| v[idx].clone());
        }
        let phys = phys?;
        if let Some(repl) = self.edits.replaced.get(&phys) {
            return Some(repl.clone());
        }
        self.read_physical(phys)
    }

    /// Convert a post-edit logical line back to a physical line for searching.
    /// Returns None for inserted lines (caller should use `resolve` instead).
    pub fn logical_to_physical(&self, n: u64) -> Option<u64> {
        let (phys, blk) = self.logical_to_physical_with_block(n)?;
        if blk.is_some() {
            None
        } else {
            phys
        }
    }

    /// Read a physical (original) line from the mmap by line_no.
    /// Strips trailing `\r\n` or `\n` so the returned bytes contain only
    /// the visible text. Handles the last line with no trailing newline.
    ///
    /// Uses sparse-aware scanning: `offset_of_line` returns the nearest anchor
    /// (underestimate), and we scan forward from there to find the exact position.
    fn read_physical(&self, phys: u64) -> Option<LineBytes> {
        let slice = self.mmap.as_slice();
        let total = slice.len();

        // Resolve nearest sparse anchor at or before `phys`.
        let (anchor_byte, anchor_line) = self.index.resolve_anchor(phys);

        // Scan forward from anchor to find the exact start of `phys`.
        let mut pos = anchor_byte as usize;
        let mut current = anchor_line;
        let target = phys;
        while current < target && pos < total {
            match memchr::memchr(b'\n', &slice[pos..]) {
                Some(nl) => {
                    pos += nl + 1;
                    current += 1;
                }
                None => {
                    pos = total;
                    break;
                }
            }
        }

        if pos >= total && current < target {
            // Past EOF — but if pos == total and current == target, it's the
            // implicit empty last line after a trailing newline.
            if pos == total && current == target {
                return Some(Vec::new());
            }
            return None;
        }

        let start = pos;
        let line_end = match memchr::memchr(b'\n', &slice[pos..]) {
            Some(nl) => pos + nl + 1,
            None => total,
        };

        // Strip trailing \n (and \r for CRLF).
        let raw_end = if line_end > start && slice[line_end - 1] == b'\n' {
            line_end - 1
        } else {
            line_end
        };
        let end = if raw_end > start && slice[raw_end - 1] == b'\r' {
            raw_end - 1
        } else {
            raw_end
        };

        if end <= start {
            return Some(Vec::new());
        }
        Some(slice[start..end].to_vec())
    }

    /// O(log B) lookup. Returns (Some(phys), None) for normal physical lines,
    /// (None, Some((anchor, idx))) for inserted lines, None if past EOF.
    fn logical_to_physical_with_block(
        &self,
        n: u64,
    ) -> Option<(Option<u64>, Option<(u64, usize)>)> {
        self.mapping.resolve(&self.edits.inserted, n, self.index.line_count())
    }
}

/// Mutable edit operations. Carries mmap/index refs for snapshotting bytes.
pub struct LineEditor<'a> {
    pub mmap: &'a MmapBackend,
    pub index: &'a LineIndex,
    pub edits: &'a mut EditBuffer,
}

impl<'a> LineEditor<'a> {
    pub fn new(mmap: &'a MmapBackend, index: &'a LineIndex, edits: &'a mut EditBuffer) -> Self {
        Self { mmap, index, edits }
    }

    /// Read current bytes for a physical line (honors replaced).
    pub fn current_bytes(&self, phys_line: u64) -> Option<LineBytes> {
        if self.edits.deleted.contains(&phys_line) {
            return None;
        }
        if let Some(repl) = self.edits.replaced.get(&phys_line) {
            return Some(repl.clone());
        }
        let mapping = &self.edits.mapping;
        let view = LineView::new(self.mmap, self.index, self.edits, mapping);
        view.read_physical(phys_line)
    }

    /// Delete a single physical line. Returns the deleted bytes (for undo/yank).
    pub fn delete_line(&mut self, phys_line: u64) -> Option<LineBytes> {
        if self.edits.deleted.contains(&phys_line) {
            return None;
        }
        let bytes = self.current_bytes(phys_line)?;
        self.edits.deleted.insert(phys_line);
        self.edits.replaced.remove(&phys_line);
        self.edits.push_undo(EditOp::Delete {
            line: phys_line,
            bytes: bytes.clone(),
        });
        self.edits.dirty = true;
        self.edits.invalidate_mapping();
        Some(bytes)
    }

    /// Replace a physical line's content.
    pub fn replace_line(&mut self, phys_line: u64, new: LineBytes) -> Option<LineBytes> {
        let old = self.current_bytes(phys_line)?;
        if old == new {
            return None;
        }
        self.edits.deleted.remove(&phys_line);
        self.edits.replaced.insert(phys_line, new.clone());
        self.edits.push_undo(EditOp::Replace {
            line: phys_line,
            old: old.clone(),
            new,
        });
        self.edits.dirty = true;
        // No mapping change: replaced doesn't change line count.
        Some(old)
    }

    /// Insert lines after `after_phys`. Use `u64::MAX` to insert before line 0.
    pub fn insert_lines(&mut self, after_phys: u64, lines: Vec<LineBytes>) {
        if lines.is_empty() {
            return;
        }
        let key = if after_phys == u64::MAX { u64::MAX } else { after_phys };
        let entry = self.edits.inserted.entry(key).or_default();
        let index = entry.len();
        entry.extend(lines.iter().cloned());
        self.edits.push_undo(EditOp::Insert {
            after: after_phys,
            index,
            lines,
        });
        self.edits.dirty = true;
        self.edits.invalidate_mapping();
    }

    /// Insert a single line INSIDE an inserted block, right after the line at
    /// `index` within that block (used to split an already-inserted line).
    pub fn insert_line_in_block(&mut self, anchor: u64, index: usize, bytes: LineBytes) {
        let entry = self.edits.inserted.entry(anchor).or_default();
        let pos = (index + 1).min(entry.len());
        entry.insert(pos, bytes.clone());
        self.edits.push_undo(EditOp::Insert {
            after: anchor,
            index: pos,
            lines: vec![bytes],
        });
        self.edits.dirty = true;
        self.edits.invalidate_mapping();
    }

    /// Undo one operation. Returns true if anything was undone.
    pub fn undo(&mut self) -> bool {
        let op = match self.edits.pop_undo() {
            Some(o) => o,
            None => return false,
        };
        self.apply_inverse(&op);
        self.edits.push_redo(op);
        self.edits.dirty = self.edits.edit_count() > 0;
        true
    }

    /// Redo one operation. Returns true if anything was redone.
    pub fn redo(&mut self) -> bool {
        let op = match self.edits.pop_redo() {
            Some(o) => o,
            None => return false,
        };
        self.apply_forward(&op);
        self.edits.push_undo(op);
        self.edits.dirty = self.edits.edit_count() > 0;
        true
    }

    /// Apply an op's forward effect (redo path). Does not re-capture undo.
    fn apply_forward(&mut self, op: &EditOp) {
        match op {
            EditOp::Replace { line, new, .. } => {
                self.edits.replaced.insert(*line, new.clone());
            }
            EditOp::Delete { line, .. } => {
                self.edits.deleted.insert(*line);
                self.edits.replaced.remove(line);
                self.edits.invalidate_mapping();
            }
            EditOp::Insert { after, index, lines } => {
                let key = *after;
                let entry = self.edits.inserted.entry(key).or_default();
                let pos = (*index).min(entry.len());
                for (i, b) in lines.iter().enumerate() {
                    entry.insert(pos + i, b.clone());
                }
                self.edits.invalidate_mapping();
            }
            EditOp::ReplaceBlock { anchor, index, new, .. } => {
                if let Some(entry) = self.edits.inserted.get_mut(anchor) {
                    if *index < entry.len() {
                        entry[*index] = new.clone();
                    }
                }
            }
            EditOp::DeleteBlock { anchor, index, .. } => {
                if let Some(entry) = self.edits.inserted.get_mut(anchor) {
                    if *index < entry.len() {
                        entry.remove(*index);
                    }
                    if entry.is_empty() {
                        self.edits.inserted.remove(anchor);
                    }
                }
                self.edits.invalidate_mapping();
            }
            EditOp::Batch { ops } => {
                for sub in ops {
                    self.apply_forward(sub);
                }
            }
        }
    }

    /// Apply an op's inverse effect (undo path).
    fn apply_inverse(&mut self, op: &EditOp) {
        match op {
            EditOp::Replace { line, old, .. } => {
                self.edits.deleted.remove(line);
                self.edits.replaced.insert(*line, old.clone());
            }
            EditOp::Delete { line, .. } => {
                self.edits.deleted.remove(line);
                self.edits.invalidate_mapping();
            }
            EditOp::Insert { after, index, lines } => {
                let key = if *after == u64::MAX { u64::MAX } else { *after };
                if let Some(entry) = self.edits.inserted.get_mut(&key) {
                    let end = (*index + lines.len()).min(entry.len());
                    if *index < end {
                        entry.drain(*index..end);
                    }
                    if entry.is_empty() {
                        self.edits.inserted.remove(&key);
                    }
                }
                self.edits.invalidate_mapping();
            }
            EditOp::ReplaceBlock { anchor, index, old, .. } => {
                if let Some(entry) = self.edits.inserted.get_mut(anchor) {
                    if *index < entry.len() {
                        entry[*index] = old.clone();
                    }
                }
            }
            EditOp::DeleteBlock { anchor, index, bytes } => {
                let entry = self.edits.inserted.entry(*anchor).or_default();
                let pos = (*index).min(entry.len());
                entry.insert(pos, bytes.clone());
                self.edits.invalidate_mapping();
            }
            EditOp::Batch { ops } => {
                for sub in ops.iter().rev() {
                    self.apply_inverse(sub);
                }
            }
        }
    }
}