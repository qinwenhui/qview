//! 字符度量（`CharMetrics`）—— 格子系统的基础刻度。
//!
//! 格宽 = ASCII 字符宽，格高 = 行高，都由字号 + 字体决定，且**全浏览器唯一来源**。
//! 单个字符占多少格由 `cells(ch)` 决定（ASCII 1 格，CJK / emoji 等全宽字符 2 格）。
//! 这里同时封装「像素 ↔ 字符列」换算（基于 egui galley 的 glyph 位置，像素级精确），
//! 收敛 viewer 里散落的 `measure_char_width` / `text_pixel_width` / glyph 二分。

use egui::epaint::text::Row;

/// 等宽字体格子系统的基础刻度。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharMetrics {
    /// ASCII 字符宽（像素），即 1 个格子的宽。
    pub char_w: f32,
    /// 行高（像素），即 1 个格子的高。
    pub line_h: f32,
}

impl CharMetrics {
    pub fn new(char_w: f32, line_h: f32) -> Self {
        Self { char_w, line_h }
    }

    /// 单个字符占几个格子（ASCII 1，全宽字符 2）。
    pub fn cells(&self, ch: char) -> u32 {
        if is_wide_char(ch) {
            2
        } else {
            1
        }
    }

    /// 一行文本的像素宽度（按格子估算；等宽字体下 ≈ egui 真实排版）。
    pub fn text_w(&self, s: &str) -> f32 {
        s.chars().map(|c| self.cells(c) as f32).sum::<f32>() * self.char_w
    }

    /// 一行内字符列 → 像素 x（基于 glyph 位置，像素级精确）。
    /// `col` 是**行内**字符列（0-based，相对该视觉行起点）。
    pub fn char_to_x(&self, row: &Row, col: usize) -> f32 {
        if col >= row.glyphs.len() {
            row.rect.right()
        } else if col == 0 {
            row.rect.left()
        } else {
            row.glyphs[col].pos.x
        }
    }

    /// 像素 x → 一行内的字符列（二分 glyph 位置）。
    /// `x` 相对 galley 原点（渲染时 galley 画在 `content_x`，所以调用方传
    /// `点击x - content_x`）。返回「最后一个左边界 ≤ x 的字符索引」——即光标落在
    /// 该字符前（标准编辑器语义）。
    pub fn x_to_char(&self, row: &Row, x: f32) -> usize {
        last_glyph_le(&row.glyphs, x)
    }

    /// 一个「列宽 = char_w × cells 之和」的字符索引 → 像素 x（估算，无 galley 时用）。
    pub fn char_to_x_est(&self, s: &str, char_idx: usize) -> f32 {
        s.chars()
            .take(char_idx)
            .map(|c| self.cells(c) as f32)
            .sum::<f32>()
            * self.char_w
    }
}

