//! 超长行的视觉行布局缓存（`HugeLayout`）—— 水槽的隔板。
//!
//! 一条超长行会被连续换行切成多个视觉行。`HugeLayout` 记录每个视觉行的元数据
//! （字符起点 + 字符数），是「行内字符 ↔ 视觉行/行内列」换算的**唯一权威**。
//! 不存 galley —— 渲染按需 layout，这里只负责几何基准；`viewer` 在渲染/补齐时
//! 调用 `set_row` 记录，之后任何位置换算（光标、命中、选区、跳转）都走
//! `row_of_char` / `char_of_row_col`。

use std::cell::RefCell;

/// 超长行一个视觉行的元数据。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowMeta {
    /// 该视觉行在原始行内的字符起点。
    pub char_start: usize,
    /// 该视觉行包含的字符数。
    pub char_count: usize,
}

impl RowMeta {
    /// 该视觉行的结束字符位置（不含，exclusive）。
    #[inline]
    pub fn end(&self) -> usize {
        self.char_start + self.char_count
    }
}

/// 超长行按视觉行切分的缓存。
///
/// `rows[i]` 是第 `i` 个视觉行的元数据，`char_start` 从第 0 行起严格连续递增。
/// `RefCell` 让「渲染时补齐、命中时查询」在不可变借用下也能更新缓存。
#[derive(Clone, Debug, Default)]
pub struct HugeLayout {
    rows: RefCell<Vec<RowMeta>>,
}

impl HugeLayout {
    pub fn new() -> Self {
        Self::default()
    }

    /// 清空全部记录（wrap_w / 字号 / 文本内容变化时必须调用）。
    pub fn clear(&self) {
        self.rows.borrow_mut().clear();
    }

    /// 记录（或覆盖）第 `row` 个视觉行的元数据。
    /// `char_start` 必须从第 0 行起连续累积（第 0 行起点 = 0）。
    pub fn set_row(&self, row: usize, char_start: usize, char_count: usize) {
        let mut rows = self.rows.borrow_mut();
        if rows.len() <= row {
            rows.resize(row + 1, RowMeta::default());
        }
        rows[row] = RowMeta {
            char_start,
            char_count,
        };
    }

    /// 已记录的视觉行数。
    pub fn len(&self) -> usize {
        self.rows.borrow().len()
    }

    /// 第 `row` 个视觉行的元数据；未记录（或空行）返回 `None`。
    pub fn get(&self, row: usize) -> Option<RowMeta> {
        self.rows
            .borrow()
            .get(row)
            .copied()
            .filter(|m| m.char_count > 0)
    }

    /// 行内字符 `char_idx` → `(视觉行, 行内列)`。
    /// 对 `rows`（char_start 单调递增）二分；`char_idx` 超出已记录范围时返回
    /// 最后一个已记录行的末尾。
    pub fn row_of_char(&self, char_idx: usize) -> (usize, usize) {
        let rows = self.rows.borrow();
        if rows.is_empty() {
            return (0, 0);
        }
        let mut lo = 0usize;
        let mut hi = rows.len().saturating_sub(1); // inclusive 上界（配合 mid 偏上）
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            if rows[mid].char_start <= char_idx {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let r = rows[lo];
        // col 是光标列（0..=char_count），超出该行字符数时 clamp 到行尾。
        let col = char_idx.saturating_sub(r.char_start).min(r.char_count);
        (lo, col)
    }

    /// `(视觉行, 行内列)` → 行内字符索引。`col` 是光标列（0..=char_count，
    /// `col == char_count` 表示行尾 / 下一行起点），超出行尾时 clamp 到行尾。
    pub fn char_of_row_col(&self, row: usize, col: usize) -> usize {
        self.get(row).map_or(0, |m| m.char_start + col.min(m.char_count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_layout() -> HugeLayout {
        // 模拟 3 行：每行 5 字符
        let l = HugeLayout::new();
        l.set_row(0, 0, 5);
        l.set_row(1, 5, 5);
        l.set_row(2, 10, 5);
        l
    }

    #[test]
    fn row_of_char_maps() {
        let l = sample_layout();
        assert_eq!(l.row_of_char(0), (0, 0));
        assert_eq!(l.row_of_char(3), (0, 3));
        assert_eq!(l.row_of_char(5), (1, 0));
        assert_eq!(l.row_of_char(8), (1, 3));
        assert_eq!(l.row_of_char(10), (2, 0));
        assert_eq!(l.row_of_char(14), (2, 4));
        assert_eq!(l.row_of_char(99), (2, 5)); // 超出 → 最后一行行尾
    }

    #[test]
    fn char_of_row_col_maps() {
        let l = sample_layout();
        assert_eq!(l.char_of_row_col(0, 0), 0);
        assert_eq!(l.char_of_row_col(1, 2), 7);
        assert_eq!(l.char_of_row_col(2, 4), 14);
        assert_eq!(l.char_of_row_col(2, 5), 15);  // 行尾 = 下一行起点
        assert_eq!(l.char_of_row_col(2, 99), 15); // clamp 到行尾
        assert_eq!(l.char_of_row_col(9, 0), 0);   // 未记录行 → 0
    }

    #[test]
    fn set_row_grows_and_overwrites() {
        let l = HugeLayout::new();
        assert_eq!(l.len(), 0);
        l.set_row(0, 0, 3);
        l.set_row(1, 3, 3);
        assert_eq!(l.len(), 2);
        l.set_row(1, 3, 4); // 覆盖
        assert_eq!(l.get(1).unwrap().char_count, 4);
        l.clear();
        assert_eq!(l.len(), 0);
    }
}
