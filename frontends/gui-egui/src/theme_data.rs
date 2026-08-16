//! Built-in colour palettes for the six included themes. Each theme is a
//! `ThemeColors` struct; the `style` module wraps them inside `Theme`.

use egui::Color32;

// ---------------------------------------------------------------------------
// ThemeColors — every colour slot used by the UI
// ---------------------------------------------------------------------------

// Many fields are designed per the theme spec but only a subset are wired
// into the render code yet. Removing the struct-level dead_code warning so
// the public API stays complete and documented.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ThemeColors {
    pub bg_primary: Color32,
    pub bg_secondary: Color32,
    pub bg_tertiary: Color32,
    pub bg_hover: Color32,
    pub bg_active: Color32,

    pub text_primary: Color32,
    pub text_secondary: Color32,
    pub text_disabled: Color32,
    pub text_link: Color32,

    pub line_number: Color32,
    pub line_number_bg: Color32,

    pub level_error: Color32,
    pub level_warn: Color32,
    pub level_info: Color32,
    pub level_debug: Color32,
    pub level_trace: Color32,

    pub search_highlight: Color32,
    pub search_current: Color32,

    pub success: Color32,
    pub warning: Color32,
    pub error: Color32,
    pub info: Color32,

    pub scrollbar_track: Color32,
    pub scrollbar_thumb: Color32,
    pub scrollbar_hover: Color32,

    pub selection_bg: Color32,
    pub selection_border: Color32,

    pub btn_primary: Color32,
    pub btn_success: Color32,
    pub btn_danger: Color32,
    pub btn_neutral: Color32,
    pub btn_purple: Color32,
}

// ---------------------------------------------------------------------------
// Helper — parse "#RRGGBB" or "#RRGGBBAA"
// ---------------------------------------------------------------------------

fn hex(c: &str) -> Color32 {
    let c = c.trim_start_matches('#');
    let r = u8::from_str_radix(&c[0..2], 16).unwrap_or(0xAA);
    let g = u8::from_str_radix(&c[2..4], 16).unwrap_or(0xAA);
    let b = u8::from_str_radix(&c[4..6], 16).unwrap_or(0xAA);
    if c.len() >= 8 {
        let a = u8::from_str_radix(&c[6..8], 16).unwrap_or(0xFF);
        Color32::from_rgba_premultiplied(r, g, b, a)
    } else {
        Color32::from_rgb(r, g, b)
    }
}

fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    Color32::from_rgba_premultiplied(r, g, b, a)
}

// ---------------------------------------------------------------------------
// Individual theme definitions
// ---------------------------------------------------------------------------

/// Dark Pro — the default theme. Low-contrast, easy on the eyes.
pub fn dark_pro() -> ThemeColors {
    ThemeColors {
        bg_primary: hex("#1C1E22"),
        bg_secondary: hex("#23262A"),
        bg_tertiary: hex("#2D3136"),
        bg_hover: hex("#383D45"),
        bg_active: hex("#272B31"),

        text_primary: hex("#D2D2D2"),
        text_secondary: hex("#A0A0A0"),
        text_disabled: hex("#606060"),
        text_link: hex("#58A6F5"),

        line_number: hex("#848484"),
        line_number_bg: hex("#1C1E22"),

        level_error: hex("#E04754"),
        level_warn: hex("#FEC81E"),
        level_info: hex("#13C27E"),
        level_debug: hex("#77ABEF"),
        level_trace: hex("#9297A1"),

        search_highlight: hex("#F6D48D"),
        search_current: hex("#FF8C00"),

        success: hex("#3FB950"),
        warning: hex("#FEC81E"),
        error: hex("#E04754"),
        info: hex("#58A6F5"),

        scrollbar_track: hex("#2A2E32"),
        scrollbar_thumb: hex("#888F9F"),
        scrollbar_hover: hex("#9AA4AD"),

        selection_bg: rgba(58, 118, 220, 140),
        selection_border: hex("#629EF8"),

        btn_primary: hex("#2173ED"),
        btn_success: hex("#0F9D59"),
        btn_danger: hex("#DA434A"),
        btn_neutral: hex("#535B69"),
        btn_purple: hex("#855DCC"),
    }
}

