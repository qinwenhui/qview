//! File layer: mmap, line index, persistent .qli cache, file watcher.

pub mod background_indexer;
pub mod index;
pub mod mmap_backend;
pub mod persist;
pub mod scan_reader;
pub mod watch;

pub use background_indexer::{BackgroundIndexer, IndexProgress};
pub use index::{IndexBuilder, LineIndex, SPARSE_FACTOR};
pub use mmap_backend::MmapBackend;
pub use persist::{Header, IndexFile, MAGIC, VERSION};
pub use scan_reader::{ScanReader, Window, WindowStream, MAX_LEAD, SCAN_WINDOW};
pub use watch::FileWatcher;