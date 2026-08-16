//! Character-level editing for the GUI, mapped onto the engine's line-based
//! edit ops. Runs only in edit mode, when no TextEdit has keyboard focus.
//!
//! Every user action is applied through `Engine` line ops, wrapped in an edit
//! batch where it touches multiple lines (split / join / paste / selection), so
//! Ctrl+Z/Ctrl+Y undo/redo whole actions. The engine coalesces consecutive
//! same-line replaces, so a typing burst is one undo step too.

use egui::{Context, Event, ImeEvent, Key};

use crate::app::QLogApp;
use crate::layout::ViewMapping;

/// Dispatch keyboard/IME input to the editor. Caller gates on edit mode.
pub fn handle_edit_keys(ctx: &Context, app: &mut QLogApp) {
    // Never steal keys while a TextEdit (search / goto / settings) is focused.
    if ctx.wants_keyboard_input() {
        return;
    }
    // Nor while any modal dialog is open (a confirm box has no focused widget,
    // so its Enter/arrows must not reach the editor).
    if app.pending_discard.is_some()
        || app.show_annotation_dialog
        || app.show_annotation_list
        || app.show_settings
        || app.show_about
        || app.show_help
        || app.show_shortcuts
        || app.show_donate
        || app.show_file_properties
        || app.show_index_manager
        || app.show_encoding_confirm
    {
        return;
    }
    let events = ctx.input(|i| i.events.clone());
    for ev in events {
        match ev {
            Event::Text(s) => {
                let ctrl = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
                if !ctrl && !s.chars().all(|c| c.is_control()) {
                    app.editor_insert_text(&s);
                }
            }
            // 中文等非 ASCII 输入：macOS 输入法组合只通过 Event::Ime 送达。
            // Preedit 存为标记文本由 viewer 绘制下划线，Commit 才真正插入。
            // （启用了输入法后，组合期间的击键不会再产生 Event::Text，不会重复插入。）
            Event::Ime(ime) => {
                match ime {
                    ImeEvent::Commit(text) => {
                        if !text.is_empty() {
                            app.editor_insert_text(&text);
                        }
                        app.edit_ime_preedit.clear();
                    }
                    ImeEvent::Preedit(text) => {
                        app.edit_ime_preedit = text;
                    }
                    ImeEvent::Enabled | ImeEvent::Disabled => {
                        app.edit_ime_preedit.clear();
                    }
                }
            }
            Event::Paste(s) => {
                app.editor_paste(&s);
            }
            Event::Key { key, pressed, .. } if pressed => {
                let modifiers = ctx.input(|i| i.modifiers);
                let ctrl = modifiers.ctrl || modifiers.command;
                let shift = modifiers.shift;
                match key {
                    Key::ArrowLeft if !ctrl => app.editor_move_col(-1),
                    Key::ArrowRight if !ctrl => app.editor_move_col(1),
                    Key::ArrowUp => app.editor_move_row(-1),
                    Key::ArrowDown => app.editor_move_row(1),
                    Key::Home => app.editor_home(),
                    Key::End => app.editor_end(),
                    Key::Backspace => app.editor_backspace(),
                    Key::Delete => app.editor_delete(),
                    Key::Enter => app.editor_enter(),
                    Key::Tab if !ctrl => app.editor_insert_text("    "),
                    Key::Z if ctrl && !shift => app.editor_undo(),
                    Key::Y if ctrl => app.editor_redo(),
                    Key::V if ctrl => {}
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

impl QLogApp {
    /// 编辑器修改了行 → **立即清空**超长行缓存（下帧渲染用 `read_line` 当前文本
    /// 重建）。否则渲染/点击用的 `cache.text` 是旧快照，与编辑器当前文本错位 →
    /// 高亮/选中/复制/插入全偏 1 字符（用户实测：version_id 高亮对但复制少 'v'）。
    /// 必须**立即清**（而非仅标记让 viewer 下一帧清）——编辑后同帧/下帧的选中
    /// 就会用到旧缓存。
    fn mark_huge_cache_dirty(&mut self) {
        self.huge_chunk_cache.clear();
        self.huge_cache_dirty.set(true);
    }

    /// Current line's decoded text with the trailing newline stripped.
    fn edit_line_text(&self, line: u64) -> String {
        self.engine
            .as_ref()
            .map(|arc| {
                let e = arc.lock();
                e.read_line(line)
                    .text
                    .trim_end_matches('\n')
                    .trim_end_matches('\r')
                    .to_string()
            })
            .unwrap_or_default()
    }

    fn edit_line_len(&self, line: u64) -> usize {
        self.edit_line_text(line).chars().count()
    }

    fn edit_total_lines(&self) -> u64 {
        self.engine
            .as_ref()
            .map(|arc| arc.lock().effective_line_count())
            .unwrap_or(0)
    }

    /// Normalized selection range, `(start, end)` where start <= end.
    fn edit_sel_range(&self) -> Option<((u64, usize), (u64, usize))> {
        let (l1, c1, l2, c2) = self.selection?;
        let a = (l1, c1);
        let b = (l2, c2);
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    fn is_sel_nonempty(&self) -> bool {
        match self.edit_sel_range() {
            Some(((l1, c1), (l2, c2))) => (l1, c1) != (l2, c2),
            None => false,
        }
    }

    /// Run `f` inside an engine edit batch (one undo/redo step).
    fn with_edit_batch(&mut self, f: impl FnOnce(&mut QLogApp)) {
        self.with_edit_batch_and_get(|app| {
            f(app);
        });
    }

    /// Like `with_edit_batch`, returning `f`'s result.
    fn with_edit_batch_and_get<T>(&mut self, f: impl FnOnce(&mut QLogApp) -> T) -> T {
        if let Some(arc) = self.engine.as_mut() {
            arc.lock().begin_edit_batch();
        }
        let r = f(self);
        if let Some(arc) = self.engine.as_mut() {
            arc.lock().end_edit_batch();
        }
        // 多行编辑（join/split/delete 选区等）可能改行数 → 行号偏移，清全部缓存
        self.mark_huge_cache_dirty();
        r
    }

    /// Clamp the caret into the file, then keep it on-screen.
    pub fn editor_set_cursor(&mut self, line: u64, col: usize) {
        let total = self.edit_total_lines();
        let line = if total == 0 { 0 } else { line.min(total - 1) };
        let col = col.min(self.edit_line_len(line));
        self.edit_cursor = Some((line, col));
        // Auto-scroll so the caret stays visible.
        // 关键：物理行号 ≠ 视觉行号。超长行 wrap 成几千个视觉行，旧逻辑用物理行
        // 号直接跟 scroll_y/row_h（视觉行）比较 → 编辑超长行时 `line < first` 误触发，
        // scroll_y = line * row_h 跳到错误位置（用户反馈「输入后主视图跳到别处、
        // 找不到光标」）。改用视觉行模型：caret 物理行 → 视觉起点 + 行内列估算。
        if let Some(m) = &self.visual_model {
            let vm = ViewMapping::new(m);
            // 超长行：用 HugeLayout 精确求行内视觉行偏移（含 CJK 每行实际字符数）；
            // 普通行：按 bytes_per_row 估算。
            let row_in = if let Some(cache) = self.huge_chunk_cache.iter().find(|c| c.line == line)
            {
                vm.char_to_row_col(&cache.layout, col).0
            } else {
                col as u64 / m.bytes_per_row.max(1)
            };
            let caret_v = vm.line_to_visual(line) + row_in;
            let first_v = (self.scroll_y / m.row_h as f64).floor() as u64;
            const VIS_ROWS: u64 = 80; // 估算视口视觉行数（与 jump_hit 一致）
            if caret_v < first_v {
                self.scroll_y = caret_v as f64 * m.row_h as f64;
            } else if caret_v > first_v.saturating_add(VIS_ROWS.saturating_sub(2)) {
                // 滚到 caret 下方，让 caret 落在视口中下部（留上下文）
                self.scroll_y = (caret_v.saturating_sub(20)) as f64 * m.row_h as f64;
            }
        } else {
            let effective_row_h = if self.word_wrap {
                self.row_h * self.wrap_height_mult
            } else {
                self.row_h
            };
            let first = (self.scroll_y / effective_row_h).floor() as u64;
            if line < first {
                self.scroll_y = line as f64 * effective_row_h;
            } else if line > first.saturating_add(60) {
                self.scroll_y = line.saturating_sub(20) as f64 * effective_row_h;
            }
        }
    }

    /// Delete the selection (raw engine ops, NO batch). Caller wraps in a batch
    /// when it touches multiple lines. Returns the caret where the gap now is.
    fn delete_selection_into_caret(&mut self) -> Option<(u64, usize)> {
        let ((sl, sc), (el, ec)) = self.edit_sel_range()?;
        if sl == el {
            let line = self.edit_line_text(sl);
            let bytes = |c: usize| crate::app::char_col_to_byte(&line, c);
            let new = format!("{}{}", &line[..bytes(sc)], &line[bytes(ec)..]);
            let mut engine = self.engine.as_mut()?.lock();
            engine.replace_logical_line(sl, new.into_bytes());
            drop(engine);
            self.mark_huge_cache_dirty();
            self.selection = None;
            Some((sl, sc))
        } else {
            let first = self.edit_line_text(sl);
            let last = self.edit_line_text(el);
            let prefix = &first[..crate::app::char_col_to_byte(&first, sc)];
            let suffix = &last[crate::app::char_col_to_byte(&last, ec)..];
            let mut engine = self.engine.as_mut()?.lock();
            // Delete middle lines bottom-up so line numbers stay valid.
            for ln in (sl + 1..=el).rev() {
                engine.delete_logical_line(ln);
            }
            let new = format!("{prefix}{suffix}");
            engine.replace_logical_line(sl, new.into_bytes());
            self.selection = None;
            Some((sl, sc))
        }
    }

    /// Insert `text` at the caret (replacing any selection). Text may span lines.
    pub fn editor_insert_text(&mut self, text: &str) {
        if text.is_empty() || self.engine.is_none() {
            return;
        }
        let sel_span_lines = match self.edit_sel_range() {
            Some(((l1, _), (l2, _))) => l1 != l2,
            None => false,
        };
        let (line, col) = if self.is_sel_nonempty() {
            if sel_span_lines {
                // Multi-line selection + insert = one undo step.
                let pos = self.with_edit_batch_and_get(|app| app.delete_selection_into_caret());
                match pos {
                    Some(p) => p,
                    None => return,
                }
            } else {
                // Single-line selection: the delete + insert replaces coalesce.
                match self.delete_selection_into_caret() {
                    Some(p) => p,
                    None => return,
                }
            }
        } else {
            match self.edit_cursor {
                Some(pos) => pos,
                None => return,
            }
        };
        if !text.contains('\n') {
            let cur = self.edit_line_text(line);
            let b = crate::app::char_col_to_byte(&cur, col);
            let new = format!("{}{}{}", &cur[..b], text, &cur[b..]);
            if let Some(arc) = self.engine.as_mut() {
                arc.lock().replace_logical_line(line, new.into_bytes());
            }
            self.mark_huge_cache_dirty();
            self.editor_set_cursor(line, col + text.chars().count());
        } else {
            // Multi-line insert (paste-like).
            let parts: Vec<&str> = text.split('\n').collect();
            self.with_edit_batch(|app| {
                let cur = app.edit_line_text(line);
                let b = crate::app::char_col_to_byte(&cur, col);
                let mut after = line;
                {
                    let mut engine = app.engine.as_mut().unwrap().lock();
                    engine.replace_logical_line(
                        line,
                        format!("{}{}", &cur[..b], parts[0]).into_bytes(),
                    );
                    for part in parts.iter().skip(1) {
                        engine.insert_logical_line_after(after, part.as_bytes().to_vec());
                        after += 1;
                    }
                }
            });
            let last = parts.last().map(|s| s.chars().count()).unwrap_or(0);
            self.editor_set_cursor(line + parts.len() as u64 - 1, last);
        }
    }

    /// Paste clipboard text at the caret (replaces selection).
    pub fn editor_paste(&mut self, text: &str) {
        self.editor_insert_text(text);
    }

    pub fn editor_backspace(&mut self) {
        if self.is_sel_nonempty() {
            let pos = self.with_edit_batch_and_get(|app| app.delete_selection_into_caret());
            if let Some(p) = pos {
                self.editor_set_cursor(p.0, p.1);
            }
            return;
        }
        let (line, col) = match self.edit_cursor {
            Some(p) => p,
            None => return,
        };
        if col > 0 {
            let cur = self.edit_line_text(line);
            // 删除光标前**整个字符**：前一字符的起始字节 → 光标处字节。
            // 旧代码 `&cur[..b - 1]` 只删 1 字节，多字节字符（如 '—' 3 字节）时
            // `b-1` 落在字符中间 → panic（用户实测：删中文破折号闪退）。
            let b = crate::app::char_col_to_byte(&cur, col);
            let prev_b = crate::app::char_col_to_byte(&cur, col - 1);
            let new = format!("{}{}", &cur[..prev_b], &cur[b..]);
            if let Some(arc) = self.engine.as_mut() {
                arc.lock().replace_logical_line(line, new.into_bytes());
            }
            self.mark_huge_cache_dirty();
            self.editor_set_cursor(line, col - 1);
        } else if line > 0 {
            self.with_edit_batch(|app| {
                let cur = app.edit_line_text(line);
                let prev = app.edit_line_text(line - 1);
                {
                    let mut engine = app.engine.as_mut().unwrap().lock();
                    engine.replace_logical_line(line - 1, format!("{prev}{cur}").into_bytes());
                    engine.delete_logical_line(line);
                }
            });
            let caret = self.edit_line_len(line - 1);
            self.editor_set_cursor(line - 1, caret);
        }
    }

    pub fn editor_delete(&mut self) {
        if self.is_sel_nonempty() {
            let pos = self.with_edit_batch_and_get(|app| app.delete_selection_into_caret());
            if let Some(p) = pos {
                self.editor_set_cursor(p.0, p.1);
            }
            return;
        }
        let (line, col) = match self.edit_cursor {
            Some(p) => p,
            None => return,
        };
        let len = self.edit_line_len(line);
        if col < len {
            let cur = self.edit_line_text(line);
            let b = crate::app::char_col_to_byte(&cur, col);
            let e = crate::app::char_col_to_byte(&cur, col + 1);
            let new = format!("{}{}", &cur[..b], &cur[e..]);
            if let Some(arc) = self.engine.as_mut() {
                arc.lock().replace_logical_line(line, new.into_bytes());
            }
            self.mark_huge_cache_dirty();
            self.editor_set_cursor(line, col);
        } else if line + 1 < self.edit_total_lines() {
            self.with_edit_batch(|app| {
                let cur = app.edit_line_text(line);
                let next = app.edit_line_text(line + 1);
                {
                    let mut engine = app.engine.as_mut().unwrap().lock();
                    engine.replace_logical_line(line, format!("{cur}{next}").into_bytes());
                    engine.delete_logical_line(line + 1);
                }
            });
            self.editor_set_cursor(line, col);
        }
    }

    pub fn editor_enter(&mut self) {
        // Enter collapses the selection (replacing it), then splits — all in
        // ONE undo step.
        let had_sel = self.is_sel_nonempty();
        let cursor = self.edit_cursor;
        if !had_sel && cursor.is_none() {
            return;
        }
        self.with_edit_batch(|app| {
            let (line, col) = if had_sel {
                app.delete_selection_into_caret().unwrap_or((0, 0))
            } else {
                cursor.unwrap()
            };
            let cur = app.edit_line_text(line);
            let b = crate::app::char_col_to_byte(&cur, col);
            {
                let mut engine = app.engine.as_mut().unwrap().lock();
                engine.replace_logical_line(line, cur[..b].as_bytes().to_vec());
                engine.insert_logical_line_after(line, cur[b..].as_bytes().to_vec());
            }
            app.editor_set_cursor(line + 1, 0);
        });
    }

    pub fn editor_move_col(&mut self, delta: i64) {
        if let Some((line, col)) = self.edit_cursor {
            let len = self.edit_line_len(line);
            let ncol = if delta < 0 {
                col.saturating_sub(1)
            } else {
                (col + 1).min(len)
            };
            self.editor_set_cursor(line, ncol);
        }
    }

    pub fn editor_move_row(&mut self, delta: i64) {
        if let Some((line, col)) = self.edit_cursor {
            let total = self.edit_total_lines();
            let nline = if delta < 0 {
                line.saturating_sub(1)
            } else {
                (line + 1).min(total.saturating_sub(1))
            };
            self.editor_set_cursor(nline, col);
        }
    }

    pub fn editor_home(&mut self) {
        if let Some((line, _)) = self.edit_cursor {
            self.editor_set_cursor(line, 0);
        }
    }

    pub fn editor_end(&mut self) {
        if let Some((line, _)) = self.edit_cursor {
            self.editor_set_cursor(line, self.edit_line_len(line));
        }
    }

    pub fn editor_undo(&mut self) {
        let did_undo = if let Some(arc) = self.engine.as_mut() {
            arc.lock().undo_one()
        } else {
            false
        };
        if did_undo {
            // 撤消修改行 → 超长行缓存失效（否则 cache 与 read_line 不一致 →
            // 渲染/高亮用旧文本，选中/复制用新文本 → 偏移 1，用户实测）
            self.mark_huge_cache_dirty();
            // Snap the caret back to the previous line's start.
            if let Some((line, _)) = self.edit_cursor {
                self.editor_set_cursor(line, 0);
            }
        }
    }

    pub fn editor_redo(&mut self) {
        let did_redo = if let Some(arc) = self.engine.as_mut() {
            arc.lock().redo_one()
        } else {
            false
        };
        if did_redo {
            self.mark_huge_cache_dirty();
            if let Some((line, _)) = self.edit_cursor {
                self.editor_set_cursor(line, 0);
            }
        }
    }
}