/// 二分找「最后一个 `glyph.pos.x <= x`」的索引（即光标落在该字符前）。
/// 点击 ≥ 行尾（最后一个字符右边界）→ 返回全部字符数（光标在行尾）。
/// 热路径：直接二分 glyphs，零分配。
fn last_glyph_le(glyphs: &[egui::epaint::text::Glyph], x: f32) -> usize {
    // 行尾：x 已越过最后一个字符右边界 → 光标在所有字符后
    if let Some(last) = glyphs.last() {
        if x >= last.pos.x + last.advance_width {
            return glyphs.len();
        }
    }
    let mut lo = 0usize;
    let mut hi = glyphs.len();
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        if mid < glyphs.len() && glyphs[mid].pos.x <= x {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// 纯二分核心：最后一个 `positions[i] <= x` 的索引。`x` 已相对行左偏移。
/// 抽出便于单元测试（不依赖 egui 类型）；`glyphs` 位置单调递增即等价。
fn last_index_le(positions: &[f32], x: f32) -> usize {
    let mut lo = 0usize;
    let mut hi = positions.len();
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        if mid < positions.len() && positions[mid] <= x {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// 判断字符是否「全宽」（东亚宽字符 / emoji，占 2 格）。
///
/// 等宽字体（qview 主字体）下全宽字符恰好占 2 个 ASCII 格宽。这里是度量模块的
/// 唯一封装点 —— 将来想用 `unicode-width` crate 精确化，只改这一个函数。
fn is_wide_char(c: char) -> bool {
    // 常用全宽码段（East Asian Width = W/F 的近似覆盖），够 qview 的日志场景用。
    matches!(c,
        '\u{1100}'..='\u{115F}'      // Hangul Jamo
        | '\u{2E80}'..='\u{303E}'    // CJK Radicals … CJK Symbols
        | '\u{3041}'..='\u{33FF}'    // Hiragana … CJK Compatibility
        | '\u{3400}'..='\u{4DBF}'    // CJK Ext A
        | '\u{4E00}'..='\u{9FFF}'    // CJK Unified
        | '\u{A000}'..='\u{A4CF}'    // Yi
        | '\u{AC00}'..='\u{D7A3}'    // Hangul Syllables
        | '\u{F900}'..='\u{FAFF}'    // CJK Compatibility Ideographs
        | '\u{FE30}'..='\u{FE4F}'    // CJK Compatibility Forms
        | '\u{FF00}'..='\u{FF60}'    // Fullwidth Forms
        | '\u{FFE0}'..='\u{FFE6}'    // Fullwidth Signs
        | '\u{1F300}'..='\u{1FAFF}'  // Emoji
        | '\u{20000}'..='\u{2FA1F}'  // CJK Ext B–F
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_ascii_vs_wide() {
        let m = CharMetrics::new(8.0, 16.0);
        assert_eq!(m.cells('a'), 1);
        assert_eq!(m.cells('1'), 1);
        assert_eq!(m.cells(' '), 1);
        assert_eq!(m.cells('你'), 2);
        assert_eq!(m.cells('🙂'), 2);
    }

    #[test]
    fn text_w_sums_cells() {
        let m = CharMetrics::new(8.0, 16.0);
        assert_eq!(m.text_w("ab"), 16.0);
        assert_eq!(m.text_w("你好"), 32.0);
        assert_eq!(m.text_w("a你b"), 32.0); // 1 + 2 + 1 格 × 8px
    }

    /// 用真实 egui 字体排版一行文本，返回 galley（验证 glyph 数组与字符索引关系）。
    fn layout_row_of(text: &str) -> std::sync::Arc<egui::Galley> {
        let fonts = egui::epaint::text::Fonts::new(1.0, 1024, egui::FontDefinitions::default());
        let font = egui::FontId::monospace(16.0);
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = f32::INFINITY;
        job.append(
            text,
            0.0,
            egui::text::TextFormat {
                font_id: font,
                ..Default::default()
            },
        );
        fonts.layout_job(job)
    }

    #[test]
    fn glyph_index_matches_char_index_plain() {
        let g = layout_row_of("abcdefghijklmnopqrstuvwxyz");
        let row = &g.rows[0];
        // 普通 ASCII：glyph 数组索引 == 字符索引（每字符一个 glyph）
        assert_eq!(row.glyphs.len(), 26);
        for (i, glyph) in row.glyphs.iter().enumerate() {
            assert_eq!(glyph.chr, "abcdefghijklmnopqrstuvwxyz".chars().nth(i).unwrap());
        }
    }

    #[test]
    fn glyph_index_vs_char_index_with_tab() {
        let g = layout_row_of("\t1234567890");
        let row = &g.rows[0];
        // 打印 glyph 数组（chr + pos.x），确认 tab 是否产生额外/缺失 glyph
        for (i, glyph) in row.glyphs.iter().enumerate() {
            eprintln!("glyph[{i}]: chr={:?} pos.x={}", glyph.chr, glyph.pos.x);
        }
        let chars: Vec<char> = "\t1234567890".chars().collect();
        assert_eq!(row.glyphs.len(), chars.len(), "tab 行 glyph 数应 = 字符数");
        for (i, glyph) in row.glyphs.iter().enumerate() {
            assert_eq!(glyph.chr, chars[i]);
        }
    }

    /// 模拟 viewer 的 per-row 逐行累积：从行开头逐视觉行 layout（max_rows=1），
    /// 每行取 glyphs.len() 推进字符位置。验证含 CJK 时累积无偏（不丢/不多算）。
    #[test]
    fn huge_row_accumulation_is_exact_with_cjk() {
        let fonts = egui::epaint::text::Fonts::new(1.0, 1024, egui::FontDefinitions::default());
        let font = egui::FontId::monospace(16.0);
        // 含 CJK 的文本（每 7 字符一个 '你'），模拟超长行内容
        let text: String = (0..500)
            .map(|i| if i % 7 == 0 { '你' } else { 'a' })
            .collect();
        let char_w = {
            let mut job = egui::text::LayoutJob::default();
            job.append(
                "a",
                0.0,
                egui::text::TextFormat {
                    font_id: font.clone(),
                    ..Default::default()
                },
            );
            fonts.layout_job(job).rows[0].glyphs[0].pos.x
        };
        // 每行 ≈ 40 个 ASCII 格宽（视口宽度）
        let wrap_w = 40.0 * char_w;

        // 从行开头逐行 layout 累积字符位置（等价 viewer 的 char_pos 累积）
        let mut char_pos = 0usize;
        let total = text.chars().count();
        let mut rows = Vec::new();
        while char_pos < total {
            // 字符索引 → 字节切片（正确处理 CJK）
            let bp = text.char_indices().nth(char_pos).map(|(b, _)| b).unwrap_or(text.len());
            let remaining = &text[bp..];
            let mut job = egui::text::LayoutJob::default();
            job.wrap.max_width = wrap_w;
            job.wrap.break_anywhere = true;
            job.wrap.max_rows = 1;
            job.wrap.overflow_character = None;
            job.append(
                remaining,
                0.0,
                egui::text::TextFormat {
                    font_id: font.clone(),
                    ..Default::default()
                },
            );
            let g = fonts.layout_job(job);
            let rc = g.rows.first().map(|r| r.glyphs.len()).unwrap_or(0);
            if rc == 0 {
                break;
            }
            rows.push((char_pos, rc));
            char_pos += rc;
        }
        // 累积必须恰好到达行尾（无丢失 / 无多算）
        assert_eq!(char_pos, total, "逐行累积应恰好覆盖整行: {rows:?}");
        // 每行字符数 > 0
        assert!(rows.len() > 5);
    }

    /// 端到端：超长行逐视觉行渲染，点击每个字符边界 → col 必须精确。
    /// 模拟 viewer 的 per-row layout + hit-test（chunk_char + x_to_char）。
    #[test]
    fn huge_hit_test_col_is_exact_at_char_boundaries() {
        let fonts = egui::epaint::text::Fonts::new(1.0, 1024, egui::FontDefinitions::default());
        let font = egui::FontId::monospace(16.0);
        // 含 CJK（每 5 字符一个 '你'）的超长行，ASCII 前缀对齐字符边界
        let text: String = (0..300)
            .map(|i| if i % 5 == 2 { '你' } else { 'a' })
            .collect();
        let char_w = {
            let mut j = egui::text::LayoutJob::default();
            j.append("a", 0.0, egui::text::TextFormat { font_id: font.clone(), ..Default::default() });
            fonts.layout_job(j).rows[0].glyphs[0].advance_width
        };
        let wrap_w = 40.0 * char_w;

        let layout_one = |s: &str| -> (std::sync::Arc<egui::Galley>, usize) {
            let mut job = egui::text::LayoutJob::default();
            job.wrap.max_width = wrap_w;
            job.wrap.break_anywhere = true;
            job.wrap.max_rows = 1;
            job.wrap.overflow_character = None;
            job.append(s, 0.0, egui::text::TextFormat { font_id: font.clone(), ..Default::default() });
            let g = fonts.layout_job(job);
            let n = g.rows.first().map(|r| r.glyphs.len()).unwrap_or(0);
            (g, n)
        };

        // 逐视觉行渲染，从 0 精确累积 chunk_char
        let mut char_pos = 0usize;
        let mut rows: Vec<(usize, usize)> = Vec::new(); // (chunk_char, char_count)
        while char_pos < text.chars().count() {
            let bp = text.char_indices().nth(char_pos).map(|(b, _)| b).unwrap_or(text.len());
            let (_, n) = layout_one(&text[bp..]);
            if n == 0 { break; }
            rows.push((char_pos, n));
            char_pos += n;
        }
        assert_eq!(char_pos, text.chars().count(), "累积应精确覆盖整行");

        // 对每个视觉行：点击第 k 个字符边界，col 必须 = chunk_char + k
        for (chunk_char, n) in &rows {
            let bp = text.char_indices().nth(*chunk_char).map(|(b, _)| b).unwrap_or(text.len());
            let (g, _) = layout_one(&text[bp..]);
            let row = &g.rows[0];
            if *chunk_char == 0 {
                for (i, gl) in row.glyphs.iter().take(6).enumerate() {
                    eprintln!("row0 glyph[{i}]: chr={:?} pos.x={}", gl.chr, gl.pos.x);
                }
                eprintln!("row0 char_count={} glyph_len={}", n, row.glyphs.len());
            }
            for k in 0..=*n {
                let x = if k < row.glyphs.len() {
                    row.glyphs[k].pos.x // 字符 k 左边界
                } else {
                    let last = row.glyphs.last().unwrap(); // 行尾
                    last.pos.x + last.advance_width
                };
                let lo = last_glyph_le(&row.glyphs, x);
                let col = chunk_char + lo;
                assert_eq!(col, chunk_char + k,
                    "点击第 {k} 字符边界应得到 col = chunk_char+{k}（chunk_char={chunk_char}）");
            }
        }
    }

    /// 用**实际字体**（NotoSansSC-VF，程序主字体）验证：每行 glyph 数 == 字符数。
    /// 若某字符（CJK/连字/破折号）渲染多个 glyph，glyph 索引 ≠ 字符索引 →
    /// char_pos 累积偏大 → chunk_char / col 偏大 1（用户实测插入偏移 ab1cd）。
    #[test]
    fn noto_font_glyph_count_matches_chars() {
        let font_path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/NotoSansSC-VF.ttf");
        let data = std::fs::read(font_path).expect("读取 NotoSansSC-VF.ttf");
        let mut defs = egui::FontDefinitions::default();
        defs.font_data
            .insert("noto".to_owned(), egui::FontData::from_owned(data).into());
        defs.families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .insert(0, "noto".to_owned());
        let fonts = egui::epaint::text::Fonts::new(1.0, 4096, defs);
        let font = egui::FontId::monospace(16.0);

        // 覆盖用户日志出现的片段 + 常见 CJK/符号 + **tab / 混合（用户实测高亮偏移的行）**
        let samples = [
            "CAST(CAST(greatest(COALESCE(",
            "你a", "a你b", "—", "——", "a—b", "1", "ab", "·", "…",
            "\tDatabase JDBC URL [Connecting",
            "[]wEx日志系统初始化完成 (支持本地日志和Kafka推送)",
            "jobName",
            "\t1234567890",
        ];
        for s in samples {
            let mut job = egui::text::LayoutJob::default();
            job.wrap.max_width = f32::INFINITY;
            job.append(s, 0.0, egui::text::TextFormat { font_id: font.clone(), ..Default::default() });
            let g = fonts.layout_job(job);
            let n_chars = s.chars().count();
            let n_glyphs: usize = g.rows.iter().map(|r| r.glyphs.len()).sum();
            assert_eq!(n_glyphs, n_chars,
                "字体 NotoSansSC-VF 渲染 '{s}'：glyph 数 {n_glyphs} != 字符数 {n_chars}（会导致累积偏移）");
            // 关键：glyph 数组索引必须 == 字符索引（char_to_x / x_to_char 依赖）
            if let Some(row) = g.rows.first() {
                for (i, glyph) in row.glyphs.iter().enumerate() {
                    let expected = s.chars().nth(i);
                    assert_eq!(Some(glyph.chr), expected,
                        "'{s}'：glyph[{i}].chr={:?} != 第{i}字符 {expected:?}（索引错位 → 高亮/选区偏）", glyph.chr);
                }
            }
        }
    }

    /// 打印 NotoSansSC-VF 对 tab / 混合行的 glyph 位置（pos.x），检查是否有
    /// 索引与位置错位（用户实测：tab/中文行高亮偏 1）。
    #[test]
    fn noto_font_tab_glyph_positions() {
        let font_path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/NotoSansSC-VF.ttf");
        let data = std::fs::read(font_path).expect("读取 NotoSansSC-VF.ttf");
        let mut defs = egui::FontDefinitions::default();
        defs.font_data.insert("noto".to_owned(), egui::FontData::from_owned(data).into());
        defs.families.entry(egui::FontFamily::Monospace).or_default().insert(0, "noto".to_owned());
        let fonts = egui::epaint::text::Fonts::new(1.0, 4096, defs);
        let font = egui::FontId::monospace(16.0);

        let s = "\tDatabase JDBC URL [Connecting";
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = f32::INFINITY;
        job.append(s, 0.0, egui::text::TextFormat { font_id: font.clone(), ..Default::default() });
        let g = fonts.layout_job(job);
        let row = &g.rows[0];
        eprintln!("=== NotoSansSC-VF: '{s}' (chars={}) glyphs={} ===", s.chars().count(), row.glyphs.len());
        for (i, gl) in row.glyphs.iter().take(16).enumerate() {
            eprintln!("glyph[{i}]: chr={:?} pos.x={:.1}", gl.chr, gl.pos.x);
        }
        // 索引必须一一对应（否则高亮/光标错位）
        assert_eq!(row.glyphs.len(), s.chars().count());
        for (i, gl) in row.glyphs.iter().enumerate() {
            assert_eq!(gl.chr, s.chars().nth(i).unwrap());
        }
    }

    /// 关键：**行内含 \n** 的"超长行"。per-row 用 max_rows=1 时，egui 在 \n 处
    /// 换行 → rows[0].glyphs 只到 \n 前 → 字符数少算 → 累积偏 → chunk_char 偏。
    /// 用户实测：frag 含 `\n`（如 `assess_fee`,\nCAST(CAST('），位置错乱。
    #[test]
    fn newline_inside_huge_line_breaks_accumulation() {
        let fonts = egui::epaint::text::Fonts::new(1.0, 1024, egui::FontDefinitions::default());
        let font = egui::FontId::monospace(16.0);
        // 逻辑一行，行内含多个 \n（如格式化 JSON）
        let text: String = (0..50).map(|_| "ab\ncd").collect::<String>(); // 200 字符，25 个 \n
        let char_w = {
            let mut j = egui::text::LayoutJob::default();
            j.append("a", 0.0, egui::text::TextFormat { font_id: font.clone(), ..Default::default() });
            fonts.layout_job(j).rows[0].glyphs[0].advance_width
        };
        let wrap_w = 40.0 * char_w;
        let total = text.chars().count();
        let bp_of = |cp: usize| text.char_indices().nth(cp).map(|(b, _)| b).unwrap_or(text.len());

        // 模拟 viewer 的 per-row 累积（修复后：切片截断到 \n，推进跳过 \n）
        let mut char_pos = 0usize;
        let mut rows = Vec::new();
        while char_pos < total {
            let bp = bp_of(char_pos);
            let slice = &text[bp..];
            // 行内 \n = 视觉换行：截断到 \n 前（否则 max_rows=1 在 \n 换行 → 少算）
            let (row_text, had_nl) = match slice.find('\n') {
                Some(nl) => (&slice[..nl], true),
                None => (slice, false),
            };
            let mut job = egui::text::LayoutJob::default();
            job.wrap.max_width = wrap_w;
            job.wrap.break_anywhere = true;
            job.wrap.max_rows = 1;
            job.wrap.overflow_character = None;
            job.append(row_text, 0.0, egui::text::TextFormat { font_id: font.clone(), ..Default::default() });
            let g = fonts.layout_job(job);
            let n = g.rows.first().map(|r| r.glyphs.len()).unwrap_or(0);
            if n == 0 && !had_nl { break; }
            rows.push((char_pos, n));
            char_pos += n + if had_nl { 1 } else { 0 }; // 跳过 \n
        }
        eprintln!("含\\n 行（修复后）：total_chars={total} 累积到 char_pos={char_pos} rows={rows:?}");
        // 修复后：含 \n 累积必须恰好覆盖整行
        assert_eq!(char_pos, total,
            "含 \\n 时累积必须恰好覆盖整行；否则 chunk_char 偏 → 插入/高亮/选区全偏");
    }

    #[test]
    fn last_index_le_binary() {
        // 每字符 8px 的等宽位置
        let pos: Vec<f32> = (0..4).map(|i| 8.0 * i as f32).collect();
        assert_eq!(last_index_le(&pos, -1.0), 0);
        assert_eq!(last_index_le(&pos, 0.0), 0);   // 字符 0 左边界 → 0 前
        assert_eq!(last_index_le(&pos, 4.0), 0);   // 字符 0 内
        assert_eq!(last_index_le(&pos, 8.0), 1);   // 字符 1 左边界
        assert_eq!(last_index_le(&pos, 12.0), 1);
        assert_eq!(last_index_le(&pos, 20.0), 2);
        assert_eq!(last_index_le(&pos, 31.0), 3);
        assert_eq!(last_index_le(&pos, 99.0), 3);  // 行尾之后
    }
}
