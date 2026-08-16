//! qview 安装向导 — egui 页面状态机（浅色淡蓝主题）。
//!
//! 载荷已由 build.rs 压缩进 `PAYLOAD`；运行时先解压到 `%TEMP%` 暂存目录，
//! 用暂存目录里的中文字体初始化 UI，然后按向导流程收集选项并执行安装。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui;
use egui::{Color32, FontData, FontDefinitions, FontFamily};
use qview_core::config::IndexBuildMode;

use qview_installer::install::{self, InstallOptions, Step, DEFAULT_EXTS};
use qview_installer::qpak;

static PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.qpak"));

/// Must stay in sync with `theme_data::all_builtin_themes()`.
const THEMES: [&str; 6] = [
    "Dark Pro",
    "Dark High Contrast",
    "Light",
    "Solarized Dark",
    "Dracula",
    "Monokai",
];

const STEPS: [&str; 5] = ["欢迎", "安装目录", "安装选项", "确认安装", "完成"];

// ---- palette (light blue theme) ----
const ACCENT: Color32 = Color32::from_rgb(43, 108, 225); // 主蓝
const SUCCESS: Color32 = Color32::from_rgb(20, 150, 95);
const DANGER: Color32 = Color32::from_rgb(205, 66, 70);
const BG_PAGE: Color32 = Color32::from_rgb(246, 249, 253); // 内容区浅蓝白
const BG_PANEL: Color32 = Color32::from_rgb(255, 255, 255); // 面板/侧栏白
const BG_WIDGET: Color32 = Color32::from_rgb(237, 242, 250); // 输入框/控件底
const BG_SOFT: Color32 = Color32::from_rgb(232, 240, 252); // 信息框浅蓝
const BORDER: Color32 = Color32::from_rgb(216, 225, 240);
const TEXT_PRIMARY: Color32 = Color32::from_rgb(28, 44, 76); // 深海军蓝
const TEXT_SECONDARY: Color32 = Color32::from_rgb(88, 102, 130);
const TEXT_HINT: Color32 = Color32::from_rgb(130, 144, 166);
const BTN_GRAY: Color32 = Color32::from_rgb(233, 238, 246); // 次级按钮底

fn main() {
    // ---- stage payload ----
    let staging = std::env::temp_dir().join(format!("qview-setup-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    if qpak::read_manifest(PAYLOAD).is_empty() {
        msg_box_error(
            "载荷为空。\n请先运行：\n  cargo run --release -p qview-installer --bin qview-bundle\n然后重新构建本安装器。",
        );
        return;
    }
    if let Err(e) = qpak::extract(PAYLOAD, &staging) {
        msg_box_error(&format!("解压载荷失败：{e}"));
        return;
    }

    let bundled_fonts = discover_asset_fonts(&staging);

    // Window title-bar icon, from the payload's own assets/icon.ico.
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([720.0, 540.0])
        .with_min_inner_size([680.0, 500.0])
        .with_title(&format!("qview {} 安装程序", env!("CARGO_PKG_VERSION")));
    if let Some(icon) = load_window_icon(&staging) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };

    let staging_for_ui = staging.clone();
    let _ = eframe::run_native(
        "qview 安装程序",
        options,
        Box::new(move |cc| {
            setup_fonts(&cc.egui_ctx, &staging_for_ui);
            setup_style(&cc.egui_ctx);
            Ok(Box::new(Wizard::new(staging_for_ui, bundled_fonts)))
        }),
    );

    // Best-effort cleanup of the staging dir.
    let _ = fs::remove_dir_all(&staging);
}

// ---------------------------------------------------------------------------
// Fonts + style
// ---------------------------------------------------------------------------

