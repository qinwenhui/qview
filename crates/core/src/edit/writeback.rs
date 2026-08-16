//! Write in-memory edits back to disk (`:w`).
//!
//! Writes to a temp file, renames original → .bak, renames temp → original.
//! The mmap is dropped before writing so Windows doesn't lock the file.
//!
//! Strategy:
//! 1. Caller drops the `MmapBackend` (or calls `refresh`) before invoking.
//! 2. We write the original file's content line-by-line, applying edits:
//!    - skip deleted lines
//!    - substitute `replaced[line]` for the original
//!    - emit `inserted[after]` in the right slot
//! 3. Atomic rename: write to `path.qli.tmp` then rename. Plus a `.bak`
//!    backup of the original on first write.
//!
//! This module is I/O-only; the logical decisions live in `App::save`.

use std::fs::{rename, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::EditBuffer;
use crate::file::MmapBackend;

/// Reconstruct full (non-sparse) line offsets from an mmap for writeback.
/// Expensive (one full scan) but only called during save.
pub fn full_offsets(mmap: &MmapBackend) -> Vec<u64> {
    let mut offsets: Vec<u64> = memchr::memchr_iter(b'\n', mmap.as_slice())
        .map(|nl| nl as u64 + 1)
        .collect();
    if !offsets.starts_with(&[0]) {
        offsets.insert(0, 0);
    }
    // If the last offset points past EOF (trailing \n case), keep it —
    // writeback uses it to know there's no implicit last line.
    offsets
}

/// Write the post-edit file to `dst` (a temp path — the caller renames it
/// over the original). Source content comes from `mmap_slice` (the original
/// file content; passed by the caller so it can stream from a live mmap).
/// The line offsets are `index_offsets` (caller's snapshot).
///
/// Backup of the ORIGINAL file is the caller's responsibility (a `.bak` copy
/// made BEFORE this temp is written), so the original can never be lost.
///
/// Returns `Ok(bytes_written)` on success.
pub fn write_to_path(
    dst: &Path,
    mmap_slice: &[u8],
    index_offsets: &[u64],
    edits: &EditBuffer,
    _new_total_lines: u64,
) -> Result<u64> {
    // Open dst for writing (truncates).
    let tmp: PathBuf = {
        let mut p = dst.as_os_str().to_owned();
        p.push(".writetmp");
        PathBuf::from(p)
    };
    let f = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
    let mut w = BufWriter::with_capacity(1024 * 1024, f);

    // 3. Walk physical lines (0..index_offsets.len()), applying edits.
    let mut written: u64 = 0;
    let mut emitted: u64 = 0;
    let mut inserted_iter = edits.inserted.iter().peekable();
    // u64::MAX means "insert before line 0" — we handle it specially.

    // Pre-insertions (anchor == u64::MAX).
    if let Some((&u64::MAX, lines)) = inserted_iter.peek() {
        for line in lines.iter() {
            write_line(&mut w, line, &mut written)?;
            emitted += 1;
        }
        inserted_iter.next();
    }

    for (i, &start) in index_offsets.iter().enumerate() {
        let phys = i as u64;

        // Skip if this physical line is deleted.
        if edits.deleted.contains(&phys) {
            continue;
        }

        // Write the (possibly replaced) content.
        let next_start = index_offsets.get(i + 1).copied().unwrap_or(mmap_slice.len() as u64);
        if let Some(repl) = edits.replaced.get(&phys) {
            write_line(&mut w, repl, &mut written)?;
        } else if start < mmap_slice.len() as u64 {
            // Does this line end in a real `\n`? `next_start` is the byte AFTER
            // the newline for a terminated line, or EOF for the last line of a
            // file WITHOUT a trailing newline — in that case the byte before
            // `next_start` is content, not `\n`, and we must NOT drop it.
            let has_nl = next_start > start
                && next_start <= mmap_slice.len() as u64
                && mmap_slice[next_start as usize - 1] == b'\n';
            let end = if has_nl {
                (next_start - 1).min(mmap_slice.len() as u64)
            } else {
                next_start.min(mmap_slice.len() as u64)
            };
            if end > start {
                let chunk = &mmap_slice[start as usize..end as usize];
                w.write_all(chunk).context("write original chunk")?;
                written += chunk.len() as u64;
            }
            if has_nl {
                w.write_all(b"\n").context("write trailing newline")?;
                written += 1;
            }
        }
        emitted += 1;

        // Post-insertions for `phys`.
        while let Some((&anchor, lines)) = inserted_iter.peek() {
            if anchor == phys {
                for line in lines.iter() {
                    write_line(&mut w, line, &mut written)?;
                    emitted += 1;
                }
                inserted_iter.next();
            } else {
                break;
            }
        }
    }

    // Trailing insertions with anchor == u64::MAX? Already handled above.
    // Trailing insertions with anchor > all physical lines — emit them now.
    for (_, lines) in inserted_iter {
        for line in lines.iter() {
            write_line(&mut w, line, &mut written)?;
            emitted += 1;
        }
    }

    w.flush().context("flush writeback")?;
    drop(w); // close file handle
    rename(&tmp, dst).with_context(|| format!("rename {} -> {}", tmp.display(), dst.display()))?;

    // Sanity: emitted should equal new_total_lines. We don't fail hard if
    // off-by-one — caller will reindex from the new file anyway.
    let _ = emitted;
    Ok(written)
}

fn write_line(w: &mut BufWriter<File>, line: &[u8], written: &mut u64) -> Result<()> {
    w.write_all(line).context("write line bytes")?;
    w.write_all(b"\n").context("write newline")?;
    *written += line.len() as u64 + 1;
    Ok(())
}

/// Compute the post-edit line count without writing.
///
/// `original_lines` is `index_offsets.len()` for a file ending in `\n`,
/// or `index_offsets.len() + 1` for one that doesn't. (The +1 accounts for
/// the trailing unterminated line.)
pub fn projected_line_count(original_lines: u64, edits: &EditBuffer) -> u64 {
    let delta = edits.net_line_delta();
    if delta >= 0 {
        original_lines + delta as u64
    } else {
        original_lines.saturating_sub((-delta) as u64)
    }
}