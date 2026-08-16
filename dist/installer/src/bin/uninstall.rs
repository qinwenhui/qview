//! qview 卸载器 — 读取 `data/uninstall.json` 并撤销安装。
//!
//! 刻意不依赖 egui / zstd：仅 winreg + windows(MessageBox) + std::fs，
//! 二进制 ~1MB。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::exit;

use qview_installer::manifest::UninstallManifest;
use winreg::enums::*;
use winreg::RegKey;

fn main() {
    let exe_dir = current_exe_dir();
    let manifest_path = exe_dir.join("data").join("uninstall.json");

    let manifest: Option<UninstallManifest> = fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());

    // No manifest → we cannot tell which files are ours. REFUSE to delete
    // anything: the uninstaller might live in a directory full of unrelated
    // files (e.g. a build output dir), and deleting them is irreversible.
    let Some(m) = manifest else {
        msg_info(
            "未找到卸载清单（data/uninstall.json）。\n\
             为避免误删文件，卸载程序不会删除任何内容。\n\
             请手动删除安装目录，以及桌面上的 qview 快捷方式。",
            "qview 卸载",
        );
        exit(0);
    };

    let confirmed = ask_yes_no("确定要卸载 qview 文本浏览器吗？", "qview 卸载");
    if !confirmed {
        exit(0);
    }

    let keep_index = ask_yes_no(
        "是否保留索引缓存（data/index）？\n选择「否」将一并删除，下次打开文件需重建索引。",
        "qview 卸载",
    );

    // 1. Registry values (OpenWithProgIds entries — don't nuke the parent key).
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    for v in &m.values_to_delete {
        if let Ok(k) = hkcu.open_subkey(&v.parent) {
            let _ = k.delete_value(&v.name);
        }
    }
    // 2. Registry keys (ProgID + uninstall entry).
    for k in &m.keys_to_delete {
        let _ = hkcu.delete_subkey_all(k);
    }

    // 3. Desktop shortcut.
    if let Some(lnk) = &m.shortcut {
        let _ = fs::remove_file(lnk);
    }

    // 4. Install directory — only when it clearly is one of ours.
    let install = Path::new(&m.install_dir);
    let looks_like_install = install.is_dir()
        && (install.join("qview.exe").exists() || install.join("uninstall.exe").exists());
    if looks_like_install {
        remove_install(install, keep_index);
    } else {
        let _ = msg_info(
            &format!(
                "安装目录不符合预期（未找到 qview.exe / uninstall.exe），已跳过删除：\n{}",
                m.install_dir
            ),
            "qview 卸载",
        );
    }
}

fn current_exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Delete the install dir (excluding the currently-running uninstall.exe),
/// optionally keeping `data/index`. The exe dir itself is scheduled for
/// deletion by a detached `cmd` after this process exits.
fn remove_install(dir: &Path, keep_index: bool) {
    if dir.is_dir() {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if name == "uninstall.exe" {
                    continue; // running now, delete it later
                }
                if keep_index && name == "data" {
                    // Remove everything inside data/ except index/.
                    if let Ok(sub) = fs::read_dir(&p) {
                        for e in sub.flatten() {
                            let c = e.path();
                            if e.file_name() == "index" {
                                continue;
                            }
                            if c.is_dir() {
                                let _ = fs::remove_dir_all(&c);
                            } else {
                                let _ = fs::remove_file(&c);
                            }
                        }
                    }
                    continue;
                }
                if p.is_dir() {
                    let _ = fs::remove_dir_all(&p);
                } else {
                    let _ = fs::remove_file(&p);
                }
            }
        }
    }

    // Schedule self-deletion (the running exe is locked until we exit).
    let dir_str = dir.display().to_string();
    let cmd = if keep_index {
        format!("ping 127.0.0.1 -n 2 >nul & del /Q \"{dir_str}\\uninstall.exe\"")
    } else {
        format!("ping 127.0.0.1 -n 2 >nul & rmdir /S /Q \"{dir_str}\"")
    };
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("cmd")
        .args(["/C", &cmd])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn();
}

/// Plain OK message box.
fn msg_info(text: &str, caption: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
    let t: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let c: Vec<u16> = caption.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        MessageBoxW(HWND::default(), PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), MB_OK | MB_ICONINFORMATION);
    }
}

/// Yes/No message box → true = Yes.
fn ask_yes_no(text: &str, caption: &str) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDYES, MB_ICONQUESTION, MB_YESNO,
    };
    let t: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let c: Vec<u16> = caption.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        MessageBoxW(HWND::default(), PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), MB_YESNO | MB_ICONQUESTION)
            == IDYES
    }
}
