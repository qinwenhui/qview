//! 载荷组装 + 打包工具（`qview-bundle`）。
//!
//! 完整打包流水线（一条命令）：
//! ```text
//! cargo run --release -p qview-installer --bin qview-bundle
//!   ├─ 构建 qview-gui-egui (release)
//!   ├─ 构建 qview-uninstall (release, 无 egui)
//!   ├─ 组装 target/install/qview-payload/（qview.exe + gui/egui/assets/* + LICENSE + uninstall.exe）
//!   └─ 构建 qview-setup：build.rs 读取 QVIEW_PAYLOAD_DIR 压缩嵌入 → setup exe
//! ```
//!
//! 源码资产只放在 `gui/egui/assets/`（字体 / 样式 / 图标 / 收款码），本工具
//! 自动全量收集；所有中间产物都在 `target/` 下，源码树保持干净。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

fn main() {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // dist/installer/ -> workspace root (3 levels up: dist/installer/dist/..)
    let root = crate_dir.join("..").join("..").canonicalize().unwrap();
    let target = root.join("target").join("release");
    let payload = root.join("target").join("install").join("qview-payload");

    // 1. Build the egui GUI.
    println!("[bundle] 构建 qview-gui-egui (release)…");
    run_cargo(&root, &["build", "--release", "-p", "qview-gui-egui"]);

    // 2. Build the tiny uninstaller (no egui).
    println!("[bundle] 构建 qview-uninstall (release)…");
    run_cargo(
        &root,
        &[
            "build",
            "--release",
            "-p",
            "qview-installer",
            "--no-default-features",
            "--features",
            "uninstaller",
        ],
    );

    // 3. Assemble the payload from source + freshly-built binaries.
    if payload.exists() {
        let _ = fs::remove_dir_all(&payload);
    }
    fs::create_dir_all(payload.join("assets")).unwrap();
    fs::copy(target.join("qview-gui-egui.exe"), payload.join("qview.exe"))
        .expect("复制 qview.exe");
    fs::copy(target.join("qview-uninstall.exe"), payload.join("uninstall.exe"))
        .expect("复制 uninstall.exe");

    // All source assets (font / themes / icon / donate QR codes) come from the
    // egui frontend asset dir — the user only ever edits this one place.
    let src_assets = root.join("frontends").join("gui-egui").join("assets");
    if src_assets.is_dir() {
        copy_dir(&src_assets, &payload.join("assets"));
    }

    let license = root.join("LICENSE");
    if license.is_file() {
        fs::copy(&license, payload.join("LICENSE")).unwrap();
    } else {
        eprintln!("[bundle] 警告: 未找到根目录 LICENSE，已跳过");
    }

    // Touch the stamp that build.rs reruns on.  This makes the inner setup
    // build always repack the freshly-assembled payload, even if an earlier
    // `cargo run` for this tool already built the crate without a payload.
    let _ = fs::write(crate_dir.join(".payload_stamp"), format!("{}", std::process::id()));

    // 4. Report.
    let listed = list_files(&payload);
    let mut total = 0u64;
    println!("[bundle] payload/ 内容:");
    for (name, size) in &listed {
        println!("  {:>10}  {}", size, name);
        total += size;
    }
    println!(
        "[bundle] 共 {} 个文件 / {:.1} MiB",
        listed.len(),
        total as f64 / 1048576.0
    );

    // 5. Build the self-contained setup exe.  build.rs packs the payload dir
    //    we just assembled (passed via QVIEW_PAYLOAD_DIR).  `--bin
    //    qview-setup` only — building the whole crate would try to relink the
    //    currently-running qview-bundle.exe and fail (file locked).
    println!("[bundle] 构建 qview-setup (内嵌载荷)…");
    run_cargo_env(
        &root,
        &["build", "--release", "-p", "qview-installer", "--bin", "qview-setup"],
        &[("QVIEW_PAYLOAD_DIR", payload.to_string_lossy().as_ref())],
    );
    // 输出带版本号（与 README / docs/INSTALLER.md 一致）：qview-setup-<version>.exe
    let setup = target.join("qview-setup.exe");
    let setup_ver = target.join(format!("qview-setup-{}.exe", env!("CARGO_PKG_VERSION")));
    if setup_ver.exists() {
        let _ = fs::remove_file(&setup_ver);
    }
    if let Err(e) = fs::rename(&setup, &setup_ver) {
        eprintln!("[bundle] 重命名 {} → {} 失败: {e}", setup.display(), setup_ver.display());
        exit(1);
    }
    println!("[bundle] 完成 → {}", setup_ver.display());
}

fn run_cargo(root: &Path, args: &[&str]) {
    run_cargo_env(root, args, &[]);
}

fn run_cargo_env(root: &Path, args: &[&str], envs: &[(&str, &str)]) {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root).args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let status = cmd.status().expect("无法启动 cargo");
    if !status.success() {
        eprintln!("[bundle] cargo 构建失败: {args:?}");
        exit(1);
    }
}

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap();
        }
    }
}

fn list_files(dir: &Path) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(entries) = fs::read_dir(&d) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(md) = fs::metadata(&p) {
                    let rel = p
                        .strip_prefix(dir)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.push((rel, md.len()));
                }
            }
        }
    }
    out.sort();
    out
}
