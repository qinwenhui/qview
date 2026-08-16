//! Settings dialog — tabbed interface for display, search, and theme config.

use egui::{Color32, Context};
use crate::log_debug;
use crate::app::QLogApp;
use qview_core::config::IndexBuildMode;

pub fn render_settings(ctx: &Context, app: &mut QLogApp) {
    crate::dialogs::centered_window(ctx, "设置", [720.0, 440.0])
        .fixed_size([720.0, 440.0])
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("设置")
                    .size(18.0)
                    .strong()
                    .color(Color32::from_rgb(191, 201, 214)),
            );
            ui.add_space(12.0);

            // ---- tab bar ----
            ui.horizontal(|ui| {
                ui.selectable_value(&mut app.settings_tab, 0, "显示");
                ui.selectable_value(&mut app.settings_tab, 1, "搜索");
                ui.selectable_value(&mut app.settings_tab, 2, "主题");
                ui.selectable_value(&mut app.settings_tab, 3, "引擎");
                ui.selectable_value(&mut app.settings_tab, 4, "AI");
            });
            ui.separator();
            ui.add_space(8.0);

            match app.settings_tab {
                0 => render_display_tab(ui, app),
                1 => render_search_tab(ui, app),
                2 => render_theme_tab(ui, app, ctx),
                3 => render_engine_tab(ui, app),
                4 => render_ai_tab(ui, app),
                _ => {}
            }

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                // Left button
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("应用").color(Color32::WHITE).size(14.0),
                        )
                        .fill(Color32::from_rgb(15, 157, 89))
                        .min_size(egui::vec2(100.0, 30.0)),
                    )
                    .clicked()
                {
                    log_debug!("settings", "应用设置并关闭");

                    // If encoding changed and a file is open, reload it now.
                    let encoding_changed = app.engine.as_ref().map_or(false, |arc| {
                        arc.lock().encoding.name() != app.config.engine.encoding.as_str()
                    });
                    if encoding_changed {
                        log_debug!("settings", "编码已变更 (→ {}), 重新加载文件", app.config.engine.encoding);
                        app.save_config();
                        app.show_settings = false;
                        if let Some(ref path) = app.path.clone() {
                            app.try_open(path.clone());
                        }
                    } else {
                        // AI 配置改动立即重建 Agent 运行时（复用常驻服务，当前文件不重新 mmap）
                        app.rebuild_agent_runtime(ctx);
                        app.save_config();
                        app.show_settings = false;
                    }
                }
                // Right button
                ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("取消").color(Color32::WHITE).size(14.0),
                            )
                            .fill(Color32::from_rgb(83, 91, 105))
                            .min_size(egui::vec2(100.0, 30.0)),
                        )
                        .clicked()
                    {
                        log_debug!("settings", "取消设置");
                        app.show_settings = false;
                    }
                });
            });
        });
}

// ---------------------------------------------------------------------------
// Display tab
// ---------------------------------------------------------------------------

fn render_display_tab(ui: &mut egui::Ui, app: &mut QLogApp) {
    egui::ScrollArea::vertical()
        .max_height(280.0)
        .show(ui, |ui| {
            // ---- font family ----
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("字体:")
                        .size(13.0)
                        .color(Color32::from_gray(190)),
                );
                let current_name = app
                    .available_fonts
                    .get(app.selected_font)
                    .cloned()
                    .unwrap_or_default();
                egui::ComboBox::from_id_salt("settings_font")
                    .width(200.0)
                    .selected_text(&current_name)
                    .show_ui(ui, |ui| {
                        for (i, name) in app.available_fonts.iter().enumerate() {
                            if ui
                                .selectable_label(i == app.selected_font, name.as_str())
                                .clicked()
                            {
                                log_debug!("settings", "切换字体: {} (index={})", name, i);
                                app.selected_font = i;
                            }
                        }
                    });
                ui.label(
                    egui::RichText::new("(重启生效)")
                        .size(11.0)
                        .color(Color32::from_gray(130)),
                );
            });

            ui.add_space(10.0);

            // ---- font size ----
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("字体大小:")
                        .size(13.0)
                        .color(Color32::from_gray(190)),
                );
                if ui.add(
                    egui::Slider::new(&mut app.font_size, 8.0..=32.0)
                        .text("px")
                        .fixed_decimals(0),
                ).changed() {
                    log_debug!("settings", "字体大小: {}px", app.font_size);
                    // Font size affects text width — refresh the horizontal
                    // scrollbar range instead of keeping the old (too wide) max.
                    app.invalidate_content_width();
                }
            });

            ui.add_space(8.0);

            // ---- row height ----
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("行高:")
                        .size(13.0)
                        .color(Color32::from_gray(190)),
                );
                if ui.add(
                    egui::Slider::new(&mut app.row_h, 14.0..=36.0)
                        .text("px")
                        .fixed_decimals(0),
                ).changed() {
                    log_debug!("settings", "行高: {}px", app.row_h);
                    app.invalidate_content_width();
                }
            });

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);

            // ---- display toggles ----
            if ui.checkbox(&mut app.show_line_numbers, "显示行号").changed() {
                log_debug!("settings", "显示行号: {}", app.show_line_numbers);
            }
            if ui.checkbox(&mut app.word_wrap, "自动换行").changed() {
                log_debug!("settings", "自动换行: {}", app.word_wrap);
            }
            if ui.checkbox(&mut app.show_whitespace, "显示空白字符").changed() {
                log_debug!("settings", "显示空白字符: {}", app.show_whitespace);
            }
            if ui.checkbox(&mut app.level_coloring, "日志级别着色").changed() {
                log_debug!("settings", "日志着色: {}", app.level_coloring);
            }
            if ui.checkbox(&mut app.show_indent_guides, "缩进参考线").changed() {
                log_debug!("settings", "缩进参考线: {}", app.show_indent_guides);
            }
        });
}

