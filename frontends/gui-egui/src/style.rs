//! Theme system — wraps `ThemeColors` inside a `Theme` struct that can
//! apply itself to an egui `Context`. Supports built-in themes and optional
//! JSON-file overrides from `assets/themes/`.

use egui::{CornerRadius, Context, Stroke, Visuals};

use crate::theme_data::{all_builtin_themes, ThemeColors};

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub dark_mode: bool,
    pub colors: ThemeColors,
}

impl Theme {
    /// Build a Theme from a built-in palette.
    pub fn from_colors(name: &str, dark_mode: bool, colors: ThemeColors) -> Self {
        Self {
            name: name.to_string(),
            dark_mode,
            colors,
        }
    }

    /// Return the six built-in themes.
    pub fn all_builtin() -> Vec<Self> {
        all_builtin_themes()
            .into_iter()
            .map(|(name, colors)| {
                let dark = name != "Light";
                Self::from_colors(name, dark, colors)
            })
            .collect()
    }

    /// Find a built-in theme by name (case-insensitive prefix match).
    #[allow(dead_code)]
    pub fn find_builtin(name: &str) -> Option<Self> {
        Self::all_builtin().into_iter().find(|t| {
            t.name
                .to_lowercase()
                .starts_with(&name.to_lowercase())
        })
    }

    /// Apply this theme's colours to the egui context.
    pub fn apply_to(&self, ctx: &Context) {
        let mut style = (*ctx.style()).clone();
        let c = &self.colors;

        style.visuals = Visuals {
            dark_mode: self.dark_mode,
            override_text_color: Some(c.text_primary),
            window_corner_radius: CornerRadius::same(8),
            window_shadow: egui::epaint::Shadow {
                offset: [0, 8].into(),
                blur: 24,
                spread: 0,
                color: egui::Color32::BLACK.gamma_multiply(if self.dark_mode { 0.4 } else { 0.12 }),
            },
            window_fill: c.bg_primary,
            window_stroke: Stroke::new(1.0, c.bg_hover),
            panel_fill: c.bg_primary,
            faint_bg_color: c.bg_secondary,
            extreme_bg_color: c.bg_active,
            code_bg_color: c.bg_tertiary,
            warn_fg_color: c.warning,
            error_fg_color: c.error,
            hyperlink_color: c.text_link,
            selection: egui::style::Selection {
                bg_fill: c.selection_bg,
                stroke: Stroke::new(1.0, c.selection_border),
            },
            widgets: egui::style::Widgets {
                noninteractive: egui::style::WidgetVisuals {
                    bg_fill: c.bg_tertiary,
                    weak_bg_fill: c.bg_secondary,
                    bg_stroke: Stroke::new(1.0, c.bg_hover),
                    corner_radius: CornerRadius::same(4),
                    fg_stroke: Stroke::new(1.0, c.text_secondary),
                    expansion: 0.0,
                },
                inactive: egui::style::WidgetVisuals {
                    bg_fill: c.bg_tertiary,
                    weak_bg_fill: c.bg_secondary,
                    bg_stroke: Stroke::new(1.0, c.text_disabled),
                    corner_radius: CornerRadius::same(5),
                    fg_stroke: Stroke::new(1.5, c.text_primary),
                    expansion: 0.0,
                },
                hovered: egui::style::WidgetVisuals {
                    bg_fill: c.bg_hover,
                    weak_bg_fill: c.bg_hover,
                    bg_stroke: Stroke::new(1.0, c.text_secondary),
                    corner_radius: CornerRadius::same(5),
                    fg_stroke: Stroke::new(1.5, c.text_primary),
                    expansion: 0.5,
                },
                active: egui::style::WidgetVisuals {
                    bg_fill: c.bg_active,
                    weak_bg_fill: c.bg_active,
                    bg_stroke: Stroke::new(1.0, c.text_primary),
                    corner_radius: CornerRadius::same(4),
                    fg_stroke: Stroke::new(2.0, c.text_primary),
                    expansion: 0.0,
                },
                open: egui::style::WidgetVisuals {
                    bg_fill: c.bg_tertiary,
                    weak_bg_fill: c.bg_secondary,
                    bg_stroke: Stroke::new(1.0, c.text_secondary),
                    corner_radius: CornerRadius::same(5),
                    fg_stroke: Stroke::new(1.5, c.text_primary),
                    expansion: 0.0,
                },
            },
            ..Default::default()
        };

        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        style.spacing.indent = 16.0;

        ctx.set_style(style);
    }
}