/// Dark High Contrast — higher contrast for projectors / bright rooms.
pub fn dark_high_contrast() -> ThemeColors {
    ThemeColors {
        bg_primary: hex("#0D0E11"),
        bg_secondary: hex("#16181D"),
        bg_tertiary: hex("#1F2228"),
        bg_hover: hex("#2C3038"),
        bg_active: hex("#1A1D23"),

        text_primary: hex("#F0F0F0"),
        text_secondary: hex("#C0C0C0"),
        text_disabled: hex("#707070"),
        text_link: hex("#6DB8FF"),

        line_number: hex("#9A9A9A"),
        line_number_bg: hex("#0D0E11"),

        level_error: hex("#FF5252"),
        level_warn: hex("#FFD740"),
        level_info: hex("#00E676"),
        level_debug: hex("#82B1FF"),
        level_trace: hex("#B0B7C3"),

        search_highlight: hex("#FFF176"),
        search_current: hex("#FF9800"),

        success: hex("#00E676"),
        warning: hex("#FFD740"),
        error: hex("#FF5252"),
        info: hex("#6DB8FF"),

        scrollbar_track: hex("#1A1D23"),
        scrollbar_thumb: hex("#A0A8B4"),
        scrollbar_hover: hex("#C0C8D4"),

        selection_bg: rgba(80, 140, 240, 160),
        selection_border: hex("#80B8FF"),

        btn_primary: hex("#2979FF"),
        btn_success: hex("#00C853"),
        btn_danger: hex("#FF1744"),
        btn_neutral: hex("#667080"),
        btn_purple: hex("#9C6CE0"),
    }
}

/// Light — daytime office use.
pub fn light() -> ThemeColors {
    ThemeColors {
        bg_primary: hex("#FFFFFF"),
        bg_secondary: hex("#F5F5F5"),
        bg_tertiary: hex("#EBEBEB"),
        bg_hover: hex("#E0E0E0"),
        bg_active: hex("#D6D6D6"),

        text_primary: hex("#1A1A1A"),
        text_secondary: hex("#555555"),
        text_disabled: hex("#999999"),
        text_link: hex("#1565C0"),

        line_number: hex("#888888"),
        line_number_bg: hex("#FAFAFA"),

        level_error: hex("#C62828"),
        level_warn: hex("#F57F17"),
        level_info: hex("#00695C"),
        level_debug: hex("#1565C0"),
        level_trace: hex("#757575"),

        search_highlight: hex("#FFF176"),
        search_current: hex("#FF8F00"),

        success: hex("#2E7D32"),
        warning: hex("#F57F17"),
        error: hex("#C62828"),
        info: hex("#1565C0"),

        scrollbar_track: hex("#E8E8E8"),
        scrollbar_thumb: hex("#B0B0B0"),
        scrollbar_hover: hex("#909090"),

        selection_bg: rgba(33, 150, 243, 80),
        selection_border: hex("#42A5F5"),

        btn_primary: hex("#1976D2"),
        btn_success: hex("#388E3C"),
        btn_danger: hex("#D32F2F"),
        btn_neutral: hex("#757575"),
        btn_purple: hex("#7B1FA2"),
    }
}

/// Solarized Dark — classic low-contrast palette.
pub fn solarized_dark() -> ThemeColors {
    ThemeColors {
        bg_primary: hex("#002B36"),
        bg_secondary: hex("#073642"),
        bg_tertiary: hex("#0A3D4A"),
        bg_hover: hex("#0E4956"),
        bg_active: hex("#05323F"),

        text_primary: hex("#839496"),
        text_secondary: hex("#657B83"),
        text_disabled: hex("#586E75"),
        text_link: hex("#268BD2"),

        line_number: hex("#586E75"),
        line_number_bg: hex("#002B36"),

        level_error: hex("#DC322F"),
        level_warn: hex("#B58900"),
        level_info: hex("#859900"),
        level_debug: hex("#268BD2"),
        level_trace: hex("#93A1A1"),

        search_highlight: hex("#B58900"),
        search_current: hex("#CB4B16"),

        success: hex("#859900"),
        warning: hex("#B58900"),
        error: hex("#DC322F"),
        info: hex("#268BD2"),

        scrollbar_track: hex("#073642"),
        scrollbar_thumb: hex("#586E75"),
        scrollbar_hover: hex("#657B83"),

        selection_bg: rgba(38, 139, 210, 100),
        selection_border: hex("#268BD2"),

        btn_primary: hex("#268BD2"),
        btn_success: hex("#859900"),
        btn_danger: hex("#DC322F"),
        btn_neutral: hex("#586E75"),
        btn_purple: hex("#6C71C4"),
    }
}

