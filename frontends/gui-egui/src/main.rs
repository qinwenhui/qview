//! qview — 高性能原生文本浏览器
//!
//! Entry point. Initialises the window, fonts, theme, and hands off to
//! `QLogApp` for the main loop.

#![windows_subsystem = "windows"]

mod agent;
mod app;
mod assets;
mod config;
mod dialogs;
mod editor;
mod fonts;
mod layout;
mod logger;
mod mem_diag;
mod menu;
mod statusbar;
mod style;
mod theme_data;
mod toolbar;
mod viewer;
#[cfg(windows)]
mod win32;

use tokio::runtime::Runtime;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

/// Decode `assets/icon.ico` into an `egui::IconData` for the window icon
/// (title bar + taskbar).  Returns `None` when no icon is present — the app
/// then falls back to the exe's default icon.
fn load_window_icon() -> Option<Arc<egui::IconData>> {
    // 优先读 sidecar (`<exe>/assets/icon.ico`)；找不到则用编译期嵌入。
    let bytes = crate::assets::icon_bytes();
    let img = image::load_from_memory(&bytes).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    crate::log_info!(
        "main",
        "加载窗口图标 (source={}, {}x{})",
        crate::assets::icon_source(),
        width,
        height
    );
    Some(Arc::new(egui::IconData {
        rgba: rgba.into_raw(),
        width,
        height,
    }))
}

fn main() -> Result<()> {
    // Init logger as early as possible.  Use the same data dir as the config.
    let data_dir = config::AppConfig::config_dir().unwrap_or_else(|| PathBuf::from("data"));
    logger::init(&data_dir);
    // 原始 LLM 请求/响应日志改为**配置开关**（默认关）：AppConfig.llm_raw_log 在
    // app 启动时通过 QVIEW_LLM_RAW_LOG 控制（config::apply_llm_raw_log），
    // 设置面板 → AI 可实时开关，避免诊断日志默认落盘。
    // 把 contexa-rs / qview-agent 的 tracing 事件也汇入 qview.log（此前全部丢失）
    logger::init_tracing();
    crate::log_info!("main", "qview v{} 启动, 数据目录: {}", env!("CARGO_PKG_VERSION"), data_dir.display());

    let path = std::env::args().nth(1).map(PathBuf::from);

    // 全局 tokio runtime（GUI 侧需要的 spawn 都用这个）
    let tokio_rt: Arc<Runtime> = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .thread_name("qview-tokio")
            .build()
            .map_err(|e| anyhow::anyhow!("tokio runtime: {e}"))?,
    );

    // Window/taskbar icon.
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 860.0])
        .with_min_inner_size([800.0, 500.0])
        .with_title("文本浏览器 · qview");
    if let Some(icon) = load_window_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };

    let tokio_for_app = Arc::clone(&tokio_rt);
    eframe::run_native(
        "qview",
        options,
        Box::new(move |cc| {
            let mut app = app::QLogApp::default();
            app.init_fonts_and_theme(&cc.egui_ctx);
            app.init_agent(cc.egui_ctx.clone(), tokio_for_app.clone());

            mem_diag::write_report("After init_fonts_and_theme", &app);

            if let Some(p) = path.clone() {
                crate::log_info!("main", "命令行参数打开文件: {}", p.display());
                app.try_open(p);
            }

            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
}
