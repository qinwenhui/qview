//! Replicate the full main() open flow with detailed timing.
//! Usage: bench-open-real <path>

use std::env;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;

use qview_core::file::persist::{file_meta, peek_header, write_index, IndexFile};
use qview_core::file::watch::derive_index_path;
use qview_core::file::SPARSE_FACTOR;
use qview::app::App;

fn main() -> Result<()> {
    let path = PathBuf::from(env::args().nth(1).expect("usage: bench-open-real <path>"));
    println!("=== Bench: {}", path.display());

    let index_path = derive_index_path(&path);
    let t_total = Instant::now();

    let t0 = Instant::now();
    let meta = file_meta(&path)?;
    let t_meta = t0.elapsed();
    println!("[1] file_meta                                     : {:?}", t_meta);

    let t0 = Instant::now();
    let mut app = App::new(path.clone())?;
    let t_app_new = t0.elapsed();
    println!("[2] App::new (mmap open)                          : {:?}", t_app_new);

    let t0 = Instant::now();
    let qli_exists = index_path.exists();
    let t_qli_check = t0.elapsed();
    println!("[3] index_path.exists()                           : {:?}", t_qli_check);

    if qli_exists {
        let t0 = Instant::now();
        let h = peek_header(&index_path)?;
        let t_peek = t0.elapsed();
        println!("[4] peek_header                                   : {:?}", t_peek);

        let fresh = h.file_size == meta.size && h.file_mtime == meta.mtime && h.file_inode == meta.inode;
        println!("    qli fresh: {}", fresh);

        if fresh {
            let t0 = Instant::now();
            let idx = IndexFile::open(&index_path)?;
            let t_qli_open = t0.elapsed();
            println!("[5] IndexFile::open (mmap .qli)                  : {:?}", t_qli_open);

            let t0 = Instant::now();
            let _offsets = if idx.header.offset_size == 4 {
                idx.offsets_u32().iter().map(|&o| o as u64).collect()
            } else {
                idx.offsets_u64().to_vec()
            };
            let t_qli_collect = t0.elapsed();
            println!("[6] collect offsets into Vec<u64>                : {:?}", t_qli_collect);
            println!("    line_count: {}", idx.header.line_count);
        } else {
            println!("[5-6] qli stale, will rebuild");
        }
    } else {
        println!("[4-6] no qli, will build fresh");
    }

    let t0 = Instant::now();
    app.build_index_blocking()?;
    let t_build = t0.elapsed();
    println!("[7] build_index_blocking                          : {:?}", t_build);

    let t0 = Instant::now();
    let offsets = app.engine.index.snapshot_offsets();
    write_index(&index_path, meta.size, meta.mtime, meta.inode, app.engine.total_lines, &offsets, SPARSE_FACTOR,
        app.engine.index.max_line_bytes(), app.engine.index.max_line_index())?;
    let t_persist = t0.elapsed();
    println!("[8] write_index (.qli persist)                    : {:?}", t_persist);
    println!("    .qli size: {} bytes ({:.2} MiB)",
        std::fs::metadata(&index_path)?.len(),
        std::fs::metadata(&index_path)?.len() as f64 / 1024.0 / 1024.0);

    println!("\n=== TOTAL: {:?}", t_total.elapsed());
    println!("(excluding TUI setup)");
    Ok(())
}