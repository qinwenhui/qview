//! Persistent line-offset index (`.qli` file).
//!
//! Format: 64-byte header + u32/u64 offset array.
//!
//! ```text
//! Header (64 bytes):
//!   [0..4]   magic      "QLI\0"
//!   [4..8]   version    u32 LE
//!   [8..16]  file_size  u64
//!   [16..24] file_mtime u64 (unix seconds)
//!   [24..32] file_inode u64 (0 on Windows)
//!   [32..40] line_count u64
//!   [40]     offset_size u8 (4 or 8)
//!   [41]     flags      u8  (bit 0: sparse)
//!   [42..46] sparse_factor u32 (only if sparse)
//!   [46..64] reserved
//!
//! Body: offsets[sparse_count] as u32 or u64 (sparse format)
//! ```

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use memmap2::Mmap;
use xxhash_rust::xxh3::Xxh3;

pub const MAGIC: [u8; 4] = *b"QLI\0";
pub const VERSION: u32 = 2;
pub const HEADER_LEN: usize = 64;

/// 写 `.qli` 时临时文件名的单调序号（防并发 writer 撞名）。
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Header {
    pub magic: [u8; 4],
    pub version: u32,
    pub file_size: u64,
    pub file_mtime: u64,
    pub file_inode: u64,
    pub line_count: u64,
    pub offset_size: u8,
    pub flags: u8,
    pub sparse_factor: u32,
    /// Byte length of the longest line (exact). 0 in older cache files.
    pub max_line_bytes: u64,
    /// 0-based index of the longest line. 0 in older cache files.
    pub max_line_index: u64,
    pub reserved: [u8; 2],
}

impl Header {
    pub fn write_into(&self, buf: &mut [u8; HEADER_LEN]) {
        buf[0..4].copy_from_slice(&self.magic);
        buf[4..8].copy_from_slice(&self.version.to_le_bytes());
        buf[8..16].copy_from_slice(&self.file_size.to_le_bytes());
        buf[16..24].copy_from_slice(&self.file_mtime.to_le_bytes());
        buf[24..32].copy_from_slice(&self.file_inode.to_le_bytes());
        buf[32..40].copy_from_slice(&self.line_count.to_le_bytes());
        buf[40] = self.offset_size;
        buf[41] = self.flags;
        buf[42..46].copy_from_slice(&self.sparse_factor.to_le_bytes());
        buf[46..54].copy_from_slice(&self.max_line_bytes.to_le_bytes());
        buf[54..62].copy_from_slice(&self.max_line_index.to_le_bytes());
        buf[62..64].fill(0);
    }

    pub fn parse(buf: &[u8; HEADER_LEN]) -> Result<Self> {
        if &buf[0..4] != MAGIC {
            anyhow::bail!("not a QLI file (magic mismatch)");
        }
        let version = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        if version != VERSION {
            anyhow::bail!("unsupported QLI version: {}", version);
        }
        Ok(Self {
            magic: MAGIC,
            version,
            file_size: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            file_mtime: u64::from_le_bytes(buf[16..24].try_into().unwrap()),
            file_inode: u64::from_le_bytes(buf[24..32].try_into().unwrap()),
            line_count: u64::from_le_bytes(buf[32..40].try_into().unwrap()),
            offset_size: buf[40],
            flags: buf[41],
            sparse_factor: u32::from_le_bytes(buf[42..46].try_into().unwrap()),
            max_line_bytes: u64::from_le_bytes(buf[46..54].try_into().unwrap()),
            max_line_index: u64::from_le_bytes(buf[54..62].try_into().unwrap()),
            reserved: [0u8; 2],
        })
    }
}

/// A loaded, mmap'd `.qli` file. The body offsets are exposed as a slice
/// (still inside the mmap, no copy).
pub struct IndexFile {
    pub header: Header,
    /// Body bytes. Each entry is either `u32` or `u64` depending on `header.offset_size`.
    pub body: Mmap,
}

impl IndexFile {
    /// Mmap a `.qli` from disk.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        #[cfg(unix)]
        {
            use memmap2::Advice;
            // Random-access pattern for the body.
            let _ = mmap.advise(Advice::Random);
        }

        let mut hdr_bytes = [0u8; HEADER_LEN];
        hdr_bytes.copy_from_slice(&mmap[..HEADER_LEN]);
        let header = Header::parse(&hdr_bytes)?;

