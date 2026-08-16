//! Windows 构建期配置：
//!   1. 把 `assets/icon.ico` 嵌入 Win32 资源（窗口/任务栏/文件图标）。
//!   2. 把整个 `frontends/gui-egui/assets/` 复制到 `OUT_DIR/../assets/`，即
//!      `<profile>/assets/`，与最终 exe 同目录。`frontends/gui-egui/src/assets.rs` 在
//!      运行时通过 `current_exe().parent()/assets/<file>` 读取。
//!
//! 这两步让 Windows release exe 不再 `include_bytes!` 整个 17M NotoSansSC-VF
//! ttf，二进制体积从 32M 降到约 9M（参考 qview-gui-native 的 12M）。
//!
//! 非 Windows 构建跳过此文件的所有逻辑。

#[cfg(windows)]
fn main() {
    use std::path::Path;

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let assets_src = manifest_dir.join("assets");
    let icon = assets_src.join("icon.ico");

    // 1. Embed icon into Win32 resources (window/taskbar/file icon).
    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-changed={}", assets_src.display());
    if icon.is_file() {
        let rc = Path::new(&std::env::var("OUT_DIR").unwrap()).join("icon.rc");
        let rc_content = format!(
            "IDI_APP ICON \"{}\"\n",
            icon.display().to_string().replace('\\', "/")
        );
        std::fs::write(&rc, rc_content).expect("write icon.rc");
        embed_resource::compile(&rc, embed_resource::NONE);
    }

    // 2. Copy all of frontends/gui-egui/assets/ → <profile>/assets/, alongside the exe.
    //    OUT_DIR is e.g. target/release/build/qview-gui-egui-XXXX/out;
    //    its grandparent is target/release/. This matches the sidecar lookup
    //    path used at runtime (`current_exe().parent().join("assets")`).
    if assets_src.is_dir() {
        let out_dir_var = std::env::var("OUT_DIR").unwrap();
        let out_dir = Path::new(&out_dir_var);
        let profile_dir = out_dir
            .ancestors()
            .nth(3) // out → build → <profile>
            .expect("compute profile dir from OUT_DIR");
        let dst = profile_dir.join("assets");
        copy_dir(&assets_src, &dst);
    }
}

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
    if dst.exists() {
        // 仅在源比目标新时刷新（避免每次 build 都 IO 几十 MB）。
        let src_meta = match std::fs::metadata(src) {
            Ok(m) => m,
            Err(_) => return,
        };
        if let Ok(dst_meta) = std::fs::metadata(dst) {
            if let (Ok(s), Ok(d)) = (src_meta.modified(), dst_meta.modified()) {
                if s <= d {
                    return;
                }
            }
        }
        let _ = std::fs::remove_dir_all(dst);
    }
    if let Err(e) = copy_dir_impl(src, dst) {
        eprintln!(
            "build.rs warning: failed to copy assets {} → {}: {}",
            src.display(),
            dst.display(),
            e
        );
    }
}

#[cfg(windows)]
fn copy_dir_impl(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_impl(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}
