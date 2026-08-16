//! 视图滚动状态。
//!
//! `y` 是**有效可视行号**：非 wrap 时 == 逻辑行号；wrap 时 ≈ 逻辑行 × wrap_factor
//! （近似，首行锚定）。`wrap_factor` 每帧由渲染发布。

#[derive(Default)]
pub struct ScrollState {
    pub y: i64,
    pub h_scroll: i64,
    /// 每屏可视行数（渲染时更新）
    pub page_size_lines: i64,
    pub max_content_w: f64,
    pub wrap_factor: f64,
    pub thumb_dragging: bool,
    pub drag_start_scroll_y: i64,
    pub drag_start_h_scroll: i64,
    pub drag_start_mouse: i32,       // y for vscroll, x for hscroll
    pub drag_track_len: i32,         // track height for vscroll, track width for hscroll
    pub drag_thumb_len: i32,
    pub drag_total_lines: u64,       // for vscroll
    pub drag_max_scroll_px: f64,     // for hscroll
    pub max_h_scroll_px: f64,        // cached after paint
}

fn eff(total: u64, factor: f64) -> i64 {
    (total as f64 * factor.max(1.0)) as i64
}

impl ScrollState {
    pub fn reset(&mut self) {
        self.y = 0;
        self.h_scroll = 0;
        self.max_content_w = 0.0;
        self.wrap_factor = 1.0;
    }

    pub fn top(&mut self) { self.y = 0; }

    pub fn bottom(&mut self, total: u64) {
        let rows = eff(total, self.wrap_factor);
        self.y = rows.saturating_sub(self.page_size_lines).max(0);
    }

    /// 跳到指定逻辑行（y = line × wrap_factor）
    pub fn goto_line(&mut self, line: u64) {
        self.y = (line as f64 * self.wrap_factor.max(1.0)) as i64;
    }

    pub fn scroll_by_lines(&mut self, delta: i32) {
        self.y = (self.y + delta as i64).max(0);
    }

    pub fn page_up(&mut self) {
        self.y = (self.y - self.page_size_lines + self.page_size_lines / 4).max(0);
    }

    pub fn page_down(&mut self, total: u64) {
        let rows = eff(total, self.wrap_factor);
        let max_scroll = rows.saturating_sub(self.page_size_lines).max(0);
        self.y = (self.y + self.page_size_lines - self.page_size_lines / 4).min(max_scroll);
    }

    pub fn h_scroll_by(&mut self, delta_px: i32) {
        self.h_scroll = (self.h_scroll + delta_px as i64).max(0);
    }
}
