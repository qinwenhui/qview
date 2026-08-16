//! 主题系统：色板数据结构 + 6 套内置主题。
//!
//! 色板逐字段对齐 gui/egui/src/theme_data.rs（egui 前端），保证两前端视觉一致。
//! GDI 无逐像素 alpha，半透明色（selection_bg）在构造时预混合到主背景。

/// 8-bit RGB 颜色
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    /// 解析 "#RRGGBB"（忽略 # 前缀；非法分量回落 0xAA，与 egui 一致）
    pub fn hex(s: &str) -> Self {
        let c = s.trim_start_matches('#');
        let rd = u8::from_str_radix(c.get(0..2).unwrap_or(""), 16).unwrap_or(0xAA);
        let gd = u8::from_str_radix(c.get(2..4).unwrap_or(""), 16).unwrap_or(0xAA);
        let bd = u8::from_str_radix(c.get(4..6).unwrap_or(""), 16).unwrap_or(0xAA);
        Self { r: rd, g: gd, b: bd }
    }

    /// GDI 颜色：COLORREF = 0x00BBGGRR
    pub fn as_u32(&self) -> u32 {
        (self.b as u32) | ((self.g as u32) << 8) | ((self.r as u32) << 16)
    }

    /// 按 alpha(0..=255) 预混合到背景 `bg` 上（前景透明 → 与背景融合）
    pub fn blend(&self, bg: Rgb, alpha: u8) -> Self {
        if alpha == 255 {
            return *self;
        }
        let a = alpha as u32;
        let f = |fg: u8, b: u8| ((fg as u32 * a) + (b as u32 * (255 - a)) + 127) / 255;
        Self { r: f(self.r, bg.r) as u8, g: f(self.g, bg.g) as u8, b: f(self.b, bg.b) as u8 }
    }
}

/// 36 个颜色槽 —— 全部 UI 绘制只从这里取色。
#[derive(Debug, Clone, Copy)]
pub struct ThemeColors {
    // 背景
    pub bg_primary: Rgb,
    pub bg_secondary: Rgb,
    pub bg_tertiary: Rgb,
    pub bg_hover: Rgb,
    pub bg_active: Rgb,
    // 文字
    pub text_primary: Rgb,
    pub text_secondary: Rgb,
    pub text_disabled: Rgb,
    pub text_link: Rgb,
    // 行号
    pub line_number: Rgb,
    pub line_number_bg: Rgb,
    // 日志级别
    pub level_error: Rgb,
    pub level_warn: Rgb,
    pub level_info: Rgb,
    pub level_debug: Rgb,
    pub level_trace: Rgb,
    // 搜索
    pub search_highlight: Rgb,
    pub search_current: Rgb,
    // 语义
    pub success: Rgb,
    pub warning: Rgb,
    pub error: Rgb,
    pub info: Rgb,
    // 滚动条
    pub scrollbar_track: Rgb,
    pub scrollbar_thumb: Rgb,
    pub scrollbar_hover: Rgb,
    // 选区
    pub selection_bg: Rgb,
    pub selection_border: Rgb,
    // 按钮
    pub btn_primary: Rgb,
    pub btn_success: Rgb,
    pub btn_danger: Rgb,
    pub btn_neutral: Rgb,
    pub btn_purple: Rgb,
    // GDI 补充
    pub statusbar_bg: Rgb,
    pub statusbar_text: Rgb,
    pub whitespace_marker: Rgb,
    pub indent_guide: Rgb,
}

impl ThemeColors {
    /// 从 egui 色板构造：`selection_bg` 按 alpha 预混合到 `bg_primary`。
    fn new(
        bg_primary: Rgb, bg_secondary: Rgb, bg_tertiary: Rgb, bg_hover: Rgb, bg_active: Rgb,
        text_primary: Rgb, text_secondary: Rgb, text_disabled: Rgb, text_link: Rgb,
        line_number: Rgb, line_number_bg: Rgb,
        level_error: Rgb, level_warn: Rgb, level_info: Rgb, level_debug: Rgb, level_trace: Rgb,
        search_highlight: Rgb, search_current: Rgb,
        success: Rgb, warning: Rgb, error: Rgb, info: Rgb,
        scrollbar_track: Rgb, scrollbar_thumb: Rgb, scrollbar_hover: Rgb,
        sel_rgba: (u8, u8, u8, u8), selection_border: Rgb,
        btn_primary: Rgb, btn_success: Rgb, btn_danger: Rgb, btn_neutral: Rgb, btn_purple: Rgb,
    ) -> Self {
        let (sr, sg, sb, sa) = sel_rgba;
        let sel_src = Rgb { r: sr, g: sg, b: sb };
        Self {
            bg_primary, bg_secondary, bg_tertiary, bg_hover, bg_active,
            text_primary, text_secondary, text_disabled, text_link,
            line_number, line_number_bg,
            level_error, level_warn, level_info, level_debug, level_trace,
            search_highlight, search_current,
            success, warning, error, info,
            scrollbar_track, scrollbar_thumb, scrollbar_hover,
            selection_bg: sel_src.blend(bg_primary, sa),
            selection_border,
            btn_primary, btn_success, btn_danger, btn_neutral, btn_purple,
            statusbar_bg: bg_secondary,
            statusbar_text: text_secondary,
            whitespace_marker: text_disabled,
            indent_guide: Rgb { r: bg_hover.r, g: bg_hover.g, b: bg_hover.b },
        }
    }
}