/// Register the bundled Chinese font so the wizard UI renders Chinese text.
fn setup_fonts(ctx: &egui::Context, staging: &Path) {
    let mut fonts = FontDefinitions::default();
    let mut names = Vec::new();
    let assets = staging.join("assets");
    if let Ok(entries) = fs::read_dir(&assets) {
        for e in entries.flatten() {
            let p = e.path();
            let ext = p
                .extension()
                .and_then(|x| x.to_str())
                .unwrap_or("")
                .to_lowercase();
            if matches!(ext.as_str(), "ttf" | "otf" | "ttc") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()).map(str::to_owned) {
                    if let Ok(bytes) = fs::read(&p) {
                        fonts.font_data.insert(
                            stem.clone(),
                            Arc::new(FontData::from_owned(bytes)),
                        );
                        names.push(stem);
                    }
                }
            }
        }
    }
    if names.is_empty() {
        if let Ok(bytes) = fs::read("C:/Windows/Fonts/msyh.ttc") {
            fonts
                .font_data
                .insert("微软雅黑".to_owned(), Arc::new(FontData::from_owned(bytes)));
            names.push("微软雅黑".to_owned());
        }
    }
    for n in &names {
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .push(n.clone());
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .push(n.clone());
    }
    ctx.set_fonts(fonts);
}

/// Decode the wizard's window icon from the staged payload's `assets/icon.ico`.
/// Returns `None` when the payload ships no icon (setup.exe then shows its
/// default window icon).
fn load_window_icon(staging: &Path) -> Option<Arc<egui::IconData>> {
    let path = staging.join("assets/icon.ico");
    if !path.is_file() {
        return None;
    }
    let img = image::open(&path).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    eprintln!("[setup] 加载窗口图标: {} ({}x{})", path.display(), width, height);
    Some(Arc::new(egui::IconData {
        rgba: rgba.into_raw(),
        width,
        height,
    }))
}

fn discover_asset_fonts(staging: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let assets = staging.join("assets");
    if let Ok(entries) = fs::read_dir(&assets) {
        for e in entries.flatten() {
            let p = e.path();
            let ext = p
                .extension()
                .and_then(|x| x.to_str())
                .unwrap_or("")
                .to_lowercase();
            if matches!(ext.as_str(), "ttf" | "otf" | "ttc") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    out.push(stem.to_owned());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Light-blue theme: light background, high-contrast dark navy text.
fn setup_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::light();
    v.override_text_color = Some(TEXT_PRIMARY);
    v.panel_fill = BG_PAGE;
    v.window_fill = BG_PANEL;
    v.faint_bg_color = BG_WIDGET;
    v.extreme_bg_color = Color32::from_rgb(230, 236, 246);
    v.window_stroke = egui::Stroke::new(1.0, BORDER);
    v.window_corner_radius = 8.0.into();
    v.window_shadow = egui::epaint::Shadow {
        offset: [0, 4].into(),
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(36),
    };
    v.widgets.noninteractive.bg_fill = BG_WIDGET;
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT_PRIMARY);
    v.widgets.inactive.bg_fill = BG_WIDGET;
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, TEXT_PRIMARY);
    v.widgets.hovered.bg_fill = Color32::from_rgb(224, 233, 248);
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.5, TEXT_PRIMARY);
    v.widgets.active.bg_fill = Color32::from_rgb(206, 222, 248);
    v.widgets.active.fg_stroke = egui::Stroke::new(1.5, TEXT_PRIMARY);
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = egui::Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;

    let mut style = (*ctx.style()).clone();
    style.visuals = v;
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::proportional(14.5),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::proportional(14.0),
    );
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::proportional(19.0),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        egui::FontId::proportional(12.5),
    );
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(16.0, 7.0);
    style.spacing.window_margin = egui::Margin::ZERO;
    ctx.set_style(style);
}

// ---------------------------------------------------------------------------
// Wizard
// ---------------------------------------------------------------------------

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Welcome,
    Dir,
    Options,
    Confirm,
    Done,
}

impl Page {
    fn index(self) -> usize {
        match self {
            Page::Welcome => 0,
            Page::Dir => 1,
            Page::Options => 2,
            Page::Confirm => 3,
            Page::Done => 4,
        }
    }
}

struct Wizard {
    staging: PathBuf,
    page: Page,

    target: String,
    theme_idx: usize,
    themes: Vec<String>,
    /// 0 = 跟随系统默认; 1.. = bundled_fonts[idx-1]
    font_idx: usize,
    bundled_fonts: Vec<String>,
    index_mode: IndexBuildMode,
    /// 0 = 自动（留 1 核给界面）; ≥ 1 = 固定线程数
    scan_threads: u32,
    desktop_shortcut: bool,
    assoc: [bool; DEFAULT_EXTS.len()],
    launch_after: bool,

