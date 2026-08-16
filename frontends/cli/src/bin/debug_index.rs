//! Debug binary: load a file, build index, print stats, exit.
//! Run with: cargo run --release --bin debug-index -- <path>

use std::env;
use std::path::PathBuf;
use std::time::Instant;

use qview_core::file::{IndexBuilder, MmapBackend};

fn main() {
    let path = PathBuf::from(env::args().nth(1).expect("usage: debug-index <path>"));
    let mmap = MmapBackend::open(&path).expect("open");
    let size = mmap.size();
    println!("file: {} size: {}", path.display(), size);

    let builder = IndexBuilder::new(mmap.clone());
    let t0 = Instant::now();
    let offsets = builder.build_with_progress(|_, _| {}).expect("build");
    let elapsed = t0.elapsed();
    println!("offsets.len(): {}", offsets.len());
    println!("build time: {:?}", elapsed);

    // Independent count via memchr on the raw bytes.
    let slice = mmap.as_slice();
    let t0 = Instant::now();
    let direct_newlines = memchr::memchr_iter(b'\n', slice).count();
    let elapsed = t0.elapsed();
    println!("memchr newlines: {} (in {:?})", direct_newlines, elapsed);

    // Compute total_lines the same way the app does.
    let ends_with_newline = !slice.is_empty() && slice[slice.len() - 1] == b'\n';
    let total_lines = if slice.is_empty() {
        0
    } else if ends_with_newline {
        (offsets.len() as u64).saturating_sub(1)
    } else {
        offsets.len() as u64
    };
    println!("computed total_lines: {}", total_lines);

    if (total_lines as i64) != (direct_newlines as i64) {
        println!("MISMATCH! total_lines vs newlines");
        let diff = direct_newlines as i64 - total_lines as i64;
        println!("diff = {}", diff);
        println!("first 10 offsets: {:?}", &offsets[..offsets.len().min(10)]);
    } else {
        println!("OK — total_lines == newline count");
    }
}