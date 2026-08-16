//! Installation actions — copy files, write initial config, desktop shortcut,
//! HKCU file associations, uninstall registry entry, uninstall manifest.
//!
//! All registry work targets `HKCU\Software\Classes` (per-user, no admin/UAC).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use qview_core::config::IndexBuildMode;
use winreg::enums::*;
use winreg::RegKey;

use crate::manifest::{UninstallManifest, ValueToDelete};

pub const PROG_ID: &str = "qview";
pub const UNINSTALL_KEY: &str =
    "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\qview";

/// The default file-association extensions, in display order.
pub const DEFAULT_EXTS: [&str; 4] = [".log", ".txt", ".out", ".err"];

/// Everything the wizard collects from the user.
pub struct InstallOptions {
    pub target: PathBuf,
    pub theme: String,
    /// `None` = let the app pick its default font.
    pub font: Option<String>,
    pub index_mode: IndexBuildMode,
    /// `0` = auto (leave one core for the UI); `≥ 1` = exact thread count.
    pub scan_threads: u32,
    pub desktop_shortcut: bool,
    pub assoc: Vec<&'static str>,
}

/// One step the installer runs, for the progress log.
pub struct Step {
    pub label: &'static str,
    pub ok: bool,
    pub detail: String,
}

/// Execute all install steps. Each step is recorded in `steps` (in order).
pub fn run(opts: &InstallOptions, staging: &Path, steps: &mut Vec<Step>) {
    step(steps, "复制文件到安装目录", || {
        copy_dir_all(staging, &opts.target)
    });

    let data_dir = opts.target.join("data");
    step(steps, "生成初始配置 config.json", || {
        write_config(
            &data_dir,
            &opts.theme,
            opts.font.as_deref(),
            opts.index_mode,
            opts.scan_threads,
        )
    });

    step(steps, "创建索引缓存目录", || {
        fs::create_dir_all(data_dir.join("index")).with_context(|| "创建 data/index")
    });

    let exe_path = opts
        .target
        .join("qview.exe")
        .to_string_lossy()
        .into_owned();
    let lnk_path = desktop_dir().map(|d| d.join("qview.lnk"));

    if opts.desktop_shortcut {
        let workdir = opts.target.to_string_lossy().into_owned();
        let lnk = lnk_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        step(steps, "创建桌面快捷方式", || {
            create_shortcut(&exe_path, &workdir, &lnk)
                .with_context(|| format!("创建快捷方式 {}", lnk))
        });
    }

    step(steps, "注册文件关联 (.log/.txt/.out/.err)", || {
        register_assoc(&exe_path, &opts.assoc)
    });

    step(steps, "注册卸载信息（控制面板）", || {
        register_uninstall(&exe_path, &opts.target.to_string_lossy())
    });

    step(steps, "写入卸载清单", || {
        write_manifest(&opts.target, lnk_path.as_deref(), &opts.assoc)
    });
}