/// Dark Pro —— 默认主题，低对比护眼。
fn dark_pro() -> ThemeColors {
    ThemeColors::new(
        Rgb::hex("#1C1E22"), Rgb::hex("#23262A"), Rgb::hex("#2D3136"),
        Rgb::hex("#383D45"), Rgb::hex("#272B31"),
        Rgb::hex("#D2D2D2"), Rgb::hex("#A0A0A0"), Rgb::hex("#606060"), Rgb::hex("#58A6F5"),
        Rgb::hex("#848484"), Rgb::hex("#1C1E22"),
        Rgb::hex("#E04754"), Rgb::hex("#FEC81E"), Rgb::hex("#13C27E"),
        Rgb::hex("#77ABEF"), Rgb::hex("#9297A1"),
        Rgb::hex("#F6D48D"), Rgb::hex("#FF8C00"),
        Rgb::hex("#3FB950"), Rgb::hex("#FEC81E"), Rgb::hex("#E04754"), Rgb::hex("#58A6F5"),
        Rgb::hex("#2A2E32"), Rgb::hex("#888F9F"), Rgb::hex("#9AA4AD"),
        (58, 118, 220, 140), Rgb::hex("#629EF8"),
        Rgb::hex("#2173ED"), Rgb::hex("#0F9D59"), Rgb::hex("#DA434A"),
        Rgb::hex("#535B69"), Rgb::hex("#855DCC"),
    )
}

/// Dark High Contrast —— 高对比，适合投影/强光。
fn dark_high_contrast() -> ThemeColors {
    ThemeColors::new(
        Rgb::hex("#0D0E11"), Rgb::hex("#16181D"), Rgb::hex("#1F2228"),
        Rgb::hex("#2C3038"), Rgb::hex("#1A1D23"),
        Rgb::hex("#F0F0F0"), Rgb::hex("#C0C0C0"), Rgb::hex("#707070"), Rgb::hex("#6DB8FF"),
        Rgb::hex("#9A9A9A"), Rgb::hex("#0D0E11"),
        Rgb::hex("#FF5252"), Rgb::hex("#FFD740"), Rgb::hex("#00E676"),
        Rgb::hex("#82B1FF"), Rgb::hex("#B0B7C3"),
        Rgb::hex("#FFF176"), Rgb::hex("#FF9800"),
        Rgb::hex("#00E676"), Rgb::hex("#FFD740"), Rgb::hex("#FF5252"), Rgb::hex("#6DB8FF"),
        Rgb::hex("#1A1D23"), Rgb::hex("#A0A8B4"), Rgb::hex("#C0C8D4"),
        (80, 140, 240, 160), Rgb::hex("#80B8FF"),
        Rgb::hex("#2979FF"), Rgb::hex("#00C853"), Rgb::hex("#FF1744"),
        Rgb::hex("#667080"), Rgb::hex("#9C6CE0"),
    )
}

/// Light —— 白天办公。
fn light() -> ThemeColors {
    ThemeColors::new(
        Rgb::hex("#FFFFFF"), Rgb::hex("#F5F5F5"), Rgb::hex("#EBEBEB"),
        Rgb::hex("#E0E0E0"), Rgb::hex("#D6D6D6"),
        Rgb::hex("#1A1A1A"), Rgb::hex("#555555"), Rgb::hex("#999999"), Rgb::hex("#1565C0"),
        Rgb::hex("#888888"), Rgb::hex("#FAFAFA"),
        Rgb::hex("#C62828"), Rgb::hex("#F57F17"), Rgb::hex("#00695C"),
        Rgb::hex("#1565C0"), Rgb::hex("#757575"),
        Rgb::hex("#FFF176"), Rgb::hex("#FF8F00"),
        Rgb::hex("#2E7D32"), Rgb::hex("#F57F17"), Rgb::hex("#C62828"), Rgb::hex("#1565C0"),
        Rgb::hex("#E8E8E8"), Rgb::hex("#B0B0B0"), Rgb::hex("#909090"),
        (33, 150, 243, 80), Rgb::hex("#42A5F5"),
        Rgb::hex("#1976D2"), Rgb::hex("#388E3C"), Rgb::hex("#D32F2F"),
        Rgb::hex("#757575"), Rgb::hex("#7B1FA2"),
    )
}

