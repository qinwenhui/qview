//! Disk read-speed benchmark: measure the disk's max sequential throughput and
//! compare it against our windowed-mmap scan path, to see how much headroom the
//! scan has.
//!
//! Usage: `cargo run --release -p qview --bin disk_bench -- <file> [--raw|--read|--scan|--pread] [--threads N]`
//!
//! * `--raw`   `FILE_FLAG_NO_BUFFERING` aligned sequential read — the true disk
//!             ceiling (bypasses the OS file cache entirely).
//! * `--read`  buffered sequential read (64 MiB buffer) — the normal cached path.
//! * `--scan`  the production scan path ([`ScanReader`]: windowed NO_BUFFERING
//!             reads into a reusable buffer, counts `\n` per 64 MiB window).
//! * `--pread` parallel File reads in 32 MiB chunks (one handle per chunk).
//! * `--praw`  parallel NO_BUFFERING reads in 4 MiB aligned chunks (raw ceiling
//!             with deep queue depth — is single-threaded raw already the max?).
//! * `--build` the real production sparse-index build (parallel memchr + fused
//!             sampling) — the number the user actually sees.
//!
//! `disk_bench --gen <path> <size_gb>` writes a log-like file (repeating
//! timestamped lines) for realistic measurements.

use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--gen") {
        let path = args.get(2).expect("--gen <path> <size_gb>");
        let gb: u64 = args.get(3).expect("--gen <path> <size_gb>").parse().unwrap();
        gen_file(Path::new(path), gb * 1024 * 1024 * 1024);
        return;
    }
    let mut path: Option<String> = None;
    let mut only: Vec<String> = Vec::new();
    let mut threads: Option<u32> = None;
    let mut window_mb: Option<u64> = None;
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        if a == "--threads" {
            threads = args.get(i + 1).and_then(|s| s.parse().ok());
            i += 2;
            continue;
        }
        if a == "--window" {
            window_mb = args.get(i + 1).and_then(|s| s.parse().ok());
            i += 2;
            continue;
        }
        if let Some(m) = a.strip_prefix("--") {
            only.push(m.to_string());
        } else if path.is_none() {
            path = Some(a.clone());
        }
        i += 1;
    }
    let path = match path {
        Some(p) => p,
        None => {
            eprintln!("usage: disk_bench <file> [--raw|--read|--scan|--pread|--praw|--build]");
            std::process::exit(2);
        }
    };
    let file = Path::new(&path);
    let size = std::fs::metadata(file).map(|m| m.len()).expect("metadata");
    println!(
        "file: {path}   size: {:.2} GiB\n",
        size as f64 / 1073741824.0
    );

    let want = |m: &str| only.is_empty() || only.iter().any(|x| x == m);
    if want("raw") {
        run("raw unbuffered (disk ceiling)", read_raw, file);
    }
    if want("read") {
        run("buffered sequential read", read_buffered, file);
    }
    if want("scan") {
        run("ScanReader scan (new)", scan_new, file);
    }
    if want("pread") {
        run("parallel File reads", read_parallel, file);
    }
    if want("praw") {
        run("parallel raw (NO_BUFFERING)", read_raw_parallel, file);
    }
    if want("build") {
        if let Some(n) = threads {
            qview_core::parallel::set_scan_threads(n);
        }
        if let Some(mb) = window_mb {
            let _ = WINDOW_BYTES.set(mb * 1024 * 1024);
        }
        run("sparse index build (prod)", bench_build, file);
    }
    if want("scanread") {
        run("ScanReader read-only (touch)", scan_read, file);
    }
    if want("threads") {
        println!(
            "available_parallelism = {:?}   scan_pool threads = {}",
            std::thread::available_parallelism(),
            qview_core::parallel::scan_pool().current_num_threads()
        );
    }
}