/// Run a closure, record its label + result into `steps`.
fn step(steps: &mut Vec<Step>, label: &'static str, f: impl FnOnce() -> Result<()>) {
    match f() {
        Ok(()) => steps.push(Step {
            label,
            ok: true,
            detail: String::new(),
        }),
        Err(e) => steps.push(Step {
            label,
            ok: false,
            detail: e.to_string(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/// Recursively copy `src` into `dst` (creating parents, overwriting files).
fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            fs::create_dir_all(to.parent().unwrap())?;
            fs::copy(&from, &to).with_context(|| format!("复制 {}", from.display()))?;
        }
    }
    Ok(())
}

/// Write the initial `data/config.json`. Only the wizard's choices are
/// written; every other field is filled by the app's `#[serde(default)]`.
fn write_config(
    data_dir: &Path,
    theme: &str,
    font: Option<&str>,
    mode: IndexBuildMode,
    scan_threads: u32,
) -> Result<()> {
    fs::create_dir_all(data_dir)?;
    let mut gui = serde_json::Map::new();
    gui.insert("theme".into(), serde_json::json!(theme));
    if let Some(f) = font {
        gui.insert("font_family".into(), serde_json::json!(f));
    }
    let mut engine = serde_json::Map::new();
    engine.insert(
        "index_build_mode".into(),
        serde_json::to_value(mode)?, // serializes as "sparse" / "full"
    );
    engine.insert("scan_threads".into(), serde_json::json!(scan_threads));
    let mut root = serde_json::Map::new();
    root.insert("version".into(), serde_json::json!(env!("CARGO_PKG_VERSION")));
    root.insert("gui".into(), serde_json::Value::Object(gui));
    root.insert("engine".into(), serde_json::Value::Object(engine));

    let json = serde_json::to_string_pretty(&serde_json::Value::Object(root))?;
    fs::write(data_dir.join("config.json"), json).with_context(|| "写入 config.json")
}

/// `{USERPROFILE}\Desktop` (the wizard is per-user by design).
fn desktop_dir() -> Option<PathBuf> {
    std::env::var("USERPROFILE")
        .ok()
        .map(|p| PathBuf::from(p).join("Desktop"))
}

// ---------------------------------------------------------------------------
// Desktop shortcut (.lnk via Shell Link COM)
// ---------------------------------------------------------------------------

fn create_shortcut(target: &str, workdir: &str, lnk_path: &str) -> windows::core::Result<()> {
    use windows::core::{Interface, PCWSTR};
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED, IPersistFile,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

    unsafe {
        // CoInitializeEx returns an HRESULT, not a Result — turn it into one.
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let sl: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let target_wide: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
        let workdir_wide: Vec<u16> = workdir.encode_utf16().chain(std::iter::once(0)).collect();
        sl.SetPath(PCWSTR(target_wide.as_ptr()))?;
        sl.SetWorkingDirectory(PCWSTR(workdir_wide.as_ptr()))?;
        // Icon from the exe itself.
        sl.SetIconLocation(PCWSTR(target_wide.as_ptr()), 0)?;

        let pf: IPersistFile = sl.cast()?;
        let lnk_wide: Vec<u16> = lnk_path.encode_utf16().chain(std::iter::once(0)).collect();
        pf.Save(PCWSTR(lnk_wide.as_ptr()), BOOL(1))?;
        CoUninitialize();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Registry (HKCU — no admin needed)
// ---------------------------------------------------------------------------

/// Register the `qview` ProgID + "Open with" entries for each extension.
///
/// Two complementary mechanisms so the app reliably shows up in 右键 → 打开方式:
/// 1. `qview` ProgID + `<ext>\OpenWithProgIds` — puts the ProgID in the
///    context-menu "Open with" submenu for those file types.
/// 2. `Applications\qview.exe` — registers the app by executable name, which
///    populates the "选择其他应用" list even for types never opened before.
fn register_assoc(exe: &str, exts: &[&str]) -> Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (classes, _) = hkcu
        .create_subkey("Software\\Classes")
        .context("打开 HKCU\\Software\\Classes")?;

    // 1. ProgID for the file types.
    let (prog, _) = classes.create_subkey(format!("{}\\shell\\open\\command", PROG_ID))?;
    prog.set_value("", &format!("\"{}\" \"%1\"", exe))?;

    let (icon, _) = classes.create_subkey(format!("{}\\DefaultIcon", PROG_ID))?;
    icon.set_value("", &format!("\"{}\",0", exe))?;

    for ext in exts {
        let (owp, _) = classes.create_subkey(format!("{}\\OpenWithProgIds", ext))?;
        owp.set_value(PROG_ID, &"")?;
    }

    // 2. App-by-executable-name registration.
    let (app, _) = classes.create_subkey("Applications\\qview.exe\\shell\\open\\command")?;
    app.set_value("", &format!("\"{}\" \"%1\"", exe))?;
    let (app_icon, _) = classes.create_subkey("Applications\\qview.exe\\DefaultIcon")?;
    app_icon.set_value("", &format!("\"{}\",0", exe))?;

    Ok(())
}

/// Register the entry shown in 控制面板 → 程序和功能.
fn register_uninstall(exe: &str, dir: &str) -> Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(UNINSTALL_KEY)
        .context("创建卸载注册表项")?;
    key.set_value("DisplayName", &"qview 文本浏览器")?;
    key.set_value("DisplayVersion", &env!("CARGO_PKG_VERSION"))?;
    key.set_value("Publisher", &"qinwh")?;
    key.set_value("InstallLocation", &dir)?;
    key.set_value("DisplayIcon", &format!("\"{}\",0", exe))?;
    key.set_value("UninstallString", &format!("\"{}\\uninstall.exe\"", dir))?;
    key.set_value("NoModify", &1u32)?;
    key.set_value("NoRepair", &1u32)?;
    Ok(())
}

/// Write the manifest the uninstaller reads.
fn write_manifest(
    target: &Path,
    shortcut: Option<&Path>,
    exts: &[&str],
) -> Result<()> {
    let manifest = UninstallManifest {
        install_dir: target.to_string_lossy().into_owned(),
        shortcut: shortcut.map(|p| p.to_string_lossy().into_owned()),
        keys_to_delete: vec![
            format!("Software\\Classes\\{}", PROG_ID),
            "Software\\Classes\\Applications\\qview.exe".to_string(),
            UNINSTALL_KEY.to_string(),
        ],
        values_to_delete: exts
            .iter()
            .map(|ext| ValueToDelete {
                parent: format!("Software\\Classes\\{}\\OpenWithProgIds", ext),
                name: PROG_ID.to_string(),
            })
            .collect(),
        uninstall_key: UNINSTALL_KEY.to_string(),
    };
    fs::create_dir_all(target.join("data"))?;
    fs::write(
        target.join("data").join("uninstall.json"),
        serde_json::to_string_pretty(&manifest)?,
    )
    .context("写入卸载清单")
}

#[cfg(test)]
mod tests {
    use super::*;
    use qview_core::config::IndexBuildMode;

    #[test]
    fn write_config_produces_minimal_json_the_app_can_parse() {
        let dir =
            std::env::temp_dir().join(format!("qview_cfg_test_{}", std::process::id()));
        write_config(&dir, "Light", Some("NotoSansSC-VF"), IndexBuildMode::Sparse, 4).unwrap();

        let raw = std::fs::read_to_string(dir.join("config.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(v["gui"]["theme"], "Light");
        assert_eq!(v["gui"]["font_family"], "NotoSansSC-VF");
        assert_eq!(v["engine"]["index_build_mode"], "sparse");
        assert_eq!(v["engine"]["scan_threads"], 4);

        // Full parseability is covered by the app-side test in
        // gui/egui/src/config.rs (minimal_config_from_installer_parses_with_defaults).
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn register_assoc_writes_all_keys_and_uninstall_removes_them() {
        // Use a throwaway extension so we never touch real .log associations.
        let test_ext = ".qviewtest";
        let exe = "C:\\Program Files\\qview\\qview.exe";
        register_assoc(exe, &[test_ext]).expect("register_assoc");

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let ok = |sub: &str, has: bool| {
            let exists = hkcu.open_subkey(sub).is_ok();
            assert_eq!(exists, has, "registry key {sub} present={exists} expected={has}");
        };
        // ProgID + command + icon.
        ok(r"Software\Classes\qview\shell\open\command", true);
        ok(r"Software\Classes\qview\DefaultIcon", true);
        // OpenWithProgIds entry.
        let owp_path = format!(r"Software\Classes\{}\OpenWithProgIds", test_ext);
        let owp = hkcu
            .open_subkey(&owp_path)
            .unwrap_or_else(|e| panic!("open_subkey({owp_path}) 失败: {e:?}"));
        assert_eq!(
            owp.get_value::<String, _>("qview").unwrap_or_default(),
            "",
            "qview OpenWithProgIds value"
        );
        // App-by-exe registration.
        ok(r"Software\Classes\Applications\qview.exe\shell\open\command", true);

        // The command must invoke the installed exe with the file as %1.
        let cmd = hkcu
            .open_subkey(r"Software\Classes\qview\shell\open\command")
            .unwrap()
            .get_value::<String, _>("")
            .unwrap();
        assert_eq!(cmd, "\"C:\\Program Files\\qview\\qview.exe\" \"%1\"");

        // Cleanup — exactly what the uninstaller does.
        for k in [
            r"Software\Classes\qview",
            r"Software\Classes\Applications\qview.exe",
            &format!("Software\\Classes\\{}", test_ext),
        ] {
            hkcu.delete_subkey_all(k).unwrap();
        }
        ok(r"Software\Classes\qview", false);
        ok(r"Software\Classes\Applications\qview.exe", false);
        ok(r"Software\Classes\.qviewtest", false);
    }

    #[test]
    fn copy_dir_all_mirrors_tree() {
        let src =
            std::env::temp_dir().join(format!("qview_copy_src_{}", std::process::id()));
        let dst =
            std::env::temp_dir().join(format!("qview_copy_dst_{}", std::process::id()));
        fs::create_dir_all(src.join("assets")).unwrap();
        fs::write(src.join("qview.exe"), b"EXE").unwrap();
        fs::write(src.join("assets/x.ttf"), b"FONT").unwrap();

        copy_dir_all(&src, &dst).unwrap();
        assert_eq!(fs::read(dst.join("qview.exe")).unwrap(), b"EXE");
        assert_eq!(fs::read(dst.join("assets/x.ttf")).unwrap(), b"FONT");
        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dst);
    }
}