/// Solarized Dark —— 经典低对比。
fn solarized_dark() -> ThemeColors {
    ThemeColors::new(
        Rgb::hex("#002B36"), Rgb::hex("#073642"), Rgb::hex("#0A3D4A"),
        Rgb::hex("#0E4956"), Rgb::hex("#05323F"),
        Rgb::hex("#839496"), Rgb::hex("#657B83"), Rgb::hex("#586E75"), Rgb::hex("#268BD2"),
        Rgb::hex("#586E75"), Rgb::hex("#002B36"),
        Rgb::hex("#DC322F"), Rgb::hex("#B58900"), Rgb::hex("#859900"),
        Rgb::hex("#268BD2"), Rgb::hex("#93A1A1"),
        Rgb::hex("#B58900"), Rgb::hex("#CB4B16"),
        Rgb::hex("#859900"), Rgb::hex("#B58900"), Rgb::hex("#DC322F"), Rgb::hex("#268BD2"),
        Rgb::hex("#073642"), Rgb::hex("#586E75"), Rgb::hex("#657B83"),
        (38, 139, 210, 100), Rgb::hex("#268BD2"),
        Rgb::hex("#268BD2"), Rgb::hex("#859900"), Rgb::hex("#DC322F"),
        Rgb::hex("#586E75"), Rgb::hex("#6C71C4"),
    )
}

/// Dracula —— 流行社区暗色。
fn dracula() -> ThemeColors {
    ThemeColors::new(
        Rgb::hex("#282A36"), Rgb::hex("#313340"), Rgb::hex("#3B3D4D"),
        Rgb::hex("#46495B"), Rgb::hex("#343746"),
        Rgb::hex("#F8F8F2"), Rgb::hex("#BFBFB5"), Rgb::hex("#6272A4"), Rgb::hex("#8BE9FD"),
        Rgb::hex("#6272A4"), Rgb::hex("#282A36"),
        Rgb::hex("#FF5555"), Rgb::hex("#FFB86C"), Rgb::hex("#50FA7B"),
        Rgb::hex("#8BE9FD"), Rgb::hex("#BD93F9"),
        Rgb::hex("#F1FA8C"), Rgb::hex("#FFB86C"),
        Rgb::hex("#50FA7B"), Rgb::hex("#FFB86C"), Rgb::hex("#FF5555"), Rgb::hex("#8BE9FD"),
        Rgb::hex("#313340"), Rgb::hex("#6272A4"), Rgb::hex("#7A89C5"),
        (139, 233, 253, 100), Rgb::hex("#8BE9FD"),
        Rgb::hex("#BD93F9"), Rgb::hex("#50FA7B"), Rgb::hex("#FF5555"),
        Rgb::hex("#44475A"), Rgb::hex("#BD93F9"),
    )
}

/// Monokai —— 编辑器经典配色。
fn monokai() -> ThemeColors {
    ThemeColors::new(
        Rgb::hex("#272822"), Rgb::hex("#2E2F29"), Rgb::hex("#383830"),
        Rgb::hex("#44453D"), Rgb::hex("#32332D"),
        Rgb::hex("#F8F8F2"), Rgb::hex("#A6A68A"), Rgb::hex("#75715E"), Rgb::hex("#66D9EF"),
        Rgb::hex("#75715E"), Rgb::hex("#272822"),
        Rgb::hex("#F92672"), Rgb::hex("#E6DB74"), Rgb::hex("#A6E22E"),
        Rgb::hex("#66D9EF"), Rgb::hex("#AE81FF"),
        Rgb::hex("#E6DB74"), Rgb::hex("#FD971F"),
        Rgb::hex("#A6E22E"), Rgb::hex("#E6DB74"), Rgb::hex("#F92672"), Rgb::hex("#66D9EF"),
        Rgb::hex("#2E2F29"), Rgb::hex("#75715E"), Rgb::hex("#8B8B70"),
        (102, 217, 239, 100), Rgb::hex("#66D9EF"),
        Rgb::hex("#66D9EF"), Rgb::hex("#A6E22E"), Rgb::hex("#F92672"),
        Rgb::hex("#49483E"), Rgb::hex("#AE81FF"),
    )
}

pub const THEME_NAMES: [&str; 6] = [
    "Dark Pro",
    "Dark High Contrast",
    "Light",
    "Solarized Dark",
    "Dracula",
    "Monokai",
];

/// 第 i 套主题色板（i 与 THEME_NAMES 对齐）
pub fn builtin(i: usize) -> ThemeColors {
    match i {
        0 => dark_pro(),
        1 => dark_high_contrast(),
        2 => light(),
        3 => solarized_dark(),
        4 => dracula(),
        _ => monokai(),
    }
}

/// 按名称前缀（大小写不敏感）定位主题索引，找不到回退 0（Dark Pro）。
pub fn find_index(name: &str) -> usize {
    let n = name.to_ascii_lowercase();
    THEME_NAMES
        .iter()
        .position(|t| t.to_ascii_lowercase() == n || t.to_ascii_lowercase().starts_with(&n))
        .unwrap_or(0)
}