    steps: Vec<Step>,
    install_ok: bool,
}

impl Wizard {
    fn new(staging: PathBuf, bundled_fonts: Vec<String>) -> Self {
        let default_dir = std::env::var("LOCALAPPDATA")
            .map(|d| PathBuf::from(d).join("Programs").join("qview"))
            .unwrap_or_else(|_| PathBuf::from("C:\\qview"));
        Self {
            staging,
            page: Page::Welcome,
            target: default_dir.to_string_lossy().into_owned(),
            theme_idx: 0,
            themes: THEMES.iter().map(|s| s.to_string()).collect(),
            font_idx: 0,
            bundled_fonts,
            index_mode: IndexBuildMode::Sparse,
            scan_threads: 0,
            desktop_shortcut: true,
            assoc: [true; DEFAULT_EXTS.len()],
            launch_after: true,
            steps: Vec::new(),
            install_ok: false,
        }
    }

    fn run_install(&mut self) {
        let opts = InstallOptions {
            target: PathBuf::from(&self.target),
            theme: self.themes[self.theme_idx].clone(),
            font: (self.font_idx > 0).then(|| self.bundled_fonts[self.font_idx - 1].clone()),
            index_mode: self.index_mode,
            scan_threads: self.scan_threads,
            desktop_shortcut: self.desktop_shortcut,
            assoc: DEFAULT_EXTS
                .iter()
                .zip(self.assoc.iter())
                .filter(|(_, &on)| on)
                .map(|(&e, _)| e)
                .collect(),
        };
        let mut steps = Vec::new();
        install::run(&opts, &self.staging, &mut steps);
        self.install_ok = steps.iter().all(|s| s.ok);
        self.steps = steps;
        self.page = Page::Done;
        if self.install_ok && self.launch_after {
            let _ = std::process::Command::new(opts.target.join("qview.exe")).spawn();
        }
    }

    // ---- pages ----