/// Write a log-like file of repeating timestamped lines (~78 B/line).
fn gen_file(path: &Path, size: u64) {
    let line = b"2026-08-05 12:00:00.000 [INFO ] worker-12  request id=a4eb10b5 status=200 dur=12us\n";
    const BUF: usize = 1024 * 1024;
    let mut buf = Vec::with_capacity(BUF);
    while buf.len() < BUF {
        let n = (BUF - buf.len()).min(line.len());
        buf.extend_from_slice(&line[..n]);
    }
    let mut f = std::fs::File::create(path).expect("create");
    let mut written = 0u64;
    while written < size {
        let n = ((size - written) as usize).min(BUF);
        f.write_all(&buf[..n]).expect("write");
        written += n as u64;
    }
    f.sync_all().expect("sync");
    println!("generated {} ({} GiB)", path.display(), written as f64 / 1073741824.0);
}

/// Optional window override for `--build` (MiB), set by `--window N`.
static WINDOW_BYTES: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

/// The real production sparse-index build (uses the scan pool + ScanReader).
fn bench_build(path: &Path) -> (u64, Duration) {
    use qview_core::file::IndexBuilder;
    let mmap = qview_core::file::MmapBackend::open(path).expect("mmap");
    let size = mmap.size();
    let start = Instant::now();
    let mut builder = IndexBuilder::new(mmap);
    if let Some(w) = WINDOW_BYTES.get() {
        builder.set_scan_window(*w);
    }
    let out = builder
        .build_sparse_with_progress(|_, _| {})
        .expect("sparse build");
    let _ = out;
    (size, start.elapsed())
}

fn run(name: &str, f: impl Fn(&Path) -> (u64, Duration), file: &Path) {
    let (bytes, dur) = f(file);
    let gb = bytes as f64 / 1e9;
    let secs = dur.as_secs_f64();
    println!(
        "{name:<30} {:>8.2} GB   {:>6.2}s  →  {:>6.2} GB/s",
        gb, secs, gb / secs.max(1e-9)
    );
}

/// Cheap consumer so the compiler can't elide the reads WITHOUT a per-byte CPU
/// loop (which would bottleneck the benchmark instead of the disk). Touches one
/// byte per 4 KiB page.
fn touch(buf: &[u8]) -> u64 {
    buf.iter().step_by(4096).fold(0u64, |a, &b| a.wrapping_add(b as u64))
}

const BUF: usize = 64 * 1024 * 1024;

/// True disk ceiling: FILE_FLAG_NO_BUFFERING, sector-aligned reads.
#[cfg(windows)]
fn read_raw(path: &Path) -> (u64, Duration) {
    use std::os::windows::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x20000000) // FILE_FLAG_NO_BUFFERING
        .open(path)
        .expect("open no-buffering");
    let size = file.metadata().unwrap().len();
    let start = Instant::now();
    // Aligned buffer (4096 = multiple of any sector size).
    let mut storage = vec![0u8; BUF + 4096];
    let base = storage.as_mut_ptr() as usize;
    let off = (4096 - (base % 4096)) % 4096;
    let buf = &mut storage[off..off + BUF];
    let mut total = 0u64;
    let mut sum = 0u64;
    let aligned_end = size - (size % 512);
    let mut f = &file;
    while total < aligned_end {
        let want = ((aligned_end - total) as usize).min(BUF);
        match f.read(&mut buf[..want]) {
            Ok(n) if n > 0 => {
                sum = sum.wrapping_add(touch(&buf[..n]));
                total += n as u64;
            }
            _ => break,
        }
    }
    let _ = sum;
    (total, start.elapsed())
}

#[cfg(not(windows))]
fn read_raw(path: &Path) -> (u64, Duration) {
    read_buffered(path) // no NO_BUFFERING equivalent on this platform
}

/// Buffered sequential read (normal cached path).
fn read_buffered(path: &Path) -> (u64, Duration) {
    let mut f = std::fs::File::open(path).expect("open");
    let start = Instant::now();
    let mut buf = vec![0u8; BUF];
    let mut total = 0u64;
    let mut sum = 0u64;
    loop {
        match f.read(&mut buf) {
            Ok(n) if n > 0 => {
                sum = sum.wrapping_add(touch(&buf[..n]));
                total += n as u64;
            }
            _ => break,
        }
    }
    let _ = sum;
    (total, start.elapsed())
}

