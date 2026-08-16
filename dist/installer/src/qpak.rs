//! qpak payload reader — mirrors the layout written by `build.rs`.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

/// One archived file.
pub struct Entry {
    /// Relative path with `/` separators (as stored).
    pub name: String,
    /// Byte range in the decompressed stream.
    pub offset: usize,
    pub len: usize,
}

/// Parse the qpak header (without decompressing the data stream yet).
pub fn read_manifest(payload: &[u8]) -> Vec<Entry> {
    let mut cur = Cursor::new(payload);
    let count = read_u32(&mut cur) as usize;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let name_len = read_u32(&mut cur) as usize;
        let name_bytes = payload[cur.position() as usize..cur.position() as usize + name_len].to_vec();
        cur.set_position(cur.position() + name_len as u64);
        let len = read_u64(&mut cur) as usize;
        let offset = read_u64(&mut cur) as usize;
        entries.push(Entry {
            name: String::from_utf8_lossy(&name_bytes).into_owned(),
            offset,
            len,
        });
    }
    entries
}

/// Decompress the data stream that follows the header.
pub fn decompress(payload: &[u8], entries: &[Entry]) -> std::io::Result<Vec<u8>> {
    let mut cur = Cursor::new(payload);
    read_u32(&mut cur); // file_count
    for _ in entries {
        let name_len = read_u32(&mut cur) as usize;
        cur.set_position(cur.position() + name_len as u64);
        read_u64(&mut cur); // len
        read_u64(&mut cur); // offset
    }
    let data_start = cur.position() as usize;
    let mut out = Vec::new();
    zstd::stream::copy_decode(&payload[data_start..], &mut out)?;
    Ok(out)
}

/// Extract every archived file into `dest` (creating parent dirs).
pub fn extract(payload: &[u8], dest: &Path) -> std::io::Result<()> {
    let entries = read_manifest(payload);
    if entries.is_empty() {
        return Ok(());
    }
    let data = decompress(payload, &entries)?;
    for e in &entries {
        let rel = e.name.replace('/', std::path::MAIN_SEPARATOR_STR);
        let out = dest.join(PathBuf::from(&rel));
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&out, &data[e.offset..e.offset + e.len])?;
    }
    Ok(())
}

fn read_u32(cur: &mut Cursor<&[u8]>) -> u32 {
    let mut b = [0u8; 4];
    cur.read_exact(&mut b).unwrap();
    u32::from_le_bytes(b)
}

fn read_u64(cur: &mut Cursor<&[u8]>) -> u64 {
    let mut b = [0u8; 8];
    cur.read_exact(&mut b).unwrap();
    u64::from_le_bytes(b)
}

// The `Read` trait is needed for `read_exact` above.
use std::io::Read;

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a qpak blob in the exact format `build.rs` writes, so the
    /// extractor is verified against the real layout.
    fn make_qpak(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut header = Vec::new();
        header.extend_from_slice(&(files.len() as u32).to_le_bytes());
        let mut data = Vec::new();
        let mut off = 0u64;
        for (name, bytes) in files {
            header.extend_from_slice(&(name.len() as u32).to_le_bytes());
            header.extend_from_slice(name.as_bytes());
            header.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            header.extend_from_slice(&off.to_le_bytes());
            data.extend_from_slice(bytes);
            off += bytes.len() as u64;
        }
        let compressed = zstd::stream::encode_all(&data[..], 1).unwrap();
        header.extend_from_slice(&compressed);
        header
    }

    #[test]
    fn qpak_roundtrip_extracts_every_file() {
        let blob = make_qpak(&[
            ("qview.exe", b"FAKE_EXE_BYTES"),
            ("assets/NotoSansSC-VF.ttf", b"\0FONT\0"),
            ("assets/sub/dir/readme.txt", b"hello world"),
        ]);
        let dest = std::env::temp_dir().join(format!("qview_qpak_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);

        extract(&blob, &dest).unwrap();
        assert_eq!(
            std::fs::read(dest.join("qview.exe")).unwrap(),
            b"FAKE_EXE_BYTES"
        );
        assert_eq!(
            std::fs::read(dest.join("assets/NotoSansSC-VF.ttf")).unwrap(),
            b"\0FONT\0"
        );
        assert_eq!(
            std::fs::read(dest.join("assets/sub/dir/readme.txt")).unwrap(),
            b"hello world"
        );
        let _ = std::fs::remove_dir_all(&dest);
    }
}
