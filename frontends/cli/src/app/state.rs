//! TUI application state. Wraps the core `Engine` and adds mode, viewport,
//! input buffer, and other UI-only state.

use std::path::PathBuf;

use anyhow::Result;

use qview_core::engine::Engine;
use qview_core::config::EngineConfig;
use crate::tui::viewport::Viewport;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Search,
    Command,
    /// Incremental search — user is still typing.
    SearchEdit,
    /// Waiting for second key of a two-key command (e.g. `dd`, `yy`).
    Operator(char),
    /// Visual line selection mode.
    Visual,
}

pub struct App {
    pub engine: Engine,
    pub viewport: Viewport,
    pub mode: Mode,
    pub input_buffer: String,
    pub should_quit: bool,
    pub tail_mode: bool,
    pub show_help: bool,
    /// Visual selection anchor line.
    pub visual_anchor: Option<u64>,
    /// P5：器灵状态（降级呈现——只显示 phase 文本，不开完整面板）。
    pub agent_status: AgentStatus,
}

/// P5：器灵状态（用于底部状态栏显示）。
#[derive(Debug, Clone, Default)]
pub struct AgentStatus {
    /// 当前 phase（None = 空闲）。
    pub phase: Option<qview_agent::event::Phase>,
}

impl AgentStatus {
    /// 状态栏显示文本（None → None；Some(p) → 中文短标签，符合小Q人设）。
    pub fn phase_label(&self) -> Option<String> {
        self.phase.as_ref().map(|p| match p {
            qview_agent::event::Phase::Routing => "计划中".into(),
            qview_agent::event::Phase::Thinking => "思考中".into(),
            qview_agent::event::Phase::Searching => "搜索中".into(),
            qview_agent::event::Phase::Inspecting => "检视中".into(),
            qview_agent::event::Phase::Drafting => "整理中".into(),
            qview_agent::event::Phase::AwaitingApproval => "待审批".into(),
            qview_agent::event::Phase::Done => "完成".into(),
            qview_agent::event::Phase::Failed => "失败".into(),
            qview_agent::event::Phase::Cancelled => "已取消".into(),
        })
    }
}

impl App {
    pub fn new(path: PathBuf) -> Result<Self> {
        Self::with_config(path, EngineConfig::default())
    }

    pub fn with_config(path: PathBuf, config: EngineConfig) -> Result<Self> {
        let engine = Engine::with_config(path, config)?;
        Ok(Self {
            engine,
            viewport: Viewport::default(),
            mode: Mode::Normal,
            input_buffer: String::new(),
            should_quit: false,
            tail_mode: false,
            show_help: false,
            visual_anchor: None,
            agent_status: AgentStatus::default(),
        })
    }

    // ---- delegated to engine ----

    pub fn effective_line_count(&self) -> u64 {
        self.engine.effective_line_count()
    }

    pub fn read_line(&self, line_no: u64) -> qview_core::cache::RawLine {
        self.engine.read_line(line_no)
    }

    pub fn is_modified(&self) -> bool {
        self.engine.is_modified()
    }

    pub fn build_index_blocking(&mut self) -> Result<()> {
        self.engine.build_index_blocking()
    }

    pub fn extend_index(&mut self, new_size: u64) -> Result<()> {
        self.engine.extend_index(new_size)
    }

    pub fn save(&mut self) -> Result<()> {
        if !self.engine.is_modified() {
            self.set_message("no changes");
            return Ok(());
        }
        self.engine.save()?;
        self.set_message(format!("wrote {} lines", self.engine.total_lines));
        Ok(())
    }

    pub fn reload(&mut self) -> Result<()> {
        self.engine.reload()?;
        self.set_message("reloaded");
        Ok(())
    }

    pub fn delete_logical_line(&mut self, line_no: u64) -> bool {
        self.engine.delete_logical_line(line_no)
    }

    pub fn yank_logical_line(&mut self, line_no: u64) -> bool {
        self.engine.yank_logical_line(line_no)
    }

    pub fn paste_after(&mut self, line_no: u64) -> bool {
        let ok = self.engine.paste_after(line_no);
        if !ok {
            self.set_message("yank stack empty");
        }
        ok
    }

    pub fn undo_one(&mut self) -> bool {
        let ok = self.engine.undo_one();
        if !ok {
            self.set_message("nothing to undo");
        } else {
            self.set_message("undo");
        }
        ok
    }

    // ---- messages ----

    pub fn set_message(&mut self, msg: impl Into<String>) {
        self.engine.set_message(msg);
    }

    pub fn clear_expired_message(&mut self) {
        self.engine.clear_expired_message();
    }

    // ---- mode / viewport ----

    pub fn cursor_line(&self) -> u64 {
        self.viewport.top_line
    }

    pub fn enter_search(&mut self) {
        self.mode = Mode::SearchEdit;
        self.input_buffer.clear();
    }