// ---------------------------------------------------------------------------
// Search tab
// ---------------------------------------------------------------------------

fn render_search_tab(ui: &mut egui::Ui, app: &mut QLogApp) {
    ui.add_space(4.0);
    if ui.checkbox(&mut app.case_sensitive, "大小写敏感").changed() {
        log_debug!("settings", "搜索-大小写敏感: {}", app.case_sensitive);
    }
    ui.add_space(4.0);
    if ui.checkbox(&mut app.use_regex, "正则表达式搜索").changed() {
        log_debug!("settings", "搜索-正则表达式: {}", app.use_regex);
    }
    ui.add_space(4.0);
    if ui.checkbox(&mut app.whole_word, "整词匹配").changed() {
        log_debug!("settings", "搜索-整词匹配: {}", app.whole_word);
    }

    ui.add_space(16.0);
    ui.label(
        egui::RichText::new("搜索历史（最多 20 条，自动保存）")
            .size(11.0)
            .color(Color32::from_gray(130)),
    );
}

// ---------------------------------------------------------------------------
// Theme tab
// ---------------------------------------------------------------------------

fn render_theme_tab(ui: &mut egui::Ui, app: &mut QLogApp, ctx: &Context) {
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("选择主题（即时生效）:")
            .size(13.0)
            .color(Color32::from_gray(190)),
    );
    ui.add_space(8.0);

    for theme in app.themes.clone().iter() {
        let selected = app.config.gui.theme == theme.name;
        let label = if selected {
            format!("◉ {}", theme.name)
        } else {
            format!("○ {}", theme.name)
        };
        if ui
            .selectable_label(selected, label)
            .clicked()
        {
            log_debug!("settings", "切换主题: {}", theme.name);
            app.switch_theme(&theme.name, ctx);
        }
    }

    ui.add_space(16.0);
    ui.label(
        egui::RichText::new("高级用户可在 assets/themes/ 放置自定义 JSON 主题文件")
            .size(11.0)
            .color(Color32::from_gray(130)),
    );
}

// ---------------------------------------------------------------------------
// Engine tab
// ---------------------------------------------------------------------------

