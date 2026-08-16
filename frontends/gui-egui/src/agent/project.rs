//! ViewIntent → QLogApp 状态投影（架构 §9）。
//!
//! 投影纪律（架构 §9.3，`FocusLine` 已按用户要求改为**自动投影**）：
//! - 自动投影：`FocusLine`（器灵跳转 → 主视图立即跟随）、`HighlightRange`（附加色条，不滚动）、
//!   `ShowMessage`（toast）、`OpenPanel`（开面板）。
//! - 点击投影：`ApplyFilter` 由时间线条目"点击应用"触发（整体淡化主视图较侵入，避免器灵分析时反复改变主视图渲染）。
//! - 失败的 ViewIntent 永远不影响 Agent 任务（忽略即可）。

use qview_application::protocol::view_intent::ViewIntent;

use crate::app::QLogApp;

/// 自动投影不滚动主视图的 ViewIntent。
/// 点击应用的（FocusLine / ApplyFilter）只留气泡，等用户点；
/// OpenDocument / NewDocument 会切换主视图，但用户说「打开/新建这个看看」
/// 就是要真开，所以**自动应用**（气泡仍保留，可再点）。
pub fn apply_intent(app: &mut QLogApp, intent: &ViewIntent) {
    match intent {
        ViewIntent::FocusLine { line } => {
            // 用户要求：器灵执行跳转工具 → 主视图**立即**跟随跳过去（不再只留气泡点击）。
            app.agent_jump_to_line(*line);
        }
        ViewIntent::ApplyFilter { .. } => {
            // 过滤器整体淡化主视图（较侵入），仍只由气泡内点击应用，
            // 避免器灵分析时反复改变主视图渲染。见 panel.rs intent_row。
        }
        ViewIntent::OpenDocument { path } => {
            let pb = std::path::PathBuf::from(path);
            // 已打开同一文件（canonical 比对）则跳过，避免重开/重启索引
            let same = app.path.as_ref().is_some_and(|cur| match (cur.canonicalize(), pb.canonicalize()) {
                (Ok(a), Ok(b)) => a == b,
                _ => cur == &pb,
            });
            if !same {
                app.open_file(pb);
            }
        }
        ViewIntent::NewDocument { .. } => {
            app.create_new_file();
        }
        ViewIntent::ClearFilter => {
            app.agent_clear_filter();
        }
        ViewIntent::ToggleWordWrap { enabled } => {
            app.word_wrap = *enabled;
            app.config.gui.word_wrap = *enabled;
            app.save_config();
            crate::log_debug!("agent", "器灵切换自动换行: {enabled}");
        }
        ViewIntent::SwitchTheme { theme } => {
            let lower = theme.to_lowercase();
            if let Some(idx) = app
                .themes
                .iter()
                .position(|t| t.name.to_lowercase().starts_with(&lower))
            {
                app.current_theme_idx = idx;
                app.config.gui.theme = app.themes[idx].name.clone();
                app.save_config();
                crate::log_debug!("agent", "器灵切换主题: {}", app.themes[idx].name);
            } else {
                crate::log_warn!("agent", "器灵切换主题失败: 未知主题 {theme}");
                app.flash_status(format!("器灵: 未知主题 {theme}"), 4);
            }
        }
        ViewIntent::HighlightRange { start, end, kind } => {
            app.agent_highlights.push((*start, *end, *kind));
            // 防止超长 session 无界累积：最多保留 64 段（丢弃最旧的）
            if app.agent_highlights.len() > 64 {
                app.agent_highlights.remove(0);
            }
        }
        ViewIntent::OpenPanel { panel } => match panel {
            qview_application::protocol::view_intent::PanelKind::Agent => {
                // Agent 侧栏始终可见，无需动作
            }
            qview_application::protocol::view_intent::PanelKind::Annotation => {
                app.show_annotation_list = true;
            }
            qview_application::protocol::view_intent::PanelKind::Filter => {
                // 过滤器面板暂未独立实现；toast 提示
                app.flash_status("器灵: 建议使用过滤器（点击时间线的过滤条目可应用）", 3);
            }
        },
        ViewIntent::ShowMessage { level, text } => {
            let msg = format!("器灵: {text}");
            match level {
                qview_application::protocol::view_intent::MessageLevel::Info => {
                    crate::log_debug!("agent", "{msg}");
                    app.flash_status(msg, 4);
                }
                qview_application::protocol::view_intent::MessageLevel::Success => {
                    crate::log_debug!("agent", "{msg}");
                    app.flash_status(msg, 4);
                }
                qview_application::protocol::view_intent::MessageLevel::Warning => {
                    crate::log_warn!("agent", "{msg}");
                    app.flash_status(msg, 5);
                }
                qview_application::protocol::view_intent::MessageLevel::Error => {
                    crate::log_error!("agent", "{msg}");
                    app.flash_status(msg, 5);
                }
            }
        }
    }
}

