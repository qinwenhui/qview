//! 文本选择模型：anchor/focus 两点 + 逐行 UTF-8 字节区间。
//!
//! 选择以"逻辑行号 + 行内 UTF-8 字节偏移"描述。跨行选择在渲染/复制时按行展开。
//! 纯 Rust，无 AppKit 依赖，便于单元测试。

/// 一个光标点：第 `line` 行、UTF-8 字节偏移 `byte`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextPoint {
    pub line: u64,
    pub byte: usize,
}

impl PartialOrd for TextPoint {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TextPoint {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.line.cmp(&other.line).then(self.byte.cmp(&other.byte))
    }
}

/// 活动选择。`active=false` 表示无选区（anchor/focus 无意义）。
#[derive(Debug, Clone, Copy)]
pub struct Selection {
    pub anchor: TextPoint,
    pub focus: TextPoint,
    pub active: bool,
}

impl Selection {
    pub fn empty() -> Self {
        Self {
            anchor: TextPoint { line: 0, byte: 0 },
            focus: TextPoint { line: 0, byte: 0 },
            active: false,
        }
    }

    pub fn clear(&mut self) {
        self.active = false;
    }

    /// 是否有可见选区（active 且两端不同）。
    pub fn is_empty(&self) -> bool {
        !self.active || self.anchor == self.focus
    }

    /// 归一化区间起点（按 (line, byte) 序）。
    pub fn ordered(&self) -> (TextPoint, TextPoint) {
        let (a, f) = (self.anchor, self.focus);
        if a <= f {
            (a, f)
        } else {
            (f, a)
        }
    }

    /// 某行在选区内的 UTF-8 字节区间 `[s, e)`；该行不在选区内返回 `None`。
    ///
    /// 中间整行返回 `(0, line_len)`，首/尾行按 anchor/focus 的字节裁切。
    pub fn selected_range_for_line(&self, line: u64, line_len: usize) -> Option<(usize, usize)> {
        if !self.active {
            return None;
        }
        let (start, end) = self.ordered();
        if line < start.line || line > end.line {
            return None;
        }
        let s = if line == start.line { start.byte } else { 0 };
        let e = if line == end.line { end.byte } else { line_len };
        if s >= e {
            None
        } else {
            Some((s, e))
        }
    }

    /// 把选区文本拼成一个字符串（跨行以 `\n` 连接）。
    ///
    /// `read_line` 由调用方提供（bridge 读行），便于在纯 Rust 测试里喂假数据。
    pub fn copy_string(&self, read_line: impl Fn(u64) -> String) -> String {
        if !self.active {
            return String::new();
        }
        let (start, end) = self.ordered();
        let mut out = String::new();
        for line in start.line..=end.line {
            let text = read_line(line);
            let len = text.len();
            let s = if line == start.line {
                start.byte.min(len)
            } else {
                0
            };
            let e = if line == end.line {
                end.byte.min(len)
            } else {
                len
            };
            if e > s {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&text[s..e]);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(n: u64, byte: usize) -> TextPoint {
        TextPoint { line: n, byte }
    }

    #[test]
    fn ordering_by_line_then_byte() {
        assert!(line(1, 5) < line(2, 0));
        assert!(line(1, 2) < line(1, 5));
        assert_eq!(line(3, 1).cmp(&line(3, 1)), std::cmp::Ordering::Equal);
    }

    #[test]
    fn single_line_range() {
        let mut sel = Selection::empty();
        sel.anchor = line(2, 3);
        sel.focus = line(2, 7);
        sel.active = true;
        assert_eq!(sel.selected_range_for_line(2, 100), Some((3, 7)));
        assert_eq!(sel.selected_range_for_line(1, 100), None);
        assert_eq!(sel.selected_range_for_line(3, 100), None);
    }

    #[test]
    fn multi_line_range_expands_middle_rows() {
        let mut sel = Selection::empty();
        sel.anchor = line(1, 2);
        sel.focus = line(3, 4);
        sel.active = true;
        assert_eq!(sel.selected_range_for_line(1, 100), Some((2, 100)));
        assert_eq!(sel.selected_range_for_line(2, 50), Some((0, 50)));
        assert_eq!(sel.selected_range_for_line(3, 100), Some((0, 4)));
        assert_eq!(sel.selected_range_for_line(4, 100), None);
    }

    #[test]
    fn reversed_anchor_focus_same_range() {
        let mut sel = Selection::empty();
        sel.anchor = line(3, 8);
        sel.focus = line(1, 1);
        sel.active = true;
        assert_eq!(sel.selected_range_for_line(1, 100), Some((1, 100)));
        assert_eq!(sel.selected_range_for_line(3, 100), Some((0, 8)));
    }

    #[test]
    fn copy_string_joins_lines() {
        let fake = |l: u64| match l {
            0 => "hello".to_string(),
            1 => "world".to_string(),
            2 => "foo".to_string(),
            _ => String::new(),
        };
        let mut sel = Selection::empty();
        sel.anchor = line(0, 2);
        sel.focus = line(1, 3);
        sel.active = true;
        assert_eq!(sel.copy_string(fake), "llo\nwor");

        // 反转方向结果一致
        sel.anchor = line(1, 3);
        sel.focus = line(0, 2);
        assert_eq!(sel.copy_string(fake), "llo\nwor");
    }

    #[test]
    fn collapsed_selection_is_empty() {
        let mut sel = Selection::empty();
        sel.anchor = line(0, 4);
        sel.focus = line(0, 4);
        sel.active = true;
        assert!(sel.is_empty());
        assert_eq!(sel.copy_string(|_| "xxxxx".to_string()), "");
    }
}