/// The production scan path: [`ScanReader`] windowed NO_BUFFERING reads into a
/// reusable aligned buffer, one 64 MiB window at a time.
fn scan_new(path: &Path) -> (u64, Duration) {
    use qview_core::file::{ScanReader, SCAN_WINDOW};
    let mut scanner = ScanReader::open(path).expect("ScanReader");
    let total = scanner.size();
    let start = Instant::now();
    let mut count = 0u64;
    let mut pos = 0u64;
    while pos < total {
        let len = (total - pos).min(SCAN_WINDOW);
        let slice = scanner.read_window(pos, len, 0).expect("read window");
        // memchr (SIMD), exactly what the index build uses.
        count += memchr::memchr_iter(b'\n', slice).count() as u64;
        pos += len;
    }
    let _ = count;
    (total, start.elapsed())
}

/// ScanReader's read loop with a cheap per-page `touch` consumer instead of
/// memchr — isolates the windowed-read overhead from the counting CPU cost.
fn scan_read(path: &Path) -> (u64, Duration) {
    use qview_core::file::{ScanReader, SCAN_WINDOW};
    let mut scanner = ScanReader::open(path).expect("ScanReader");
    let total = scanner.size();
    let start = Instant::now();
    let mut sum = 0u64;
    let mut pos = 0u64;
    while pos < total {
        let len = (total - pos).min(SCAN_WINDOW);
        let slice = scanner.read_window(pos, len, 0).expect("read window");
        sum = sum.wrapping_add(touch(slice));
        pos += len;
    }
    let _ = sum;
    (total, start.elapsed())
}

/// Parallel File reads in 32 MiB chunks, one handle per chunk (the pattern a
/// buffered-streaming scan would use).
fn read_parallel(path: &Path) -> (u64, Duration) {
    use rayon::prelude::*;
    let size = std::fs::metadata(path).unwrap().len();
    let start = Instant::now();
    const CHUNK: usize = 32 * 1024 * 1024;
    let chunks = ((size as usize) + CHUNK - 1) / CHUNK;
    let total: usize = (0..chunks)
        .into_par_iter()
        .map(|i| {
            let start_off = (i as u64) * CHUNK as u64;
            let len = ((size - start_off) as usize).min(CHUNK);
            let mut f = std::fs::File::open(path).unwrap();
            use std::io::Seek;
            f.seek(std::io::SeekFrom::Start(start_off)).unwrap();
            let mut buf = vec![0u8; len];
            f.read_exact(&mut buf).unwrap();
            memchr::memchr_iter(b'\n', &buf).count()
        })
        .sum();
    let _ = total;
    (size, start.elapsed())
}

/// Parallel NO_BUFFERING reads in 4 MiB aligned chunks via positional
/// `seek_read` on one shared handle. Answers: does deep queue depth beat the
/// single-threaded raw ceiling (2.77 GB/s)?
#[cfg(windows)]
fn read_raw_parallel(path: &Path) -> (u64, Duration) {
    use std::os::windows::fs::{FileExt, OpenOptionsExt};
    use rayon::prelude::*;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x2000_0000) // FILE_FLAG_NO_BUFFERING
        .open(path)
        .expect("open no-buffering");
    let size = file.metadata().unwrap().len();
    const CHUNK: usize = 4 * 1024 * 1024;
    let aligned_end = (size - size % 4096) as usize;
    let chunks = aligned_end.div_ceil(CHUNK);
    let start = Instant::now();
    let total: (u64, u64) = (0..chunks)
        .into_par_iter()
        .map(|i| {
            let off = i * CHUNK;
            let len = (aligned_end - off).min(CHUNK);
            // Per-chunk 4096-aligned buffer.
            let mut storage = vec![0u8; len + 4096];
            let base = storage.as_mut_ptr() as usize;
            let aoff = (4096 - (base % 4096)) % 4096;
            let buf = &mut storage[aoff..aoff + len];
            let mut pos = off as u64;
            let mut done = 0;
            while done < len {
                let n = file.seek_read(&mut buf[done..], pos).unwrap();
                if n == 0 {
                    break;
                }
                done += n;
                pos += n as u64;
            }
            (done as u64, touch(&storage[aoff..aoff + done]))
        })
        .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
    (total.0, start.elapsed())
}

#[cfg(not(windows))]
fn read_raw_parallel(path: &Path) -> (u64, Duration) {
    read_parallel(path)
}