        Ok(Self { header, body: mmap })
    }

    /// Borrow the offset slice as a typed view.
    #[inline]
    pub fn offsets_u32(&self) -> &[u32] {
        let start = HEADER_LEN;
        // For sparse format, body contains sparse_count entries, not line_count.
        let len = if self.header.flags & 1 != 0 {
            // body includes the 64-byte header — subtract it before dividing.
            (self.body.len().saturating_sub(HEADER_LEN)) / 4
        } else {
            self.header.line_count as usize
        };
        // SAFETY: we trust the persisted format; offset_size=4 means body is u32.
        unsafe {
            std::slice::from_raw_parts(
                self.body.as_ptr().add(start) as *const u32,
                len,
            )
        }
    }

    #[inline]
    pub fn offsets_u64(&self) -> &[u64] {
        let start = HEADER_LEN;
        let len = if self.header.flags & 1 != 0 {
            // body includes the 64-byte header — subtract it before dividing.
            (self.body.len().saturating_sub(HEADER_LEN)) / 8
        } else {
            self.header.line_count as usize
        };
        unsafe {
            std::slice::from_raw_parts(
                self.body.as_ptr().add(start) as *const u64,
                len,
            )
        }
    }

    /// Compute a quick fingerprint for hot-path checks (no I/O needed).
    pub fn fingerprint(file_size: u64, file_mtime: u64, file_inode: u64) -> u64 {
        let mut h = Xxh3::new();
        h.update(&file_size.to_le_bytes());
        h.update(&file_mtime.to_le_bytes());
        h.update(&file_inode.to_le_bytes());
        h.digest()
    }

    pub fn matches(&self, file_size: u64, file_mtime: u64, file_inode: u64) -> bool {
        self.header.file_size == file_size
            && self.header.file_mtime == file_mtime
            && self.header.file_inode == file_inode
    }
}

/// Write a fresh `.qli` file from sparse line offsets.
///
/// `line_count` is the user-visible line count (matches `wc -l` for files
/// terminated by `\n`, plus 1 for files that aren't).
/// `sparse_offsets` is the sparse offset array (every SPARSE_FACTOR lines).
/// `sparse_factor` is the sampling factor used.
/// `max_line_bytes` / `max_line_index` describe the longest line (0/0 if unknown).
pub fn write_index(
    path: &Path,
    file_size: u64,
    file_mtime: u64,
    file_inode: u64,
    line_count: u64,
    sparse_offsets: &[u64],
    sparse_factor: u32,
    max_line_bytes: u64,
    max_line_index: u64,
) -> Result<()> {
    let use_u32 = file_size <= u32::MAX as u64;
    let offset_size: u8 = if use_u32 { 4 } else { 8 };

    let header = Header {
        magic: MAGIC,
        version: VERSION,
        file_size,
        file_mtime,
        file_inode,
        line_count,
        offset_size,
        flags: 1, // sparse format
        sparse_factor,
        max_line_bytes,
        max_line_index,
        reserved: [0u8; 2],
    };
    let mut hdr_bytes = [0u8; HEADER_LEN];
    header.write_into(&mut hdr_bytes);

    // 唯一临时名：同一文件可能被 GUI 主视图与 Agent 侧引擎并发建索引，
    // 固定 `.tmp` 名会让两个 writer 互相截断 → 损坏缓存。加 pid+序号区分。
    let tmp = {
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        path.with_file_name(format!("{stem}.tmp.{}.{seq}", std::process::id()))
    };
    let f = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
    let mut w = BufWriter::with_capacity(1024 * 1024, f); // 1 MiB write buffer
    w.write_all(&hdr_bytes)?;
    if use_u32 {
        // Bulk conversion: write all u32s as a single buffer.
        let mut bulk = Vec::with_capacity(sparse_offsets.len() * 4);
        for &o in sparse_offsets {
            bulk.extend_from_slice(&(o as u32).to_le_bytes());
        }
        w.write_all(&bulk)?;
    } else {
        let mut bulk = Vec::with_capacity(sparse_offsets.len() * 8);
        for &o in sparse_offsets {
            bulk.extend_from_slice(&o.to_le_bytes());
        }
        w.write_all(&bulk)?;
    }
    w.flush()?;
    // Don't sync_all() here — it forces the disk to flush to physical media,
    // which on HDD takes seconds for tens of MB and stalls the whole open.
    // OS will flush lazily; if the process crashes before flush, we just
    // rebuild the index next time (cheap for small/medium files).
    drop(w);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Get file metadata used to validate an index.
pub struct FileMeta {
    pub size: u64,
    pub mtime: u64,
    pub inode: u64,
}

#[cfg(unix)]
pub fn file_meta(path: &Path) -> Result<FileMeta> {
    use std::os::unix::fs::MetadataExt;
    let md = std::fs::metadata(path)?;
    Ok(FileMeta {
        size: md.len(),
        mtime: md.mtime() as u64,
        inode: md.ino(),
    })
}

#[cfg(not(unix))]
pub fn file_meta(path: &Path) -> Result<FileMeta> {
    let md = std::fs::metadata(path)?;
    let mtime = md
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Ok(FileMeta {
        size: md.len(),
        mtime,
        inode: 0,
    })
}

/// Read the first 64 bytes of an existing `.qli` to peek at the header
/// without mmap'ing the whole file (used for quick validation).
pub fn peek_header(path: &Path) -> Result<Header> {
    let mut f = File::open(path)?;
    let mut buf = [0u8; HEADER_LEN];
    f.read_exact(&mut buf)?;
    Header::parse(&buf)
}