/// Dracula — popular community dark theme.
pub fn dracula() -> ThemeColors {
    ThemeColors {
        bg_primary: hex("#282A36"),
        bg_secondary: hex("#313340"),
        bg_tertiary: hex("#3B3D4D"),
        bg_hover: hex("#46495B"),
        bg_active: hex("#343746"),

        text_primary: hex("#F8F8F2"),
        text_secondary: hex("#BFBFB5"),
        text_disabled: hex("#6272A4"),
        text_link: hex("#8BE9FD"),

        line_number: hex("#6272A4"),
        line_number_bg: hex("#282A36"),

        level_error: hex("#FF5555"),
        level_warn: hex("#FFB86C"),
        level_info: hex("#50FA7B"),
        level_debug: hex("#8BE9FD"),
        level_trace: hex("#BD93F9"),

        search_highlight: hex("#F1FA8C"),
        search_current: hex("#FFB86C"),

        success: hex("#50FA7B"),
        warning: hex("#FFB86C"),
        error: hex("#FF5555"),
        info: hex("#8BE9FD"),

        scrollbar_track: hex("#313340"),
        scrollbar_thumb: hex("#6272A4"),
        scrollbar_hover: hex("#7A89C5"),

        selection_bg: rgba(139, 233, 253, 100),
        selection_border: hex("#8BE9FD"),

        btn_primary: hex("#BD93F9"),
        btn_success: hex("#50FA7B"),
        btn_danger: hex("#FF5555"),
        btn_neutral: hex("#44475A"),
        btn_purple: hex("#BD93F9"),
    }
}

/// Monokai — code-editor classic.
pub fn monokai() -> ThemeColors {
    ThemeColors {
        bg_primary: hex("#272822"),
        bg_secondary: hex("#2E2F29"),
        bg_tertiary: hex("#383830"),
        bg_hover: hex("#44453D"),
        bg_active: hex("#32332D"),

        text_primary: hex("#F8F8F2"),
        text_secondary: hex("#A6A68A"),
        text_disabled: hex("#75715E"),
        text_link: hex("#66D9EF"),

        line_number: hex("#75715E"),
        line_number_bg: hex("#272822"),

        level_error: hex("#F92672"),
        level_warn: hex("#E6DB74"),
        level_info: hex("#A6E22E"),
        level_debug: hex("#66D9EF"),
        level_trace: hex("#AE81FF"),

        search_highlight: hex("#E6DB74"),
        search_current: hex("#FD971F"),

        success: hex("#A6E22E"),
        warning: hex("#E6DB74"),
        error: hex("#F92672"),
        info: hex("#66D9EF"),

        scrollbar_track: hex("#2E2F29"),
        scrollbar_thumb: hex("#75715E"),
        scrollbar_hover: hex("#8B8B70"),

        selection_bg: rgba(102, 217, 239, 100),
        selection_border: hex("#66D9EF"),

        btn_primary: hex("#66D9EF"),
        btn_success: hex("#A6E22E"),
        btn_danger: hex("#F92672"),
        btn_neutral: hex("#49483E"),
        btn_purple: hex("#AE81FF"),
    }
}

/// All built-in themes as (name, colours) pairs.
pub fn all_builtin_themes() -> Vec<(&'static str, ThemeColors)> {
    vec![
        ("Dark Pro", dark_pro()),
        ("Dark High Contrast", dark_high_contrast()),
        ("Light", light()),
        ("Solarized Dark", solarized_dark()),
        ("Dracula", dracula()),
        ("Monokai", monokai()),
    ]
}
