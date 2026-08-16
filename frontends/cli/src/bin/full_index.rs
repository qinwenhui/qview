//! Debug binary: full app init path (like main but no TUI).
//! Run with: cargo run --release --bin full_index -- <path>

use std::env;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;

use qview::app::App;
use qview_core::file::persist::{file_meta, peek_header, write_index, IndexFile};
use qview_core::file::watch::derive_index_path;
use qview_core::file::{LineIndex, SPARSE_FACTOR};

fn main() -> Result<()> {
    let path = PathBuf::from(env::args().nth(1).expect("usage: full_index <path>"));
    let index_path = derive_index_path(&path);
    let meta = file_meta(&path)?;
    let mut app = App::new(path.clone())?;

    println!("app.total_lines (init): {}", app.engine.total_lines);

    if index_path.exists() {
        match peek_header(&index_path) {
            Ok(h) if h.file_size == meta.size && h.file_mtime == meta.mtime
                  && h.file_inode == meta.inode =>
            {
                match IndexFile::open(&index_path) {
                    Ok(idx) => {
                        let line_count = idx.header.line_count;
                        let offsets = if idx.header.offset_size == 4 {
                            idx.offsets_u32().iter().map(|&o| o as u64).collect()
                        } else {
                            idx.offsets_u64().to_vec()
                        };
                        app.engine.index = LineIndex::from_vec(offsets, meta.size);
                        app.engine.total_lines = line_count;
                        println!("loaded from .qli: total_lines={}", line_count);
                    }
                    Err(e) => println!("qli load failed: {}", e),
                }
            }
            _ => println!("qli stale"),
        }
    }

    if app.engine.total_lines == 0 {
        let t0 = Instant::now();
        app.build_index_blocking()?;
        println!("built: total_lines={} in {:?}", app.engine.total_lines, t0.elapsed());
        // Persist
        let offsets = app.engine.index.snapshot_offsets();
        write_index(&index_path, meta.size, meta.mtime, meta.inode, app.engine.total_lines, &offsets, SPARSE_FACTOR,
            app.engine.index.max_line_bytes(), app.engine.index.max_line_index())?;
        println!("persisted offsets.len()={}, total_lines={}", offsets.len(), app.engine.total_lines);
    }

    println!("FINAL: app.total_lines = {}", app.engine.total_lines);
    Ok(())
}