fn render_engine_tab(ui: &mut egui::Ui, app: &mut QLogApp) {
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("资源与性能设置，按功能分组。鼠标悬停各项可查看说明")
            .size(11.0)
            .color(Color32::from_gray(130)),
    );
    ui.add_space(6.0);

    egui::ScrollArea::vertical()
        .max_height(300.0)
        .show(ui, |ui| {
            // --------------------------------------------------------------
            // ▸ 打开文件
            // --------------------------------------------------------------
            section_title(ui, "打开文件");
            ui.columns(2, |cols| {
                // ---- 文本编码 | 小文件阈值 ----
                cols[0].horizontal(|ui| {
                    ui.label(lbl("文本编码:"));
                    let encodings = crate::app::ENCODINGS;
                    let current = app.config.engine.encoding.clone();
                    let current_label = encodings
                        .iter()
                        .find(|(k, _)| *k == current.as_str())
                        .map(|(_, v)| *v)
                        .unwrap_or("UTF-8 (Unicode)");
                    egui::ComboBox::from_id_salt("settings_encoding")
                        .width(170.0)
                        .selected_text(current_label)
                        .show_ui(ui, |ui| {
                            for (key, label) in encodings {
                                let selected = *key == current.as_str();
                                if ui.selectable_label(selected, *label).clicked() {
                                    log_debug!("settings", "引擎-编码: {} ({})", key, label);
                                    app.config.engine.encoding = key.to_string();
                                }
                            }
                        })
                        .response
                        .on_hover_text(
                            "打开文件时按此编码解码文字，设错会乱码。中文老日志常用 GBK/GB18030。改动会立即以新编码重新加载当前文件。",
                        );
                });
                cols[1].horizontal(|ui| {
                    ui.label(lbl("小文件阈值:"));
                    let thresholds_mb: [(u64, &str); 5] = [
                        (1, "1 MB"), (5, "5 MB"), (10, "10 MB"), (50, "50 MB"), (100, "100 MB"),
                    ];
                    let current_mb = app.config.engine.small_file_threshold / (1024 * 1024);
                    let current_label = thresholds_mb
                        .iter()
                        .find(|(mb, _)| *mb == current_mb)
                        .map(|(_, label)| *label)
                        .unwrap_or("10 MB");
                    egui::ComboBox::from_id_salt("settings_small_file_threshold")
                        .width(110.0)
                        .selected_text(current_label)
                        .show_ui(ui, |ui| {
                            for (mb, label) in &thresholds_mb {
                                if ui.selectable_label(*mb == current_mb, *label).clicked() {
                                    log_debug!("settings", "引擎-小文件阈值: {} MB", mb);
                                    app.config.engine.small_file_threshold = mb * 1024 * 1024;
                                }
                            }
                        })
                        .response
                        .on_hover_text(
                            "小于此大小的文件视为『小文件』：打开时一次性读入内存并同步建索引，不写磁盘缓存。调大→更多文件秒开但占内存更多；调小→更多文件走后台流式索引（不占内存，首次打开要等几秒）。按日常文件大小调整，默认 10MB。",
                        );
                });
            });

            ui.columns(2, |cols| {
                // ---- 行缓存条目 | 索引缓存 ----
                cols[0].horizontal(|ui| {
                    ui.label(lbl("行缓存条目:"));
                    let capacities: [(usize, &str); 4] = [
                        (5_000, "5,000"), (10_000, "10,000"),
                        (20_000, "20,000"), (50_000, "50,000"),
                    ];
                    let current_label = capacities
                        .iter()
                        .find(|(c, _)| *c == app.config.engine.line_cache_capacity)
                        .map(|(_, label)| *label)
                        .unwrap_or("10,000");
                    egui::ComboBox::from_id_salt("settings_line_cache")
                        .width(90.0)
                        .selected_text(current_label)
                        .show_ui(ui, |ui| {
                            for (cap, label) in &capacities {
                                if ui.selectable_label(*cap == app.config.engine.line_cache_capacity, *label).clicked() {
                                    log_debug!("settings", "引擎-行缓存: {} 条", cap);
                                    app.config.engine.line_cache_capacity = *cap;
                                }
                            }
                        })
                        .response
                        .on_hover_text(
                            "缓存最近浏览过的原始行文本，避免滚动时反复解码。调大→滚动更顺滑、占内存更多（每条约几十到几百字节）；调小→省内存、超大文件回滚可能略卡。默认 1 万条。",
                        );
                });
                cols[1].horizontal(|ui| {
                    ui.label(lbl("索引缓存:"));
                    ui.checkbox(&mut app.config.engine.index_cache_enabled, "")
                        .on_hover_text(
                            "勾选后把行索引存成 .qli 文件，下次打开同文件瞬间完成。取消→每次打开都重建索引（多等几秒）但省磁盘空间。小文件不写缓存。",
                        );
                });
            });

            // ---- 索引目录 ----
            ui.horizontal(|ui| {
                ui.label(lbl("索引目录:"));
                let dir_path = app
                    .config
                    .engine
                    .index_dir
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "（未设置，保存在日志文件旁）".to_string());
                ui.add_sized(
                    [190.0, 20.0],
                    egui::Label::new(
                        egui::RichText::new(dir_path).size(11.0).color(Color32::from_gray(140)),
                    ).truncate(),
                )
                .on_hover_text(".qli 索引文件的存放目录。默认在程序 data 目录下集中存放，便于清理；缓存文件以日志路径哈希命名，不冲突。");
            });

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);

            // --------------------------------------------------------------
            // ▸ 索引构建
            // --------------------------------------------------------------
            section_title(ui, "索引构建");

            // ---- 性能预设：一键联动 扫描窗口 + 扫描线程 ----
            // 实测结论（见 parallel.rs）：64MB 窗口 + 自动线程（核数−1）是
            // 已验证的最优组合。占满所有核会让读取线程被抢占、磁盘空转，实测
            // 反而更慢；更大的窗口在部分磁盘上也不会更快。故线程一律保持自动，
            // 128MB 仅作为"大窗口试用"保留，并明确提示可能无提升。
            let presets: [(&str, u32, u32); 3] = [
                ("省内存（优先界面流畅）", 32, 0),
                ("均衡（推荐）", 64, 0),
                ("高性能（大窗口·试用）", 128, 0),
            ];
            let cur_w = app.config.engine.scan_window_mb;
            let cur_t = app.config.engine.scan_threads;
            let current_preset = presets
                .iter()
                .find(|(_, w, t)| *w == cur_w && *t == cur_t)
                .map(|(n, _, _)| *n)
                .unwrap_or("自定义（手动调整）");
            ui.horizontal(|ui| {
                ui.label(lbl("性能预设:"));
                egui::ComboBox::from_id_salt("settings_perf_preset")
                    .width(210.0)
                    .selected_text(current_preset)
                    .show_ui(ui, |ui| {
                        for (name, w, t) in presets {
                            if ui.selectable_label(name == current_preset, name).clicked() {
                                log_debug!("settings", "引擎-性能预设: {}", name);
                                app.config.engine.scan_window_mb = w;
                                app.config.engine.scan_threads = t;
                            }
                        }
                        if ui
                            .selectable_label(current_preset == "自定义（手动调整）", "自定义（手动调整）")
                            .clicked()
                        {
                            // 手动改「扫描窗口」/「扫描线程」会自动回到自定义。
                        }
                    })
                    .response
                    .on_hover_text(
                        "一键选择内存与速度的平衡。实测结论：64MB 窗口 + 自动线程（核数−1）是最优组合——占满所有核会让读取线程被抢占、磁盘空转（实测反而更慢），更大的窗口在部分磁盘上也不会更快。省内存=32MB（双缓冲少占 ~64MB 内存，构建略慢）；均衡=64MB（推荐）；高性能=128MB 大窗口试用（若没有变快请切回均衡）。手动修改『扫描窗口』或『扫描线程』会自动变为『自定义』。",
                    );
            });

            ui.columns(2, |cols| {
                // ---- 构建方式 | 扫描窗口 ----
                cols[0].horizontal(|ui| {
                    ui.label(lbl("构建方式:"));
                    let cur = app.config.engine.index_build_mode;
                    let cur_label = match cur {
                        IndexBuildMode::Sparse => "稀疏采样（省内存）",
                        IndexBuildMode::Full => "全量偏移（省 CPU）",
                    };
                    egui::ComboBox::from_id_salt("settings_index_build_mode")
                        .width(170.0)
                        .selected_text(cur_label)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(
                                cur == IndexBuildMode::Sparse,
                                "稀疏采样（省内存 · 单遍）",
                            ).clicked() {
                                log_debug!("settings", "引擎-索引构建方式: 稀疏采样 (Sparse)");
                                app.config.engine.index_build_mode = IndexBuildMode::Sparse;
                            }
                            if ui.selectable_label(
                                cur == IndexBuildMode::Full,
                                "全量偏移（单遍 · 峰值内存高）",
                            ).clicked() {
                                log_debug!("settings", "引擎-索引构建方式: 全量偏移 (Full)");
                                app.config.engine.index_build_mode = IndexBuildMode::Full;
                            }
                        })
                        .response
                        .on_hover_text(
                            "稀疏（推荐）：只记录每 128 行的位置，构建省内存（1 亿行省约 800MB）。全量：记录每一行位置，峰值内存更高（约 8 字节/行）但构建更省 CPU。两者都只读一遍文件、结果一致，可随时切换（下次打开文件生效）。",
                        );
                });
                cols[1].horizontal(|ui| {
                    ui.label(lbl("扫描窗口:"));
                    let windows: [(u32, &str); 5] = [
                        (16, "16 MB"), (32, "32 MB"), (64, "64 MB（默认）"),
                        (128, "128 MB"), (256, "256 MB"),
                    ];
                    let cur = app.config.engine.scan_window_mb;
                    let cur_label = windows
                        .iter()
                        .find(|(v, _)| *v == cur)
                        .map(|(_, l)| *l)
                        .unwrap_or("64 MB（默认）");
                    egui::ComboBox::from_id_salt("settings_scan_window")
                        .width(120.0)
                        .selected_text(cur_label)
                        .show_ui(ui, |ui| {
                            for (v, label) in &windows {
                                if ui.selectable_label(*v == cur, *label).clicked() {
                                    log_debug!("settings", "引擎-扫描窗口: {} MB", v);
                                    app.config.engine.scan_window_mb = *v;
                                }
                            }
                        })
                        .response
                        .on_hover_text(
                            "索引和搜索从磁盘流式读取的块大小（Windows 上绕过系统缓存，不占系统内存）。调大→大文件构建/搜索略快（窗口边界更少），但程序占用内存增大（双缓冲≈2 倍窗口，128MB 窗口约占 256MB）；调小→省内存、速度略降。普通场景保持 64MB，内存吃紧用 32MB，追求大文件极限速度且内存充裕用 128MB 以上。改动不影响已建索引，下次打开文件/搜索生效。",
                        );
                });
            });

            // ---- 扫描线程 ----
            ui.horizontal(|ui| {
                ui.label(lbl("扫描线程:"));
                let cur = app.config.engine.scan_threads;
                let opts: [(u32, &str); 7] = [
                    (0, "自动（留 1 核给界面）"),
                    (1, "1"), (2, "2"), (4, "4"), (8, "8"), (16, "16"), (32, "32"),
                ];
                let cur_label = opts
                    .iter()
                    .find(|(v, _)| *v == cur)
                    .map(|(_, l)| l.to_string())
                    .unwrap_or_else(|| format!("{} 线程", cur));
                egui::ComboBox::from_id_salt("settings_scan_threads")
                    .width(190.0)
                    .selected_text(&cur_label)
                    .show_ui(ui, |ui| {
                        for (v, label) in &opts {
                            if ui.selectable_label(*v == cur, *label).clicked() {
                                log_debug!("settings", "引擎-扫描线程: {}", v);
                                app.config.engine.scan_threads = *v;
                            }
                        }
                    })
                    .response
                    .on_hover_text(
                        "索引构建与搜索使用的 CPU 核数。自动=总核数−1，留 1 核给界面和读取线程，大文件扫描时窗口不卡、磁盘不空转。调大→扫描更快、界面可能卡顿；调小→界面流畅、扫描变慢。始终保留 1 核（上限=总核数−1，占满所有核实测反而更慢）。线程池在启动时创建，改后需重启生效。",
                    );
            });

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);

            // --------------------------------------------------------------
            // ▸ 搜索资源
            // --------------------------------------------------------------
            section_title(ui, "搜索资源");
            ui.columns(2, |cols| {
                // ---- 采样间隔 | 采样上限 ----
                cols[0].horizontal(|ui| {
                    ui.label(lbl("采样间隔:"));
                    let intervals: [(u32, &str); 5] = [
                        (50, "50"), (100, "100（默认）"), (200, "200"), (500, "500"), (1000, "1000"),
                    ];
                    let cur = app.config.engine.search.sample_interval;
                    let cur_label = intervals
                        .iter()
                        .find(|(v, _)| *v == cur)
                        .map(|(_, l)| *l)
                        .unwrap_or("100（默认）");
                    egui::ComboBox::from_id_salt("settings_search_interval")
                        .width(130.0)
                        .selected_text(cur_label)
                        .show_ui(ui, |ui| {
                            for (v, label) in &intervals {
                                if ui.selectable_label(*v == cur, *label).clicked() {
                                    log_debug!("settings", "引擎-搜索采样间隔: {}", v);
                                    app.config.engine.search.sample_interval = *v;
                                }
                            }
                        })
                        .response
                        .on_hover_text(
                            "命中很多时只记录每 N 个命中之一的位置用于跳转，命中总数始终精确。调大（如 1000）→更省内存、跳转稍慢；调小（如 50）→跳转快、占内存多。内存≈命中数÷间隔×8 字节，默认 100。",
                        );
                });
                cols[1].horizontal(|ui| {
                    ui.label(lbl("采样上限:"));
                    let caps: [(usize, &str); 5] = [
                        (1_000_000, "100 万"), (5_000_000, "500 万"),
                        (10_000_000, "1000 万（默认）"), (20_000_000, "2000 万"), (50_000_000, "5000 万"),
                    ];
                    let cur = app.config.engine.search.max_samples;
                    let cur_label = caps
                        .iter()
                        .find(|(v, _)| *v == cur)
                        .map(|(_, l)| *l)
                        .unwrap_or("1000 万（默认）");
                    egui::ComboBox::from_id_salt("settings_search_max_samples")
                        .width(140.0)
                        .selected_text(cur_label)
                        .show_ui(ui, |ui| {
                            for (v, label) in &caps {
                                if ui.selectable_label(*v == cur, *label).clicked() {
                                    log_debug!("settings", "引擎-搜索采样上限: {}", v);
                                    app.config.engine.search.max_samples = *v;
                                }
                            }
                        })
                        .response
                        .on_hover_text(
                            "最多保存多少个命中位置，超过后导航到未记录范围会回退重扫。调大→导航更完整、占内存多（上限×8 字节，1000 万≈80MB）；调小→省内存、超大结果集导航变慢。默认 1000 万。",
                        );
                });
            });
        });

    ui.add_space(10.0);
    ui.separator();
    ui.add_space(8.0);

    ui.label(
        egui::RichText::new("生效时机：编码=立即重载当前文件；采样间隔/上限=下次搜索；构建方式/小文件阈值/行缓存/索引目录/扫描窗口=下次打开文件；扫描线程=重启后生效")
            .size(11.0)
            .color(Color32::from_gray(130)),
    );
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new("采样覆盖 = 间隔 × 上限（默认 100×1000万 = 10 亿条命中，超出后导航变慢）；命中总数始终精确")
            .size(11.0)
            .color(Color32::from_gray(130)),
    );
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new("点击「应用」时若编码变更会自动以新编码重新加载当前文件")
            .size(11.0)
            .color(Color32::from_gray(130)),
    );
}

