//! Bench: simulate 70M-line scroll using the new O(log B) EditMapping vs the
//! old O(P) approach (re-implemented inline for comparison).
//!
//! Usage: bench-logical-resolve <path>

use std::env;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use qview_core::edit::EditBuffer;
use qview_core::file::{IndexBuilder, LineIndex, MmapBackend};

fn main() -> Result<()> {
    let path = PathBuf::from(env::args().nth(1).expect("usage: bench-logical-resolve <path>"));
    println!("file: {}", path.display());

    let mmap = MmapBackend::open(&path)?;
    let builder = IndexBuilder::new(mmap.clone());
    let offsets = builder.build_with_progress(|_, _| {})?;
    let total = mmap.size();
    let total_lines = offsets.len() as u64 - 1;
    let index = LineIndex::from_vec(offsets, total);
    println!("lines: {}", total_lines);

    // Build a sparse edit set: 1000 deletions scattered across the file.
    let mut edits = EditBuffer::new();
    for i in 0..1000u64 {
        edits.deleted.insert(i * 70000);
    }
    edits.rebuild_mapping();
    println!(
        "edits: 1000 deletions; breakpoints={}",
        edits.mapping.breakpoints.len()
    );

    // Simulate scrolling: read 1000 random logical lines near the end.
    let samples: Vec<u64> = (0..1000u64)
        .map(|i| total_lines - 1 - i * 70)
        .collect();

    // Bench 1: EditMapping.resolve (new O(log B))
    let t0 = Instant::now();
    let mut ok = 0usize;
    for &n in &samples {
        if edits.mapping.resolve(&edits.inserted, n, total_lines).is_some() {
            ok += 1;
        }
    }
    let dt_new = t0.elapsed();
    println!("new O(log B): {:?} ({} resolved)", dt_new, ok);

    // Bench 2: naive O(P) — re-implementation of the OLD algorithm.
    let t0 = Instant::now();
    let mut ok2 = 0usize;
    for &n in &samples {
        if old_resolve(&mmap, &index, &edits, n, total_lines).is_some() {
            ok2 += 1;
        }
    }
    let dt_old = t0.elapsed();
    println!("old O(P):     {:?} ({} resolved)", dt_old, ok2);

    println!(
        "speedup: {:.1}x",
        dt_old.as_secs_f64() / dt_new.as_secs_f64().max(1e-9)
    );

    // Bench 3: read_line-like operation (full path) on 1000 lines near end.
    let t0 = Instant::now();
    for &n in &samples {
        let r = edits.mapping.resolve(&edits.inserted, n, total_lines);
        if let Some((phys, _blk)) = r {
            if let Some(p) = phys {
                let _start = index.offset_of_line(p);
            }
        }
    }
    println!("full read_line (1000 lines near EOF): {:?}", t0.elapsed());

    Ok(())
}

fn old_resolve(
    _mmap: &MmapBackend,
    index: &LineIndex,
    edits: &EditBuffer,
    n: u64,
    max_phys: u64,
) -> Option<()> {
    let mut remaining = n;
    if let Some(lines) = edits.inserted.get(&u64::MAX) {
        let c = lines.len() as u64;
        if remaining < c {
            return Some(());
        }
        remaining -= c;
    }
    let mut k = 0u64;
    loop {
        if k >= max_phys {
            return None;
        }
        if edits.deleted.contains(&k) {
            if let Some(lines) = edits.inserted.get(&k) {
                let c = lines.len() as u64;
                if remaining < c {
                    return Some(());
                }
                remaining -= c;
            }
            k += 1;
            continue;
        }
        if remaining == 0 {
            let _ = index.offset_of_line(k);
            return Some(());
        }
        remaining -= 1;
        if let Some(lines) = edits.inserted.get(&k) {
            let c = lines.len() as u64;
            if remaining < c {
                return Some(());
            }
            remaining -= c;
        }
        k += 1;
    }
}