    fn page_welcome(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        title(ui, "欢迎");
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("即将把 qview 文本浏览器安装到您的计算机。")
                .size(14.0)
                .color(TEXT_SECONDARY),
        );
        ui.add_space(18.0);
        for line in [
            "·  10 GB+ 大文件秒开，内存占用低",
            "·  内置 AI 器灵小Q：对话式日志分析",
            "·  SIMD 搜索 + 正则双引擎",
            "·  超长行自动换行 + 会话历史回看",
            "·  vim 风格键位 + 行内编辑 + tail -f",
        ] {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(line).size(14.0).color(TEXT_PRIMARY));
            });
            ui.add_space(6.0);
        }
        ui.add_space(18.0);
        ui.label(
            egui::RichText::new("点击「下一步」选择安装目录。整个安装过程无需管理员权限。")
                .size(12.5)
                .color(TEXT_HINT),
        );
    }

    fn page_dir(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        title(ui, "选择安装目录");
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("安装到:").size(14.0).color(TEXT_SECONDARY));
            ui.add(
                egui::TextEdit::singleline(&mut self.target)
                    .desired_width(340.0)
                    .hint_text("安装目录"),
            );
            if ui
                .add(egui::Button::new("浏览…").min_size(egui::vec2(76.0, 30.0)))
                .clicked()
            {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    self.target = dir.to_string_lossy().into_owned();
                }
            }
        });
        ui.add_space(12.0);
        if self.target.trim().is_empty() {
            ui.label(egui::RichText::new("请选择一个安装目录").size(13.0).color(DANGER));
        } else {
            egui::Frame::new()
                .fill(BG_SOFT)
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    for line in [
                        format!("程序: {}\\qview.exe", self.target),
                        format!("配置: {}\\data\\config.json", self.target),
                        format!("索引缓存: {}\\data\\index\\", self.target),
                    ] {
                        ui.label(
                            egui::RichText::new(line).size(13.0).color(TEXT_PRIMARY),
                        );
                        ui.add_space(4.0);
                    }
                });
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new("推荐安装到用户目录（无需管理员权限，配置完全可写）。")
                    .size(12.5)
                    .color(TEXT_HINT),
            );
        }
    }

    fn page_options(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        title(ui, "安装选项");
        ui.add_space(10.0);

        row(ui, "界面主题", |ui| {
            egui::ComboBox::from_id_salt("opt_theme")
                .width(260.0)
                .selected_text(&self.themes[self.theme_idx])
                .show_ui(ui, |ui| {
                    for (i, name) in self.themes.iter().enumerate() {
                        ui.selectable_value(&mut self.theme_idx, i, name);
                    }
                });
        });

        row(ui, "默认字体", |ui| {
            let selected = if self.font_idx == 0 {
                "跟随系统默认".to_string()
            } else {
                self.bundled_fonts[self.font_idx - 1].clone()
            };
            egui::ComboBox::from_id_salt("opt_font")
                .width(260.0)
                .selected_text(&selected)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.font_idx, 0, "跟随系统默认");
                    for (i, name) in self.bundled_fonts.iter().enumerate() {
                        ui.selectable_value(&mut self.font_idx, i + 1, name);
                    }
                });
        });

        row(ui, "索引构建", |ui| {
            let labels = [
                (IndexBuildMode::Sparse, "稀疏采样（省内存）"),
                (IndexBuildMode::Full, "全量偏移（单遍快）"),
            ];
            let cur = self.index_mode;
            let cur_label = labels
                .iter()
                .find(|(m, _)| *m == cur)
                .map(|(_, l)| *l)
                .unwrap_or("稀疏采样（省内存）");
            egui::ComboBox::from_id_salt("opt_index_mode")
                .width(260.0)
                .selected_text(cur_label)
                .show_ui(ui, |ui| {
                    for (mode, label) in labels {
                        ui.selectable_value(&mut self.index_mode, mode, label);
                    }
                });
        });

        row(ui, "扫描线程", |ui| {
            let opts: [(u32, &str); 7] = [
                (0, "自动（推荐 · 留 1 核给界面）"),
                (1, "1"), (2, "2"), (4, "4"), (8, "8"), (16, "16"), (32, "32"),
            ];
            let cur = self.scan_threads;
            let cur_label = opts
                .iter()
                .find(|(v, _)| *v == cur)
                .map(|(_, l)| *l)
                .unwrap_or("自动（推荐 · 留 1 核给界面）");
            egui::ComboBox::from_id_salt("opt_scan_threads")
                .width(260.0)
                .selected_text(cur_label)
                .show_ui(ui, |ui| {
                    for (v, label) in opts {
                        ui.selectable_value(&mut self.scan_threads, v, label);
                    }
                });
        });
        ui.label(
            egui::RichText::new("用于索引构建和搜索的并行线程数；留 1 核给界面可避免大文件索引时窗口卡死")
                .size(12.0)
                .color(TEXT_HINT),
        );

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(10.0);
        ui.checkbox(&mut self.desktop_shortcut, "创建桌面快捷方式");
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("文件关联（右键 → 打开方式）:")
                .size(13.5)
                .color(TEXT_SECONDARY),
        );
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            for (i, ext) in DEFAULT_EXTS.iter().enumerate() {
                ui.checkbox(&mut self.assoc[i], *ext);
            }
        });
        ui.label(
            egui::RichText::new("关联后，双击这些文件或右键选择「打开方式」即可用 qview 打开")
                .size(12.0)
                .color(TEXT_HINT),
        );
    }

    fn page_confirm(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        title(ui, "确认安装");
        ui.add_space(12.0);

        let font_name = if self.font_idx == 0 {
            "跟随系统默认".to_string()
        } else {
            self.bundled_fonts[self.font_idx - 1].clone()
        };
        let mode_name = match self.index_mode {
            IndexBuildMode::Sparse => "稀疏采样（省内存）",
            IndexBuildMode::Full => "全量偏移（单遍快）",
        };
        let thread_name = if self.scan_threads == 0 {
            "自动（留 1 核给界面）".to_string()
        } else {
            format!("{} 线程", self.scan_threads)
        };
        let assoc_str = DEFAULT_EXTS
            .iter()
            .zip(self.assoc.iter())
            .filter(|(_, &on)| on)
            .map(|(&e, _)| e)
            .collect::<Vec<_>>()
            .join(" ");

        egui::Frame::new()
            .fill(BG_SOFT)
            .corner_radius(6.0)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                let rows: [(&str, String); 7] = [
                    ("安装目录", self.target.clone()),
                    ("界面主题", self.themes[self.theme_idx].clone()),
                    ("默认字体", font_name),
                    ("索引构建", mode_name.to_string()),
                    ("扫描线程", thread_name),
                    ("桌面快捷方式", if self.desktop_shortcut { "创建".into() } else { "不创建".into() }),
                    ("文件关联", if assoc_str.is_empty() { "无".into() } else { assoc_str }),
                ];
                egui::Grid::new("confirm_grid")
                    .num_columns(2)
                    .spacing([28.0, 9.0])
                    .show(ui, |ui| {
                        for (k, v) in rows {
                            ui.label(
                                egui::RichText::new(k).size(13.5).color(TEXT_SECONDARY),
                            );
                            ui.label(
                                egui::RichText::new(v).size(13.5).color(TEXT_PRIMARY),
                            );
                            ui.end_row();
                        }
                    });
            });

        ui.add_space(14.0);
        ui.checkbox(&mut self.launch_after, "安装完成后启动 qview");
    }

    fn page_done(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        if self.install_ok {
            ui.label(
                egui::RichText::new("✓ 安装完成")
                    .size(22.0)
                    .strong()
                    .color(SUCCESS),
            );
        } else {
            ui.label(
                egui::RichText::new("✗ 部分步骤失败")
                    .size(18.0)
                    .strong()
                    .color(DANGER),
            );
        }
        ui.add_space(10.0);
        for s in &self.steps {
            ui.horizontal(|ui| {
                let (mark, color) = if s.ok { ("✓", TEXT_SECONDARY) } else { ("✗", DANGER) };
                ui.label(egui::RichText::new(mark).color(color));
                ui.label(egui::RichText::new(s.label).color(color));
                if !s.detail.is_empty() {
                    ui.label(
                        egui::RichText::new(s.detail.as_str())
                            .size(11.5)
                            .color(TEXT_HINT),
                    );
                }
            });
            ui.add_space(4.0);
        }
        ui.add_space(12.0);
        ui.label(
            egui::RichText::new("可在 控制面板 → 程序和功能 中卸载，或运行安装目录下的 uninstall.exe")
                .size(12.0)
                .color(TEXT_HINT),
        );
    }

    // ---- navigation (uniform buttons, right-aligned) ----

    fn nav(&mut self, ui: &mut egui::Ui) {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // primary / success action (rightmost)
            match self.page {
                Page::Welcome => {
                    if self.btn(ui, "下一步", BtnKind::Primary, true).clicked() {
                        self.page = Page::Dir;
                    }
                }
                Page::Dir => {
                    if self
                        .btn(ui, "下一步", BtnKind::Primary, !self.target.trim().is_empty())
                        .clicked()
                    {
                        self.page = Page::Options;
                    }
                }
                Page::Options => {
                    if self.btn(ui, "下一步", BtnKind::Primary, true).clicked() {
                        self.page = Page::Confirm;
                    }
                }
                Page::Confirm => {
                    if self.btn(ui, "安装", BtnKind::Success, true).clicked() {
                        self.run_install();
                    }
                }
                Page::Done => {
                    if self.btn(ui, "完成", BtnKind::Success, true).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }

            // secondary (back)
            if self.page != Page::Welcome && self.page != Page::Done {
                if self.btn(ui, "上一步", BtnKind::Secondary, true).clicked() {
                    self.page = match self.page {
                        Page::Dir => Page::Welcome,
                        Page::Options => Page::Dir,
                        Page::Confirm => Page::Options,
                        _ => Page::Welcome,
                    };
                }
            }

            // ghost (cancel)
            if self.page != Page::Done {
                if self.btn(ui, "取消", BtnKind::Ghost, true).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        });
    }

    /// One uniform button for every nav action.
    fn btn(&self, ui: &mut egui::Ui, text: &str, kind: BtnKind, enabled: bool) -> egui::Response {
        let (fill, fg) = match kind {
            BtnKind::Primary => (ACCENT, Color32::WHITE),
            BtnKind::Success => (SUCCESS, Color32::WHITE),
            BtnKind::Secondary => (BTN_GRAY, TEXT_PRIMARY),
            BtnKind::Ghost => (Color32::TRANSPARENT, TEXT_SECONDARY),
        };
        let stroke = if kind == BtnKind::Ghost {
            egui::Stroke::new(1.0, BORDER)
        } else {
            egui::Stroke::NONE
        };
        let btn = egui::Button::new(egui::RichText::new(text).size(14.0).color(fg))
            .fill(fill)
            .stroke(stroke)
            .corner_radius(6.0)
            .min_size(egui::vec2(104.0, 34.0));
        ui.add_enabled(enabled, btn)
    }
}

