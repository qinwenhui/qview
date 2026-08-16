//! 对话框：NSOpenPanel 选文件 + NSAlert 系列（错误 / 属性 / 设置 / 帮助 / 快捷键 / 关于）。
//!
//! ## 重入不变量
//!
//! `runModal` 会跑嵌套 runloop 并再次触发 timer → `with_app` 会二次拿到 `&mut App`
//! （UB）。因此本模块所有函数都遵守：**先在 `with_app` 短闭包内取出数据，再开
//! modal，modal 返回后再进 `with_app` 应用结果**。

use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertStyle, NSModalResponseOK, NSOpenPanel, NSWindow, NSWorkspace};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::app::with_app;
use crate::util::ns_string;

/// 打开文件对话框。返回选中路径，取消则 None。
pub fn pick_file(_window: &Option<Retained<NSWindow>>) -> Option<PathBuf> {
    let mtm = MainThreadMarker::new().unwrap();
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(false);
    let resp = panel.runModal();
    if resp == NSModalResponseOK {
        if let Some(url) = panel.URL() {
            if let Some(p) = url.path() {
                return Some(PathBuf::from(p.to_string()));
            }
        }
    }
    None
}

/// 顶部错误提示（调用方必须在 `with_app` 闭包之外调用）。
pub fn show_error(msg: &str) {
    let mtm = MainThreadMarker::new().unwrap();
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&ns_string("错误"));
    alert.setInformativeText(&ns_string(msg));
    alert.setAlertStyle(NSAlertStyle::Critical);
    alert.addButtonWithTitle(&ns_string("确定"));
    alert.runModal();
}

/// 文件属性。
pub fn show_properties() {
    let info = with_app(|app| {
        if let Some(b) = &app.bridge {
            format!(
                "文件: {}\n大小: {}\n行数: {}\n索引: {}",
                b.path.display(),
                crate::util::human_bytes(b.size),
                b.total_lines(),
                if app.indexing_active {
                    "正在建立…"
                } else {
                    "就绪"
                },
            )
        } else {
            "未打开文件".to_string()
        }
    });
    show_info("文件属性", &info);
}

pub fn show_about() {
    show_info(
        "关于 qview",
        "qview\n原生 AppKit / CoreText 文本浏览器\n基于 qview-core 引擎（mmap + 后台索引 + 快速搜索）",
    );
}

pub fn show_help() {
    show_info(
        "帮助",
        "qview 是一个本地日志文件查看器。\n\n\
         · Cmd+O 打开文件\n\
         · 支持超大文件（内存映射，不整读）\n\
         · 后台建立行索引，可即时搜索\n\
         · 菜单/快捷键见“快捷键”",
    );
}

pub fn show_shortcuts() {
    show_info(
        "快捷键",
        "Cmd+O        打开\nCmd+R        重新加载\nCmd+W        关闭\n\n\
         Cmd+F        查找\nF3 / Cmd+G      下一个\nShift+F3 / Cmd+Shift+G  上一个\n\n\
         Cmd+L        跳到行\nHome/End     顶部 / 底部\nPageUp/PageDown  上一页 / 下一页\n\n\
         Cmd+= / Cmd+-  字体加大 / 减小\nCmd+0          字体重置\n\n\
         Cmd+Shift+T  切换主题\nEsc          取消搜索\n\n\
         工具 → 缓存管理   查看/清空 .qli 索引缓存",
    );
}

pub fn open_config_dir() {
    if let Some(dir) = crate::config::AppConfig::config_dir() {
        let url = objc2_foundation::NSURL::fileURLWithPath(&ns_string(&dir.display().to_string()));
        let ws = NSWorkspace::sharedWorkspace();
        let _ = ws.openURL(&url);
    }
}

/// 跳到行对话框。
pub fn prompt_goto() {
    let total = with_app(|app| app.total_lines());
    if total == 0 {
        return;
    }
    let mtm = MainThreadMarker::new().unwrap();
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&ns_string("跳到行"));
    alert.setInformativeText(&ns_string(&format!("行号范围 1 - {}", total)));
    alert.addButtonWithTitle(&ns_string("确定"));
    alert.addButtonWithTitle(&ns_string("取消"));

    let input = objc2_app_kit::NSTextField::new(mtm);
    input.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(160.0, 24.0)));
    input.setPlaceholderString(Some(&ns_string("行号")));
    alert.setAccessoryView(Some(&input));

    let resp = alert.runModal();
    if resp == NSModalResponseOK {
        let s = input.stringValue().to_string();
        if let Ok(n) = s.trim().parse::<u64>() {
            if n > 0 {
                with_app(|app| app.goto_line(n - 1));
            }
        }
    }
}

/// 索引管理：列出缓存目录里的 .qli 文件，并可一键清空（下次打开自动重建）。
pub fn manage_indexes() {
    let (dir, indexing) = with_app(|app| {
        let d = app
            .config
            .engine
            .index_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("(未设置)"));
        (d, app.indexing_active)
    });

    // 收集 .qli 文件（文件名 + 大小）
    let mut files: Vec<(String, u64)> = Vec::new();
    let mut total_bytes = 0u64;
    if dir.is_dir() {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.ends_with(".qli") {
                    if let Ok(md) = e.metadata() {
                        total_bytes += md.len();
                        files.push((name, md.len()));
                    }
                }
            }
        }
    }
    files.sort();

    let mut body = format!("索引缓存目录:\n{}\n", dir.display());
    if indexing {
        body.push_str("\n⚠ 正在建立索引，请完成后清理\n");
    }
    if files.is_empty() {
        body.push_str("\n没有索引缓存文件");
    } else {
        body.push_str(&format!(
            "\n{} 个缓存文件，共 {}\n",
            files.len(),
            crate::util::human_bytes(total_bytes)
        ));
        for (name, sz) in files.iter().take(15) {
            let short = if name.chars().count() > 42 {
                format!("{}…", name.chars().take(41).collect::<String>())
            } else {
                name.clone()
            };
            body.push_str(&format!(" · {}  ({})\n", short, crate::util::human_bytes(*sz)));
        }
        if files.len() > 15 {
            body.push_str(&format!(" · …其余 {} 个\n", files.len() - 15));
        }
    }

    // 有缓存且不在索引中才给删除按钮
    let can_delete = !files.is_empty() && !indexing;
    let mtm = MainThreadMarker::new().unwrap();
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&ns_string("索引管理"));
    alert.setInformativeText(&ns_string(&body));
    alert.addButtonWithTitle(&ns_string(if can_delete { "清空缓存" } else { "关闭" }));
    if can_delete {
        alert.addButtonWithTitle(&ns_string("取消"));
    }
    let resp = alert.runModal();
    if can_delete && resp == NSModalResponseOK {
        // 清空 .qli（下次打开对应文件会强制重建）
        for (name, _) in &files {
            let _ = std::fs::remove_file(dir.join(name));
        }
        show_info("索引管理", &format!("已删除 {} 个缓存文件。\n重新打开文件时索引会自动重建。", files.len()));
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn show_info(title: &str, text: &str) {
    let mtm = MainThreadMarker::new().unwrap();
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&ns_string(title));
    alert.setInformativeText(&ns_string(text));
    alert.addButtonWithTitle(&ns_string("确定"));
    alert.runModal();
}

