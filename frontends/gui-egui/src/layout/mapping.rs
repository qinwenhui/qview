//! 坐标总闸（`ViewMapping`）—— 全浏览器唯一做「字符 ↔ 视觉行/列 ↔ 物理行」
//! 换算的地方。顶层（viewer / editor / app）要任何位置信息都问它，不再各自
//! 实现，杜绝三套坐标（字节 / 字符 / 视觉列）互不相通导致的偏移类 bug。
//!
//! 职责划分：
//! - `CharMetrics`：格宽 / 格高 + 像素 ↔ 字符列
//! - `HugeLayout`：超长行每个视觉行的字符范围
//! - `VisualRowModel`：物理行 ↔ 视觉行（普通行 + 超长行展开的估算模型）
//! - `ViewMapping`：组合上述三者的统一入口
//!
//! 字节层的换算（byte ↔ char）需要行文本上下文，由调用方（viewer / app）用
//! `char_col_to_byte` / `char_pos_to_byte_pos` 完成后再走本模块。

use crate::layout::HugeLayout;

/// 超过此字节数的行走 per-row（按字符切片 layout，避免大行一次 layout 整个
/// galley 后 caret/选区按「单行」处理出 bug）。阈值要小于普通 wrap_w 的字符数，
/// 8KB 足够——所有 wrap 后多行的行都触发。
pub const CHUNK_LINE_BYTES: usize = 8 * 1024;

/// 向上取整除法。
#[inline]
fn ceil_div(a: u64, b: u64) -> u64 {
    if b == 0 {
        0
    } else {
        (a + b - 1) / b
    }
}

/// 一行（字节长 `byte_len`）展开成多少个视觉行。普通行 = row_mult；
/// 超长行 = ceil(字节 / bytes_per_row) —— 连续 wrap（per-row 渲染就是整行连续
/// 换行，不分块）。旧实现按 32KB 分块向上取整再求和，会累积高估 ~8-11 行
/// （用户实测：跳转偏差），已改为连续估算。
pub fn line_visual_rows(byte_len: u64, bytes_per_row: u64, row_mult: u64) -> u64 {
    if byte_len <= CHUNK_LINE_BYTES as u64 {
        return row_mult;
    }
    ceil_div(byte_len, bytes_per_row)
}

/// 当前视口宽下的每行字节数估算（等宽字体：wrap_w / 字符宽）。
pub fn estimate_bytes_per_row(char_w: f32, wrap_w: f64) -> u64 {
    ((wrap_w / char_w.max(1.0) as f64).floor()).max(1.0) as u64
}

/// 物理行 ↔ 视觉行模型（每帧构建一次；viewer 渲染与 app 侧跳转共用）。
#[derive(Clone, Debug)]
pub struct VisualRowModel {
    /// 每行字节数估算（视口宽 / 字符宽）。
    pub bytes_per_row: u64,
    /// 普通行的视觉行数（word_wrap 估算 / 1）。
    pub row_mult: u64,
    pub row_h: f32,
    /// (物理行, 该行视觉行数)，仅超长行。
    pub huge: Vec<(u64, u64)>,
    /// prefix[i] = 前 i 个超长行累计的额外行数 (rows - row_mult)。
    pub prefix: Vec<u64>,
}

impl VisualRowModel {
    pub fn build(
        char_w: f32,
        wrap_w: f64,
        row_h: f32,
        row_mult: u64,
        huge_lines: &[(u64, u64)],
    ) -> Self {
        let bytes_per_row = estimate_bytes_per_row(char_w, wrap_w);
        let mut huge = Vec::with_capacity(huge_lines.len());
        let mut prefix = Vec::with_capacity(huge_lines.len() + 1);
        prefix.push(0u64);
        for &(l, blen) in huge_lines {
            let rows = line_visual_rows(blen, bytes_per_row, row_mult);
            huge.push((l, rows));
            prefix.push(*prefix.last().unwrap() + rows.saturating_sub(row_mult));
        }
        Self {
            bytes_per_row,
            row_mult,
            row_h,
            huge,
            prefix,
        }
    }

