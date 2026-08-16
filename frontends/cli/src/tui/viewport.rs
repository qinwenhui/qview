//! Virtual scrolling viewport. Tracks the visible window (top line, height,
//! horizontal offset). On-demand rendering from mmap; no scrollback buffer.

#[derive(Debug, Clone)]
pub struct Viewport {
    /// Top line (0-indexed). Always in `[0, total_lines.saturating_sub(visible_h)]`.
    pub top_line: u64,
    /// Number of visible lines (set from terminal size).
    pub visible_h: u16,
    /// Horizontal offset for wide content (number of columns scrolled left).
    pub horiz_offset: u16,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            top_line: 0,
            visible_h: 24,
            horiz_offset: 0,
        }
    }
}

impl Viewport {
    pub fn new(visible_h: u16) -> Self {
        Self {
            visible_h,
            ..Default::default()
        }
    }

    pub fn resize(&mut self, h: u16) {
        self.visible_h = h.max(1);
    }

    pub fn scroll_down(&mut self, n: u64, total_lines: u64) {
        let max_top = total_lines.saturating_sub(self.visible_h as u64);
        self.top_line = self.top_line.saturating_add(n).min(max_top);
    }

    pub fn scroll_up(&mut self, n: u64) {
        self.top_line = self.top_line.saturating_sub(n);
    }

    pub fn page_down(&mut self, total_lines: u64) {
        self.scroll_down(self.visible_h as u64, total_lines);
    }

    pub fn page_up(&mut self) {
        self.scroll_up(self.visible_h as u64);
    }

    pub fn to_line(&mut self, line: u64, total_lines: u64) {
        let max_top = total_lines.saturating_sub(self.visible_h as u64);
        self.top_line = line.min(max_top);
    }

    pub fn center_on(&mut self, line: u64, total_lines: u64) {
        let half = (self.visible_h as u64) / 2;
        let target = line.saturating_sub(half);
        self.to_line(target, total_lines);
    }

    pub fn to_top(&mut self) {
        self.top_line = 0;
    }

    pub fn to_bottom(&mut self, total_lines: u64) {
        self.top_line = total_lines.saturating_sub(self.visible_h as u64);
    }

    pub fn scroll_right(&mut self, n: u16) {
        self.horiz_offset = self.horiz_offset.saturating_add(n);
    }

    pub fn scroll_left(&mut self, n: u16) {
        self.horiz_offset = self.horiz_offset.saturating_sub(n);
    }

    pub fn bottom_line(&self, total_lines: u64) -> u64 {
        (self.top_line + self.visible_h as u64).min(total_lines) - 1
    }
}