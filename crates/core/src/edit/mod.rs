//! In-memory edit buffer. The mmap is read-only; all modifications live here
//! until `:w` writes them back. Edits change line numbering — `LineView` maps
//! logical (post-edit) line numbers to physical (original file) positions.

mod view;
pub mod save_task;
pub mod writeback;

pub use view::{LineEditor, LineView};

use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// A single line's worth of bytes, no trailing `\n`.
pub type LineBytes = Vec<u8>;

/// A single editable operation, for the undo/redo stacks.
#[derive(Debug, Clone)]
pub enum EditOp {
    Replace {
        line: u64,
        old: LineBytes,
        new: LineBytes,
    },
    Delete {
        line: u64,
        bytes: LineBytes,
    },
    /// Replace the content of a line INSIDE an inserted block (created by a
    /// split / paste). `anchor` + `index` address the block entry; the undo
    /// data is the same old/new bytes as [`EditOp::Replace`].
    ReplaceBlock {
        anchor: u64,
        index: usize,
        old: LineBytes,
        new: LineBytes,
    },
    /// Delete a line INSIDE an inserted block. Mirrors [`EditOp::Delete`] for
    /// lines that have no physical counterpart.
    DeleteBlock {
        anchor: u64,
        index: usize,
        bytes: LineBytes,
    },
    Insert {
        /// The line AFTER which we inserted. `u64::MAX` = inserted before line 0.
        after: u64,
        /// Position inside the inserted block where the lines go. Appends when
        /// equal to the block's current length (the normal case).
        index: usize,
        lines: Vec<LineBytes>,
    },
    /// A group of ops applied atomically (one undo/redo step) — the GUI wraps
    /// multi-op actions (split line, join lines, paste, selection replace)
    /// in a batch so Ctrl+Z undoes the whole action at once.
    Batch {
        ops: Vec<EditOp>,
    },
}

#[derive(Debug, Default, Clone)]
pub struct EditBuffer {
    /// Line replacements (line_no -> new bytes, no trailing \n).
    pub replaced: BTreeMap<u64, LineBytes>,
    /// Deleted line numbers.
    pub deleted: BTreeSet<u64>,
    /// Lines inserted after the given original line number.
    /// Use `u64::MAX` to represent "before line 0".
    pub inserted: BTreeMap<u64, Vec<LineBytes>>,
    /// Yank stack (most recent at the back). Each entry is one or more lines.
    pub yank_stack: Vec<Vec<LineBytes>>,
    /// Undo stack (newest at the back).
    pub undo_stack: VecDeque<EditOp>,
    /// Redo stack (newest at the back). Cleared whenever a fresh edit happens.
    pub redo_stack: VecDeque<EditOp>,
    /// When `Some`, undo-recorded ops collect here and are pushed as a single
    /// [`EditOp::Batch`] by `end_batch` — one undo/redo step per user action.
    pub batch: Option<Vec<EditOp>>,
    /// Set to true on any modification. Drives the status-bar `[Modified]` flag.
    pub dirty: bool,
    /// Eagerly maintained breakpoint cache. See `EditMapping`. Built once at
    /// construction and rebuilt by `rebuild_mapping` after structural
    /// mutations (insert/delete/undo of those).
    pub mapping: crate::edit::view::EditMapping,
}

impl EditBuffer {
    /// Rebuild the breakpoint cache from current `inserted` + `deleted`.
    /// Called by `LineEditor` after every structural change.
    pub fn rebuild_mapping(&mut self) {
        self.mapping = crate::edit::view::EditMapping::compute(&self.inserted, &self.deleted);
    }
}

