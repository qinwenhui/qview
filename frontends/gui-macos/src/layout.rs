//! 换行感知几何估算。
//!
//! 渲染本身用逐行 `visual_rows` 精确累积 y 坐标；但滚动条比例、goto_line、
//! 首可见行等必须在不知道整行布局的情况下快速估算，所以用固定放大因子
//! `WRAP_FACTOR`（与 egui viewer.rs 一致）。估算只影响滚动/跳转的精度，
//! 不影响可见行渲染的几何正确性。

/// 自动换行模式下行高放大因子（长行折行后平均占用 ~1.8 行）。
pub const WRAP_FACTOR: f64 = 1.8;

/// 估算单行步进（换行模式下放大）。
#[inline]
pub fn estimated_row_step(row_h: f64, word_wrap: bool) -> f64 {
    if word_wrap {
        row_h * WRAP_FACTOR
    } else {
        row_h
    }
}

/// 估算总内容高度（滚动条比例 / 文档尺寸用）。
#[inline]
pub fn estimate_content_h(total_lines: u64, row_h: f64, word_wrap: bool) -> f64 {
    total_lines as f64 * estimated_row_step(row_h, word_wrap)
}

/// 估算某行（0-based）的 y 坐标（goto / 滚动定位用）。
#[inline]
pub fn estimate_line_y(line: u64, row_h: f64, word_wrap: bool) -> f64 {
    line as f64 * estimated_row_step(row_h, word_wrap)
}

/// 估算首可见行（从视口顶部 scroll_y 出发）。
#[inline]
pub fn first_visible_line(scroll_y: f64, row_h: f64, word_wrap: bool) -> u64 {
    let step = estimated_row_step(row_h, word_wrap);
    (scroll_y / step).floor().max(0.0) as u64
}

/// 估算一个视口高度内大概能容纳多少行（渲染缓冲用）。
#[inline]
pub fn estimate_visible_lines(clip_h: f64, row_h: f64, word_wrap: bool) -> u64 {
    let step = estimated_row_step(row_h, word_wrap);
    ((clip_h / step).ceil() as u64).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_wrap_step_equals_row_h() {
        assert_eq!(estimated_row_step(18.0, false), 18.0);
        assert_eq!(estimate_line_y(10, 18.0, false), 180.0);
        assert_eq!(first_visible_line(180.0, 18.0, false), 10);
    }

    #[test]
    fn wrap_step_uses_factor() {
        assert_eq!(estimated_row_step(18.0, true), 18.0 * WRAP_FACTOR);
        assert_eq!(estimate_content_h(100, 18.0, true), 100.0 * 18.0 * WRAP_FACTOR);
        assert_eq!(estimate_line_y(10, 18.0, true), 10.0 * 18.0 * WRAP_FACTOR);
    }

    #[test]
    fn first_visible_line_never_underflows() {
        assert_eq!(first_visible_line(0.0, 18.0, false), 0);
        assert_eq!(first_visible_line(5.0, 18.0, true), 0);
        assert_eq!(estimate_visible_lines(0.0, 18.0, false), 1);
    }
}
