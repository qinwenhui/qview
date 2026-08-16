//! Benchmark cold-cache open time for a large file.
//! Usage: bench-open <path>

use std::env;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use rayon::prelude::*;

use qview_core::file::{IndexBuilder, MmapBackend};

fn main() -> Result<()> {
    let path = PathBuf::from(env::args().nth(1).expect("usage: bench-open <path>"));
    let meta = std::fs::metadata(&path)?;
    let file_size = meta.len();
    println!("file: {}  size: {} ({:.2} MiB)", path.display(), file_size, file_size as f64 / 1024.0 / 1024.0);

    // === Method 1: mmap + par_chunks (current) ===
    let t0 = Instant::now();
    let mmap = MmapBackend::open(&path)?;
    let t_mmap_open = t0.elapsed();
    println!("[mmap]  open()                          : {:?}", t_mmap_open);

    let builder = IndexBuilder::new(mmap.clone());
    let t0 = Instant::now();
    let _offsets = builder.build_with_progress(|_, _| {})?;
    let t_mmap_build = t0.elapsed();
    println!("[mmap]  build_index (par_chunks scan)   : {:?}", t_mmap_build);

    let _ = mmap; // keep alive

    // === Method 2: direct File reads + parallel ===
    println!("\n--- method 2: direct File reads ---");
    let path2 = path.clone();
    let t0 = Instant::now();
    let chunk_results: Vec<Vec<u64>> = (0..file_size_div_chunks(file_size, 32 * 1024 * 1024))
        .into_par_iter()
        .map(|i| {
            let start = (i as u64) * (32u64 * 1024 * 1024);
            let end = ((start + 32 * 1024 * 1024) as u64).min(file_size);
            let len = (end - start) as usize;
            let mut file = File::open(&path2).unwrap();
            file.seek(SeekFrom::Start(start)).unwrap();
            let mut buf = vec![0u8; len];
            file.read_exact(&mut buf).unwrap();
            memchr::memchr_iter(b'\n', &buf)
                .map(|nl| start + nl as u64 + 1)
                .collect()
        })
        .collect();
    let t_read_build = t0.elapsed();
    let total_newlines: usize = chunk_results.iter().map(|v| v.len()).sum();
    println!("[read]  build_index (parallel File read): {:?}", t_read_build);
    println!("         newlines counted: {}", total_newlines);

    // === Method 3: single-threaded sequential read (no parallel) ===
    println!("\n--- method 3: single-thread sequential ---");
    let t0 = Instant::now();
    let mut file = File::open(&path)?;
    let mut buf = vec![0u8; 32 * 1024 * 1024];
    let mut total = 0usize;
    let mut pos = 0u64;
    while pos < file_size {
        let to_read = ((file_size - pos) as usize).min(buf.len());
        file.seek(SeekFrom::Start(pos))?;
        file.read_exact(&mut buf[..to_read])?;
        total += memchr::memchr_iter(b'\n', &buf[..to_read]).count();
        pos += to_read as u64;
    }
    let t_seq = t0.elapsed();
    println!("[seq]   single-thread sequential read   : {:?}", t_seq);
    println!("         newlines counted: {}", total);

    println!("\n=== summary ===");
    println!("  mmap  (current): {:?} total", t_mmap_open + t_mmap_build);
    println!("  read (parallel): {:?}", t_read_build);
    println!("  seq  (single) : {:?}", t_seq);

    Ok(())
}

fn file_size_div_chunks(size: u64, chunk: usize) -> usize {
    ((size as usize) + chunk - 1) / chunk
}