impl eframe::App for Wizard {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Header (white bar, blue title).
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(BG_PANEL)
                    .inner_margin(egui::Margin {
                        left: 26, right: 26, top: 14, bottom: 12,
                    }),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("qview 文本浏览器")
                            .size(20.0)
                            .strong()
                            .color(ACCENT),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(env!("CARGO_PKG_VERSION"))
                            .size(13.0)
                            .color(TEXT_HINT),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new("高性能日志 / 文本浏览器")
                                .size(12.0)
                                .color(TEXT_HINT),
                        );
                    });
                });
            });

        // Nav (white bar, right-aligned uniform buttons).
        egui::TopBottomPanel::bottom("nav")
            .frame(
                egui::Frame::new()
                    .fill(BG_PANEL)
                    .inner_margin(egui::Margin {
                        left: 26, right: 26, top: 10, bottom: 12,
                    }),
            )
            .show(ctx, |ui| {
                ui.add_space(2.0);
                self.nav(ui);
            });

        // Steps sidebar.
        egui::SidePanel::left("steps")
            .exact_width(150.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(BG_PANEL)
                    .inner_margin(egui::Margin::symmetric(16, 22)),
            )
            .show(ctx, |ui| {
                let current = self.page.index();
                for (i, label) in STEPS.iter().enumerate() {
                    let (circle_fill, circle_fg, text_fg, strong) = if i == current {
                        (ACCENT, Color32::WHITE, TEXT_PRIMARY, true)
                    } else if i < current {
                        (SUCCESS, Color32::WHITE, TEXT_SECONDARY, false)
                    } else {
                        (Color32::from_rgb(222, 229, 238), Color32::from_rgb(150, 160, 180), Color32::from_rgb(150, 160, 180), false)
                    };
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
                        let painter = ui.painter();
                        painter.circle_filled(rect.center(), 12.0, circle_fill);
                        let mark = if i < current {
                            "✓".to_string()
                        } else {
                            (i + 1).to_string()
                        };
                        painter.text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            mark,
                            egui::FontId::proportional(11.0),
                            circle_fg,
                        );
                        ui.add_space(6.0);
                        let mut rt = egui::RichText::new(*label).size(13.5).color(text_fg);
                        if strong {
                            rt = rt.strong();
                        }
                        ui.label(rt);
                    });
                    ui.add_space(5.0);
                }
            });

        // Content (light page background).
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG_PAGE)
                    .inner_margin(egui::Margin {
                        left: 28, right: 28, top: 16, bottom: 16,
                    }),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.page {
                        Page::Welcome => self.page_welcome(ui),
                        Page::Dir => self.page_dir(ui),
                        Page::Options => self.page_options(ui),
                        Page::Confirm => self.page_confirm(ui),
                        Page::Done => self.page_done(ui),
                    });
            });
    }
}

// ---------------------------------------------------------------------------
// Small UI helpers
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum BtnKind {
    Primary,
    Success,
    Secondary,
    Ghost,
}

/// Page heading.
fn title(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(20.0)
            .strong()
            .color(TEXT_PRIMARY),
    );
}

/// One `label : control` row.
fn row(ui: &mut egui::Ui, label: &str, control: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [72.0, 26.0],
            egui::Label::new(egui::RichText::new(label).size(14.0).color(TEXT_SECONDARY)),
        );
        ui.add_space(6.0);
        control(ui);
    });
    ui.add_space(10.0);
}

/// Native error box for pre-GUI failures.
fn msg_box_error(text: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let t: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let c: Vec<u16> = "qview 安装程序".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        MessageBoxW(HWND::default(), PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), MB_OK | MB_ICONERROR);
    }
}
