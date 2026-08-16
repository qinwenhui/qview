//! Build script for the installer crate.
//!
//! Two jobs:
//! 1. Embed the user-provided app icon (`gui/egui/assets/icon.ico`) into every
//!    exe of this crate — only if the file exists.  No icon in the repo, no
//!    icon embedded (default icon).
//! 2. Compress the install payload into `$OUT_DIR/payload.qpak` (zstd), which
//!    the setup binary embeds via `include_bytes!`.  The payload is staged by
//!    the `qview-bundle` tool into `target/install/qview-payload/` and passed
//!    to this script through the `QVIEW_PAYLOAD_DIR` env var — it never lives
//!    in the source tree.
//!
//! qpak format:
//! ```text
//! [u32 LE file_count]
//!   × file_count:
//!     [u32 LE name_len][name bytes, UTF-8, '/' separators]
//!     [u64 LE data_len][u64 LE data_offset]     // in the UNCOMPRESSED stream
//! [zstd stream: concatenation of all file data]
//! ```

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    embed_icon_if_present();
    pack_payload();
}

fn embed_icon_if_present() {
    #[cfg(windows)]
    {
        let icon = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../gui/egui/assets/icon.ico");
        // Re-run this build script whenever the icon appears / changes — cargo
        // otherwise won't know and would skip embedding on the next build.
        println!("cargo:rerun-if-changed={}", icon.display());
        if icon.is_file() {
            // windres needs an .rc file (it can't take a bare .ico), so
            // generate one with an absolute path into OUT_DIR.
            let rc = Path::new(&env::var("OUT_DIR").unwrap()).join("icon.rc");
            let rc_content = format!(
                "IDI_APP ICON \"{}\"\n",
                icon.display().to_string().replace('\\', "/")
            );
            fs::write(&rc, rc_content).expect("write icon.rc");
            embed_resource::compile(&rc, embed_resource::NONE);
        }
    }
}

fn pack_payload() {
    // The `qview-bundle` tool touches this stamp after reassembling the
    // payload.  Emitting it UNCONDITIONALLY (even for an empty payload) keeps
    // cargo in explicit rerun mode, so the inner setup build always reruns
    // this script and repacks — without it, a payload-less outer `cargo run`
    // would leave cargo in package-file mode and the setup build would reuse a
    // stale/empty qpak.
    let stamp = Path::new(env!("CARGO_MANIFEST_DIR")).join(".payload_stamp");
    println!("cargo:rerun-if-changed={}", stamp.display());

    // Payload dir: set by `qview-bundle` (target/install/qview-payload);
    // falls back to a crate-local `payload/` dir if you stage one by hand.
    let payload_dir: PathBuf = env::var("QVIEW_PAYLOAD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("payload"));

    // Collect all files recursively, sorted by name for deterministic output.
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    if payload_dir.is_dir() {
        let mut stack = vec![payload_dir.clone()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let entry = entry.unwrap();
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let rel = p.strip_prefix(&payload_dir).unwrap();
                    let name = rel.to_string_lossy().replace('\\', "/");
                    println!("cargo:rerun-if-changed={}", p.display());
                    let bytes = fs::read(&p).unwrap();
                    files.push((name, bytes));
                }
            }
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));

    // Header (manifest) + concatenated data.
    let mut header = Vec::new();
    header.extend_from_slice(&(files.len() as u32).to_le_bytes());
    let mut data = Vec::new();
    let mut offset: u64 = 0;
    for (name, bytes) in &files {
        header.extend_from_slice(&(name.len() as u32).to_le_bytes());
        header.extend_from_slice(name.as_bytes());
        header.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        header.extend_from_slice(&offset.to_le_bytes());
        data.extend_from_slice(bytes);
        offset += bytes.len() as u64;
    }

    let compressed = zstd::stream::encode_all(&data[..], 19).expect("zstd encode payload");

    let mut out = header;
    out.extend_from_slice(&compressed);

    let out_dir = env::var("OUT_DIR").unwrap();
    fs::write(Path::new(&out_dir).join("payload.qpak"), &out).expect("write payload.qpak");

    // Only warn when the setup binary is the one being built (other bins
    // don't embed the payload and would otherwise spam a pointless warning).
    if files.is_empty() && env::var("CARGO_BIN_NAME").as_deref() == Ok("qview-setup") {
        println!(
            "cargo:warning=qview-installer: 载荷为空 — 请用 \
             `cargo run --release -p qview-installer --bin qview-bundle` 打包（会生成 setup exe）"
        );
    }
}