    /// 物理行 `line` **之前**的额外视觉行数（超长行展开贡献）。
    fn extra_before(&self, line: u64) -> u64 {
        let mut lo = 0usize;
        let mut hi = self.huge.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.huge[mid].0 < line {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        self.prefix[lo]
    }

    /// 物理行 → 该行视觉行的起点。
    pub fn line_to_visual(&self, line: u64) -> u64 {
        line.saturating_mul(self.row_mult)
            .saturating_add(self.extra_before(line))
    }

    /// 视觉行 → 该视觉行所在的物理行。
    pub fn visual_to_line(&self, v: u64) -> u64 {
        let mut lo = 0u64;
        let mut hi = 1u64 << 50;
        let mut ans = 0u64;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.line_to_visual(mid) <= v {
                ans = mid;
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        ans
    }

    /// 总视觉行数（含超长行展开）。
    pub fn content_rows(&self, num_rows: u64) -> u64 {
        num_rows
            .saturating_mul(self.row_mult)
            .saturating_add(*self.prefix.last().unwrap_or(&0))
    }

    /// 行内字节偏移 → 该行内的视觉行偏移（估算；等宽字体下 ≈ 真实）。
    pub fn row_in_line_for_byte(&self, byte_in_line: u64) -> u64 {
        byte_in_line / self.bytes_per_row.max(1)
    }
}

/// 坐标总闸：组合 `VisualRowModel`（物理行 ↔ 视觉行）与 `HugeLayout`（超长行
/// 视觉行缓存）的统一换算入口。像素层（`CharMetrics::char_to_x` / `x_to_char`）
/// 由调用方按需组合。顶层（viewer / editor / app）要任何「物理行 ↔ 视觉行 ↔
/// 行内字符/列」换算都走这里，不再各自实现。
#[derive(Clone, Copy)]
pub struct ViewMapping<'a> {
    pub model: &'a VisualRowModel,
}

impl<'a> ViewMapping<'a> {
    pub fn new(model: &'a VisualRowModel) -> Self {
        Self { model }
    }

    /// 物理行 → 该行视觉行起点。
    #[inline]
    pub fn line_to_visual(&self, line: u64) -> u64 {
        self.model.line_to_visual(line)
    }

    /// 视觉行 → 物理行。
    #[inline]
    pub fn visual_to_line(&self, v: u64) -> u64 {
        self.model.visual_to_line(v)
    }

    /// 普通行行内字节偏移 → 行内视觉行偏移（估算；超长行请用 `char_to_row_col`）。
    #[inline]
    pub fn row_in_line_for_byte(&self, byte_in_line: u64) -> u64 {
        self.model.row_in_line_for_byte(byte_in_line)
    }

    /// 超长行内：行内字符索引 → `(行内视觉行偏移, 行内列)`。
    /// 精确（基于 `HugeLayout` 缓存的每行实际字符数，含 CJK 正确处理）。
    #[inline]
    pub fn char_to_row_col(&self, layout: &HugeLayout, char_idx: usize) -> (u64, usize) {
        let (row, col) = layout.row_of_char(char_idx);
        (row as u64, col)
    }

    /// 超长行内：`(行内视觉行偏移, 行内列)` → 行内字符索引。精确。
    #[inline]
    pub fn row_col_to_char(&self, layout: &HugeLayout, row_in_line: u64, col: usize) -> usize {
        layout.char_of_row_col(row_in_line as usize, col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_visual_rows_normal_and_huge() {
        assert_eq!(line_visual_rows(100, 100, 1), 1);
        assert_eq!(line_visual_rows(100, 100, 3), 3);
        assert_eq!(line_visual_rows(CHUNK_LINE_BYTES as u64, 100, 2), 2);
        let a = line_visual_rows(6_000_000, 100, 1);
        let b = line_visual_rows(6_000_000, 100, 3);
        assert!(a > 10_000);
        assert_eq!(a, b);
        assert_eq!(line_visual_rows(6_000_000, 147, 1), (6_000_000 + 146) / 147);
        assert!(line_visual_rows(CHUNK_LINE_BYTES as u64 + 1, 100, 2) > 2);
    }

    #[test]
    fn visual_row_model_maps_lines() {
        let blen = 70_000;
        let rows = line_visual_rows(blen, 100, 1);
        assert!(rows > 100);
        let m = VisualRowModel::build(10.0, 1000.0, 16.0, 1, &[(1, blen)]);
        assert_eq!(m.line_to_visual(0), 0);
        assert_eq!(m.line_to_visual(1), 1);
        assert_eq!(m.line_to_visual(2), 1 + rows);
        assert_eq!(m.visual_to_line(0), 0);
        assert_eq!(m.visual_to_line(50), 1);
        assert_eq!(m.visual_to_line(rows), 1);
        assert_eq!(m.visual_to_line(rows + 1), 2);
        assert_eq!(m.content_rows(3), 2 + rows);
        assert_eq!(m.row_in_line_for_byte(350), 3);
    }

    #[test]
    fn view_mapping_char_row_col_delegates() {
        let m = VisualRowModel::build(10.0, 1000.0, 16.0, 1, &[]);
        let vm = ViewMapping::new(&m);
        let l = HugeLayout::new();
        l.set_row(0, 0, 5);
        l.set_row(1, 5, 5);
        assert_eq!(vm.char_to_row_col(&l, 7), (1, 2));
        assert_eq!(vm.row_col_to_char(&l, 1, 2), 7);
        assert_eq!(vm.line_to_visual(3), 3); // 无超长行 → 视觉行 = 物理行
    }
}