    pub fn enter_command(&mut self) {
        self.mode = Mode::Command;
        self.input_buffer.clear();
    }

    pub fn exit_input_mode(&mut self) {
        self.mode = Mode::Normal;
        self.input_buffer.clear();
        self.visual_anchor = None;
    }

    pub fn cancel_pending(&mut self) {
        if matches!(self.mode, Mode::Operator(_) | Mode::Visual) {
            self.mode = Mode::Normal;
            self.visual_anchor = None;
        }
    }

    pub fn on_search_buffer_change(&mut self) {
        self.engine.cache.invalidate_display();
    }

    // ---- search ----

    pub fn submit_search(&mut self) -> Result<()> {
        let q = self.input_buffer.clone();
        self.exit_input_mode();
        if q.is_empty() {
            self.engine.submit_search(String::new(), qview_core::search::SearchOptions::default())?;
            self.set_message("search cleared");
        } else {
            self.engine.submit_search(q.clone(), qview_core::search::SearchOptions::default())?;
            self.set_message(format!("searching '{}'...", q));
        }
        Ok(())
    }

    pub fn poll_bg_search(&mut self) -> bool {
        let (done, msg) = self.engine.poll_bg_search();
        if let Some(m) = msg {
            self.set_message(m);
        }
        done
    }

    // ---- visual mode ----

    pub fn enter_visual(&mut self, line_no: u64) {
        self.mode = Mode::Visual;
        self.visual_anchor = Some(line_no);
        self.set_message(format!("-- VISUAL -- line {}", line_no + 1));
    }

    pub fn exit_visual(&mut self) {
        self.mode = Mode::Normal;
        self.visual_anchor = None;
    }

    pub fn delete_visual(&mut self) -> bool {
        let anchor = match self.visual_anchor {
            Some(a) => a,
            None => return false,
        };
        let cursor = self.viewport.top_line;
        let (lo, hi) = if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        let mut yanked = Vec::with_capacity((hi - lo + 1) as usize);
        for n in (lo..=hi).rev() {
            if let Some(bytes) = self.engine.delete_logical_line_and_return(n) {
                yanked.push(bytes);
            }
        }
        yanked.reverse();
        if !yanked.is_empty() {
            self.engine.edits.yank_lines(yanked);
            self.set_message(format!("deleted {} lines", hi - lo + 1));
        }
        self.exit_visual();
        true
    }

    // ---- command processing ----

    pub fn submit_command(&mut self) -> Result<()> {
        let cmd = self.input_buffer.clone();
        self.exit_input_mode();
        if cmd == "q" || cmd == "quit" {
            self.should_quit = true;
            return Ok(());
        }
        if cmd == "q!" {
            self.engine.edits.clear();
            self.should_quit = true;
            self.set_message("discarded changes; quit");
            return Ok(());
        }
        if cmd == "e!" {
            self.reload()?;
            return Ok(());
        }
        if let Some(rest) = cmd.strip_prefix('w') {
            if rest.is_empty() {
                self.save()?;
                return Ok(());
            }
        }
        if cmd == "undo" || cmd == "u" {
            self.undo_one();
            return Ok(());
        }
        if let Some(rest) = cmd.strip_prefix('s') {
            if let Some((pat, repl, global)) = parse_substitute(rest) {
                let cur = self.cursor_line();
                if let Some(msg) = self.engine.substitute_current(cur, &pat, &repl, global) {
                    self.set_message(msg);
                }
                return Ok(());
            }
        }
        if let Some(rest) = cmd.strip_prefix("t") {
            if rest.is_empty() {
                self.tail_mode = !self.tail_mode;
                self.set_message(format!(
                    "tail mode: {}",
                    if self.tail_mode { "on" } else { "off" }
                ));
                return Ok(());
            }
            if rest == "f" {
                self.tail_mode = true;
                self.set_message("tail follow: on");
                return Ok(());
            }
        }
        if let Ok(line) = cmd.parse::<u64>() {
            let target = line.saturating_sub(1);
            self.viewport
                .to_line(target, self.engine.total_lines);
            self.set_message(format!("jumped to line {}", line));
            return Ok(());
        }
        self.set_message(format!("unknown command: :{}", cmd));
        Ok(())
    }
}

/// Parse `:s<delim>pat<delim>repl<delim>[g]`. Returns (pat, repl, global).
pub fn parse_substitute(input: &str) -> Option<(String, String, bool)> {
    let mut chars = input.chars();
    let delim = chars.next()?;
    let rest: String = chars.collect();
    let mut parts = rest.split(delim);
    let pat = parts.next()?.to_string();
    let repl = parts.next()?.to_string();
    let tail = parts.next().unwrap_or("");
    let global = tail.contains('g');
    Some((pat, repl, global))
}