impl EditBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        !self.dirty
            && self.replaced.is_empty()
            && self.deleted.is_empty()
            && self.inserted.is_empty()
    }

    pub fn clear(&mut self) {
        self.replaced.clear();
        self.deleted.clear();
        self.inserted.clear();
        self.yank_stack.clear();
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.batch = None;
        self.dirty = false;
        self.mapping = crate::edit::view::EditMapping::compute(&self.inserted, &self.deleted);
    }

    /// Invalidate the breakpoint cache. Call after any structural mutation.
    pub fn invalidate_mapping(&mut self) {
        self.rebuild_mapping();
    }

    /// Total modification ops pushed onto the undo stack (cheap signal of "is edited").
    pub fn edit_count(&self) -> usize {
        self.replaced.len() + self.deleted.len() + self.inserted.len()
    }

    /// Net line count delta: insertions minus deletions.
    /// Does NOT account for replaced (replaced doesn't change line count).
    pub fn net_line_delta(&self) -> i64 {
        let ins: i64 = self.inserted.values().map(|v| v.len() as i64).sum();
        let del: i64 = self.deleted.len() as i64;
        ins - del
    }

    /// Yank a single line onto the yank stack (clones the bytes).
    pub fn yank_line(&mut self, bytes: LineBytes) {
        self.yank_stack.push(vec![bytes]);
    }

    /// Yank multiple lines (used by V visual-line).
    pub fn yank_lines(&mut self, lines: Vec<LineBytes>) {
        if !lines.is_empty() {
            self.yank_stack.push(lines);
        }
    }

    /// Pop the most recent yank. Empty = nothing yanked.
    pub fn take_yank(&mut self) -> Option<Vec<LineBytes>> {
        self.yank_stack.pop()
    }

    /// Push an op onto the undo stack. Caps at 1024.
    ///
    /// Consecutive [`EditOp::Replace`] ops on the SAME physical line are merged
    /// into one step (kept: the oldest `old`, the newest `new`), so a typing
    /// burst is undone as a single keystroke. When a batch is open the op is
    /// collected into the batch instead of the stack.
    pub fn push_undo(&mut self, op: EditOp) {
        if let Some(batch) = &mut self.batch {
            merge_or_push(batch, op);
            return;
        }
        merge_or_push_deque(&mut self.undo_stack, op);
        if self.undo_stack.len() >= 1024 {
            self.undo_stack.pop_front();
        }
    }

    /// Pop one undo op.
    pub fn pop_undo(&mut self) -> Option<EditOp> {
        self.undo_stack.pop_back()
    }

    /// Push an op onto the redo stack. Caps at 1024.
    pub fn push_redo(&mut self, op: EditOp) {
        if self.redo_stack.len() >= 1024 {
            self.redo_stack.pop_front();
        }
        self.redo_stack.push_back(op);
    }

    /// Pop one redo op.
    pub fn pop_redo(&mut self) -> Option<EditOp> {
        self.redo_stack.pop_back()
    }

    /// A fresh edit invalidates the redo history.
    pub fn clear_redo(&mut self) {
        self.redo_stack.clear();
    }

    /// Start recording a batch of ops as ONE undo/redo step. Nested calls are
    /// an error (asserted). Also clears the redo history.
    pub fn begin_batch(&mut self) {
        debug_assert!(self.batch.is_none(), "nested edit batch");
        self.batch = Some(Vec::new());
        self.redo_stack.clear();
    }

    /// Close the batch opened by [`EditBuffer::begin_batch`]. Pushes the
    /// collected ops as a single [`EditOp::Batch`]. Returns the op count.
    pub fn end_batch(&mut self) -> usize {
        match self.batch.take() {
            Some(ops) if !ops.is_empty() => {
                let n = ops.len();
                self.push_undo(EditOp::Batch { ops });
                n
            }
            _ => 0,
        }
    }

    /// Whether an undo/redo op is currently being applied (true inside the
    /// engine's undo/redo); fresh edits may check this to decide redo-clear.
    pub fn undo_count(&self) -> usize {
        self.undo_stack.len()
    }
}

/// Push `op` into an open batch, merging a consecutive same-line Replace so a
/// typing burst inside one action stays a single undo step.
fn merge_or_push(v: &mut Vec<EditOp>, op: EditOp) {
    if let EditOp::Replace { line, new, .. } = &op {
        if let Some(EditOp::Replace { line: pl, new: pn, .. }) = v.last_mut() {
            if *pl == *line {
                *pn = new.clone();
                return;
            }
        }
    }
    if let EditOp::ReplaceBlock { anchor, index, new, .. } = &op {
        if let Some(EditOp::ReplaceBlock { anchor: pa, index: pi, new: pn, .. }) = v.last_mut() {
            if *pa == *anchor && *pi == *index {
                *pn = new.clone();
                return;
            }
        }
    }
    v.push(op);
}

/// Same merge for the undo deque (the non-batched path).
fn merge_or_push_deque(d: &mut VecDeque<EditOp>, op: EditOp) {
    if let EditOp::Replace { line, new, .. } = &op {
        if let Some(EditOp::Replace { line: pl, new: pn, .. }) = d.back_mut() {
            if *pl == *line {
                *pn = new.clone();
                return;
            }
        }
    }
    if let EditOp::ReplaceBlock { anchor, index, new, .. } = &op {
        if let Some(EditOp::ReplaceBlock { anchor: pa, index: pi, new: pn, .. }) = d.back_mut() {
            if *pa == *anchor && *pi == *index {
                *pn = new.clone();
                return;
            }
        }
    }
    d.push_back(op);
}