// ---------------------------------------------------------------------------
// AI tab（器灵 Agent 配置）
// ---------------------------------------------------------------------------

fn render_ai_tab(ui: &mut egui::Ui, app: &mut QLogApp) {
    use qview_agent::config::LlmProvider;

    let provider = app.config.agent.provider.provider;
    const W: f32 = 210.0; // 统一输入框宽度

    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("配置器灵连接的 LLM 服务；改完点「应用」立即生效。API Key 直接填在下方，无需环境变量。")
            .size(11.0)
            .color(Color32::from_gray(140)),
    );
    ui.add_space(6.0);

    // ---- ⚠️ 数据安全警告（醒目、常驻；宽而矮，不挤占配置项）----
    egui::Frame::NONE
        .fill(Color32::from_rgb(62, 46, 18)) // 琥珀暗底
        .stroke(egui::Stroke::new(1.0, Color32::from_rgb(214, 158, 46)))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::same(7))
        .show(ui, |ui| {
            // 一段自然流动的正文，满宽自动换行 → 行数最少、框最矮
            ui.set_max_width(ui.available_width());
            ui.label(
                egui::RichText::new("⚠️ 数据安全警告：")
                    .size(12.5)
                    .strong()
                    .color(Color32::from_rgb(255, 205, 92)),
            );
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(
                    "请勿使用第三方模型 API（尤其各类「中转站 / 代理接口」）分析生产日志等敏感数据——中转调用会经第三方服务，日志中的 IP、账号、密钥、业务数据可能被留存或泄露；发往远程模型的内容一律视为可能被对方记录。如需分析敏感内容，请改用企业内部模型（内网 Ollama / 企业私有端点）。",
                )
                .size(11.5)
                .color(Color32::from_rgb(244, 214, 164)),
            );
        });
    ui.add_space(8.0);

    // ---- LLM 原始请求日志开关（默认关；开则写 {data_dir}/llm_raw.log）----
    let before = app.config.llm_raw_log;
    ui.checkbox(
        &mut app.config.llm_raw_log,
        "记录 LLM 原始请求 / 响应到 llm_raw.log（诊断用，可能含敏感数据，默认关）",
    );
    if app.config.llm_raw_log != before {
        // 实时生效：contexa-llm 每次调用读 QVIEW_LLM_RAW_LOG，无需重启
        app.config.apply_llm_raw_log();
        app.config.save();
        log_debug!(
            "settings",
            "LLM 原始请求日志: {}",
            if app.config.llm_raw_log { "开" } else { "关" }
        );
    }
    ui.add_space(8.0);

    egui::ScrollArea::vertical()
        .max_height(300.0)
        .show(ui, |ui| {
            // --------------------------------------------------------------
            // ▸ 连接
            // --------------------------------------------------------------
            section_title(ui, "连接");
            egui::Grid::new("ai_conn_grid")
                .num_columns(2)
                .spacing([12.0, 7.0])
                .show(ui, |ui| {
                    // Provider
                    ui.label(lbl("Provider"));
                    let providers: [(LlmProvider, &str); 5] = [
                        (LlmProvider::Mock, "Mock · 离线演示"),
                        (LlmProvider::OpenAI, "OpenAI"),
                        (LlmProvider::OpenAICompat, "OpenAI 兼容端点"),
                        (LlmProvider::Ollama, "Ollama · 本地"),
                        (LlmProvider::DeepSeek, "DeepSeek"),
                    ];
                    let cur = provider;
                    let cur_label = providers
                        .iter()
                        .find(|(p, _)| *p == cur)
                        .map(|(_, l)| *l)
                        .unwrap_or("Mock · 离线演示");
                    egui::ComboBox::from_id_salt("ai_provider")
                        .width(W)
                        .selected_text(cur_label)
                        .show_ui(ui, |ui| {
                            for (p, label) in providers {
                                if ui.selectable_label(p == cur, label).clicked() {
                                    log_debug!("settings", "AI-Provider: {:?}", p);
                                    app.config.agent.provider.provider = p;
                                }
                            }
                        })
                        .response
                        .on_hover_text("LLM 服务商：Mock 离线演示（不发网络请求）；OpenAI 官方接口；OpenAI 兼容端点用于企业内网 / 通用服务；Ollama 走本地推理；DeepSeek 官方 API。");
                    ui.end_row();

                    // 模型（Mock 忽略）
                    ui.label(lbl("模型"));
                    ui.add(
                        egui::TextEdit::singleline(&mut app.config.agent.provider.model)
                            .desired_width(W),
                    )
                    .on_hover_text("真实 LLM 必填。Mock 忽略。");
                    ui.end_row();

                    // Base URL
                    let mut base_url = app.config.agent.provider.base_url.clone().unwrap_or_default();
                    ui.label(lbl("Base URL"));
                    let base_url_resp = ui.add(
                        egui::TextEdit::singleline(&mut base_url).desired_width(W),
                    );
                    if base_url_resp
                        .on_hover_text("自定义 API 地址；留空则用所选 Provider 的默认地址（如 Ollama 的 http://localhost:11434/v1）。OpenAI 兼容端点 / 企业内网代理在此填写。")
                        .changed()
                    {
                        app.config.agent.provider.base_url = non_empty(base_url);
                    }
                    ui.end_row();

                    // API Key（密码框；可切换显示）
                    let mut key = app.config.agent.provider.api_key.clone().unwrap_or_default();
                    ui.label(lbl("API Key"));
                    ui.horizontal(|ui| {
                        let key_resp = ui.add(
                            egui::TextEdit::singleline(&mut key)
                                .password(!app.agent_show_key)
                                .desired_width(W - 44.0),
                        );
                        if key_resp
                            .on_hover_text("访问密钥，直接填写即可（存于本地 config.json）。勿用第三方中转站分析敏感日志——内容会经对方服务留存。")
                            .changed()
                        {
                            app.config.agent.provider.api_key = non_empty(key);
                        }
                        ui.checkbox(&mut app.agent_show_key, "")
                            .on_hover_text("明文显示");
                    });
                    ui.end_row();

                    // 环境变量回退（高级）
                    let mut env = app.config.agent.provider.api_key_env.clone().unwrap_or_default();
                    ui.label(lbl("Env 变量"));
                    let env_resp = ui.add(
                        egui::TextEdit::singleline(&mut env)
                            .desired_width(W)
                            .hint_text("OPENAI_API_KEY"),
                    );
                    if env_resp
                        .on_hover_text("高级：填环境变量名（如 OPENAI_API_KEY）则从环境变量读取密钥，且优先于直接填写的 API Key；留空 = 只用上面的 Key。")
                        .changed()
                    {
                        app.config.agent.provider.api_key_env = non_empty(env);
                    }
                    ui.end_row();

                    // 温度 / 最大 token（真实 LLM 才显示）
                    if provider != LlmProvider::Mock {
                        let mut temp = app.config.agent.provider.temperature.unwrap_or(0.0);
                        ui.label(lbl("Temperature"));
                        let temp_resp =
                            ui.add(egui::Slider::new(&mut temp, 0.0..=2.0).fixed_decimals(1));
                        if temp_resp
                            .on_hover_text("采样随机度 0~2：越高回复越发散/有创意，越低越稳定/可复现。注意：DeepSeek 推理模型开启思考时此值被忽略。")
                            .changed()
                        {
                            app.config.agent.provider.temperature = (temp > 0.0).then_some(temp);
                        }
                        ui.end_row();
                        let mut mt = app.config.agent.provider.max_tokens.unwrap_or(0);
                        ui.label(lbl("最大 Tokens"));
                        let mt_resp =
                            ui.add(egui::DragValue::new(&mut mt).range(0..=1_000_000).speed(100));
                        if mt_resp
                            .on_hover_text("单次回复输出上限：模型一轮最多生成的 token 数（默认 4000 ≈ 6000+ 中文字）。回复常被截断就调大；嫌慢就调小。只影响输出，不管输入历史。")
                            .changed()
                        {
                            app.config.agent.provider.max_tokens = (mt > 0).then_some(mt);
                        }
                        ui.end_row();

                        // 思考强度（DeepSeek 推理模型专用）。
                        // 按 DeepSeek 官方文档，OpenAI 协议下发两个独立字段：
                        //   - thinking.type: enabled/disabled（开关）
                        //   - reasoning_effort: low/high/xhigh/max（强度）
                        // `none` 档对应"关 thinking + 不发明 reasoning_effort"。
                        // 改了之后下一次 LLM 调用即生效。
                        let levels: [(&str, &str); 5] = [
                            ("none", "none（关闭思考）"),
                            ("low", "low（快）"),
                            ("high", "high（中）"),
                            ("xhigh", "xhigh（深）"),
                            ("max", "max（最深）"),
                        ];
                        let cur_label = app
                            .config
                            .agent
                            .provider
                            .reasoning_effort
                            .as_deref()
                            .and_then(|s| levels.iter().find(|(k, _)| *k == s).map(|(_, v)| *v))
                            .unwrap_or("low（快）");
                        ui.label(lbl("思考强度"));
                        egui::ComboBox::from_id_salt("ai_reasoning_effort")
                            .selected_text(cur_label)
                            .show_ui(ui, |ui| {
                                for (key, label) in levels {
                                    if ui
                                        .selectable_label(cur_label == label, label)
                                        .clicked()
                                    {
                                        app.config.agent.provider.reasoning_effort =
                                            Some(key.to_string());
                                        log_debug!(
                                            "settings",
                                            "AI-ReasoningEffort: {}",
                                            key
                                        );
                                    }
                                }
                            })
                            .response
                            .on_hover_text("DeepSeek 推理模型思考深度：none 关闭思考（最快）；low 快；high 中；xhigh 深；max 最深（最慢）。qview「工具调用 + 读全文」场景下 low 已够用，调高会明显变慢。");
                        ui.end_row();
                    }
                });

            // Mock 专用
            if provider == LlmProvider::Mock {
                ui.add_space(4.0);
                egui::Grid::new("ai_mock_grid")
                    .num_columns(2)
                    .spacing([12.0, 7.0])
                    .show(ui, |ui| {
                        let mut script = app
                            .config
                            .agent
                            .provider
                            .mock_script_path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default();
                        ui.label(lbl("脚本文件"));
                        let script_resp = ui.add(
                            egui::TextEdit::singleline(&mut script)
                                .desired_width(W)
                                .hint_text("Vec<LLMResponse> JSON"),
                        );
                        if script_resp
                            .on_hover_text("Mock 专用：指定一个 Vec<LLMResponse> JSON 文件，器灵按顺序回放其中内容作为模型回复（自动化测试用）。")
                            .changed()
                        {
                            app.config.agent.provider.mock_script_path = non_empty_path(script);
                        }
                        ui.end_row();
                        let mut static_text = app
                            .config
                            .agent
                            .provider
                            .mock_static
                            .clone()
                            .unwrap_or_default();
                        ui.label(lbl("静态回复"));
                        let static_resp = ui.add(
                            egui::TextEdit::singleline(&mut static_text)
                                .desired_width(W)
                                .hint_text("离线演示时直接返回这句话"),
                        );
                        if static_resp
                            .on_hover_text("Mock 专用：离线演示时每次固定返回这句话。")
                            .changed()
                        {
                            app.config.agent.provider.mock_static = non_empty(static_text);
                        }
                        ui.end_row();
                    });
            }

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);

            // --------------------------------------------------------------
            // ▸ 限额
            // --------------------------------------------------------------
            section_title(ui, "限额");
            egui::Grid::new("ai_limit_grid")
                .num_columns(4)
                .spacing([8.0, 7.0])
                .show(ui, |ui| {
                    ui.label(lbl("最大轮数"));
                    ui.add(egui::DragValue::new(&mut app.config.agent.max_tool_rounds).range(1..=64))
                        .on_hover_text("一次任务里「思考 + 工具调用」循环最多进行多少轮（默认 20）。到顶自动停下并汇报已做的部分。");
                    ui.label(lbl("工具调用"));
                    ui.add(egui::DragValue::new(&mut app.config.agent.max_tool_calls).range(1..=512))
                        .on_hover_text("整个任务累计最多调用多少次工具（默认 20），防止器灵失控反复调用。");
                    ui.end_row();
                    ui.label(lbl("Token 预算"));
                    ui.add(egui::DragValue::new(&mut app.config.agent.max_token_budget).range(1_000..=5_000_000).speed(1_000))
                        .on_hover_text("整个任务累计消耗的 token 上限（各轮输入 + 输出之和，默认 200,000），超了立即中止任务——是防「无限烧钱」的保险丝。想放开可调大。");
                    ui.label(lbl("墙钟(秒)"));
                    ui.add(egui::DragValue::new(&mut app.config.agent.max_wall_seconds).range(1.0..=3600.0))
                        .on_hover_text("整个任务最长运行多少秒（默认 300 = 5 分钟），超时中止。");
                    ui.end_row();
                    ui.label(lbl("并发工具"));
                    ui.add(egui::DragValue::new(&mut app.config.agent.max_tool_workers).range(1..=32))
                        .on_hover_text("同一时刻最多并行执行的工具数（默认 20）。并行多 = 快但更耗 token。");
                    ui.label(lbl("结果上限"));
                    ui.add(egui::DragValue::new(&mut app.config.agent.tool_result_max_chars).range(1_000..=200_000).speed(100))
                        .on_hover_text("单个工具结果最多回传多少字符给模型（默认 8000），超出截断——防止大文件读取把上下文塞爆。");
                    ui.end_row();
                });

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);

            // --------------------------------------------------------------
            // ▸ 行为 / 记忆
            // --------------------------------------------------------------
            section_title(ui, "行为 / 记忆");
            ui.checkbox(&mut app.config.agent.context_compress_enabled, "长上下文自动压缩")
                .on_hover_text("上下文接近上限时压缩较早内容");
            ui.checkbox(&mut app.config.agent.context_budget_enabled, "上下文预算（降级旧消息）")
                .on_hover_text("超出预算时把旧消息降级到概要");
            ui.checkbox(&mut app.config.agent.memory_enabled, "会话内记忆（InMemory）")
                .on_hover_text("跨任务记住关键结论");

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);

            // --------------------------------------------------------------
            // ▸ 权限 / 安全
            // --------------------------------------------------------------
            section_title(ui, "权限 / 安全");
            egui::Grid::new("ai_perm_grid")
                .num_columns(4)
                .spacing([8.0, 7.0])
                .show(ui, |ui| {
                    ui.label(lbl("单次读行数"));
                    ui.add(egui::DragValue::new(&mut app.config.agent.max_read_lines).range(1..=5000))
                        .on_hover_text("一次「读取文件」工具调用最多读多少行（默认 5000），防止把超大文件全量塞进上下文。");
                    ui.end_row();
                });
            let mut redact = app.config.agent.redact_patterns.join(",");
            ui.horizontal(|ui| {
                ui.add_sized([120.0, 20.0], egui::Label::new(lbl("脱敏正则")));
                let redact_resp = ui.add(
                    egui::TextEdit::singleline(&mut redact)
                        .desired_width(W)
                        .hint_text("password=\\S+ , token=[0-9]+"),
                );
                if redact_resp
                    .on_hover_text("逗号分隔的正则列表；匹配到的内容在发送给模型前被打码（如 password=\\S+、token=[0-9]+）。留空 = 不脱敏。")
                    .changed()
                {
                    app.config.agent.redact_patterns = redact
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
            });

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);

            // --------------------------------------------------------------
            // ▸ 审计
            // --------------------------------------------------------------
            section_title(ui, "审计");
            let mut audit_dir = app
                .config
                .agent
                .audit_dir
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            ui.horizontal(|ui| {
                ui.add_sized([120.0, 20.0], egui::Label::new(lbl("审计目录")));
                let audit_resp = ui.add(
                    egui::TextEdit::singleline(&mut audit_dir)
                        .desired_width(W)
                        .hint_text("留空 = 内存审计；填目录 = 落盘 ndjson"),
                );
                if audit_resp
                    .on_hover_text("留空 = 仅在内存中做审计（进程退出即丢）；填目录 = 把每次 LLM/工具调用写成 ndjson 落盘，便于事后排查。")
                    .changed()
                {
                    app.config.agent.audit_dir = non_empty_path(audit_dir);
                }
            });
        });

    ui.add_space(10.0);
    ui.separator();
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("生效时机：点击「应用」立即重建 Agent 运行时（进行中的任务会被取消）")
            .size(11.0)
            .color(Color32::from_gray(130)),
    );
}

/// 空白输入 → None（归一化设置页的空字段）。
fn non_empty(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn non_empty_path(s: String) -> Option<std::path::PathBuf> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(t))
    }
}

/// Section header inside the engine tab.
fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(format!("▸ {text}"))
            .size(14.0)
            .strong()
            .color(Color32::from_rgb(96, 178, 255)),
    );
    ui.add_space(4.0);
}

/// Engine-tab label style helper.
fn lbl(text: &str) -> egui::RichText {
    egui::RichText::new(text).size(13.0).color(Color32::from_gray(190))
}
