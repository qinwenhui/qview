use std::io::{stdout, Stdout};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{self, Event, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use qview::app::{App, Mode};
use qview::config::AppConfig;
use qview_core::file::persist::{file_meta, peek_header, write_index, IndexFile};
use qview_core::file::watch::{derive_index_path, FileWatcher};
use qview_core::file::{LineIndex, SPARSE_FACTOR};
use qview::tui::input::{is_ctrl_c, map_key, InputAction};
use qview::tui::render::{render, StatusInfo};

#[derive(Parser, Debug)]
#[command(version, about = "Ultra high-performance log viewer")]
struct Cli {
    /// Path to the log file.
    file: PathBuf,

    /// Disable persistent index.
    #[arg(long)]
    no_index: bool,

    /// Build index in foreground (block UI until done).
    #[arg(long)]
    sync_index: bool,

    /// Start in tail-follow mode (incremental reindex).
    #[arg(short = 'f', long)]
    follow: bool,

    /// Path to a TOML config file overriding engine defaults.
    #[arg(long)]
    config: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let (app_cfg, cfg_source) = AppConfig::load_with_override(cli.config.as_deref())
        .context("load tui config")?;
    eprintln!("[qview] config loaded from {}", cfg_source.display());

    let index_path = derive_index_path(&cli.file);
    let meta = file_meta(&cli.file)?;
    let mut app = App::with_config(cli.file.clone(), app_cfg.engine_config().clone())?;
    app.tail_mode = cli.follow;

    if !cli.no_index && index_path.exists() {
        match peek_header(&index_path) {
            Ok(h)
                if h.file_size == meta.size
                    && h.file_mtime == meta.mtime
                    && h.file_inode == meta.inode =>
            {
                match IndexFile::open(&index_path) {
                    Ok(idx) => {
                        let line_count = idx.header.line_count;
                        let offsets = if idx.header.offset_size == 4 {
                            idx.offsets_u32().iter().map(|&o| o as u64).collect()
                        } else {
                            idx.offsets_u64().to_vec()
                        };
                        app.engine.index = LineIndex::from_vec(offsets, meta.size);
                        app.engine.total_lines = line_count;
                        app.engine.known_size = meta.size;
                    }
                    Err(e) => {
                        app.set_message(format!("index load failed: {}", e));
                    }
                }
            }
            _ => {
                app.set_message("index stale; will rebuild");
            }
        }
    }

    if cli.sync_index || app.engine.total_lines == 0 {
        if app.engine.total_lines == 0 {
            app.set_message("building index...");
            let t0 = Instant::now();
            app.build_index_blocking().context("build index")?;
            app.engine.known_size = app.engine.mmap.size();
            app.set_message(format!(
                "index ready ({} lines in {:?})",
                app.engine.total_lines,
                t0.elapsed()
            ));
        }
        if !cli.no_index {
            let sparse_offsets = app.engine.index.snapshot_offsets();
            if let Err(e) = write_index(
                &index_path,
                meta.size,
                meta.mtime,
                meta.inode,
                app.engine.total_lines,
                &sparse_offsets,
                SPARSE_FACTOR,
                app.engine.index.max_line_bytes(),
                app.engine.index.max_line_index(),
            ) {
                app.set_message(format!("persist index failed: {}", e));
            }
        }
    }

    // Spawn file watcher for tail -f.
    let watcher = FileWatcher::spawn(cli.file.clone(), Duration::from_millis(500))?;

    let mut terminal = setup_terminal()?;
    let res = run_loop(&mut terminal, app, &watcher);
    teardown_terminal(terminal)?;
    res
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut out = stdout();
    // Mouse capture is off — on Windows cmd it causes display corruption
    // and we don't implement mouse interaction anyway.
    execute!(out, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    Ok(Terminal::new(backend)?)
}

fn teardown_terminal(mut terminal: Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn run_loop<B>(terminal: &mut Terminal<B>, mut app: App, watcher: &FileWatcher) -> Result<()>
where
    B: ratatui::backend::Backend,
{
    // Use a short poll while background work is active (search/index),
    // and a longer poll when idle. ratatui's diff-based rendering makes
    // no-change frames essentially free.
    let active_tick = Duration::from_millis(50);
    let idle_tick = Duration::from_millis(250);

    loop {
        app.clear_expired_message();

        // Tail -f: detect file growth and reindex incrementally.
        if let Some(new_size) = watcher.try_next() {
            if new_size != app.engine.known_size {
                if let Err(e) = app.engine.mmap.refresh() {
                    app.set_message(format!("refresh failed: {}", e));
                } else if new_size > app.engine.known_size {
                    if let Err(e) = app.extend_index(new_size) {
                        app.set_message(format!("extend index failed: {}", e));
                    } else {
                        app.set_message(format!("tail: +{} lines", app.engine.total_lines));
                        if app.tail_mode {
                            app.viewport.to_bottom(app.effective_line_count());
                        }
                    }
                }
            }
        }

        // Background search progress.
        app.poll_bg_search();

        let size = terminal.size()?;
        app.viewport.resize(size.height.saturating_sub(3));

        let has_bg_work = app.engine.bg_search.is_some() || app.engine.bg_indexer.is_some();
        let poll_dur = if has_bg_work { active_tick } else { idle_tick };

        terminal.draw(|frame| {
            let info = StatusInfo {
                total_lines: app.effective_line_count(),
                file_size: app.engine.mmap.size(),
                indexed: app.engine.index.is_complete(),
                search_query: app.engine.search_query.clone(),
                search_hits: app.engine.search.len(),
                search_cursor: if app.engine.search.is_empty() {
                    None
                } else {
                    Some(app.engine.search.cursor())
                },
                mode: app.mode,
                message: app.engine.message.clone(),
                agent_phase: app.agent_status.phase_label(),
                input_buffer: app.input_buffer.clone(),
                file_path: app
                    .engine
                    .mmap
                    .path()
                    .to_str()
                    .unwrap_or("?")
                    .to_string(),
                tail_mode: app.tail_mode,
                show_help: app.show_help,
                modified: app.is_modified(),
            };
            render(frame, &mut app, info);
        })?;

        if event::poll(poll_dur)? {
            if let Event::Key(key) = event::read()? {
                // Filter to Press only: on Windows, holding a key generates
                // Repeat events, which causes toggle actions (like ? help)
                // to flicker or get stuck.
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if is_ctrl_c(&key) {
                    app.should_quit = true;
                } else {
                    handle_key(&mut app, key)?;
                }
            }
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    let action = map_key(app.mode, key);
    let total_lines = app.effective_line_count();
    match action {
        InputAction::None => {}
        InputAction::Quit => app.should_quit = true,
        InputAction::ScrollDown(n) => app.viewport.scroll_down(n, total_lines),
        InputAction::ScrollUp(n) => app.viewport.scroll_up(n),
        InputAction::PageDown => app.viewport.page_down(total_lines),
        InputAction::PageUp => app.viewport.page_up(),
        InputAction::HalfPageDown => {
            let h = (app.viewport.visible_h as u64) / 2;
            app.viewport.scroll_down(h.max(1), total_lines)
        }
        InputAction::HalfPageUp => {
            let h = (app.viewport.visible_h as u64) / 2;
            app.viewport.scroll_up(h.max(1))
        }
        InputAction::ToTop => app.viewport.to_top(),
        InputAction::ToBottom => app.viewport.to_bottom(total_lines),
        InputAction::GotoLine => {
            app.enter_command();
            app.input_buffer.push('0');
        }
        InputAction::EnterSearch => {
            app.enter_search();
        }
        InputAction::EnterCommand => {
            app.enter_command();
        }
        InputAction::NextSearchHit => jump_search_hit(app, true, 1)?,
        InputAction::PrevSearchHit => jump_search_hit(app, false, 1)?,
        InputAction::NextSearchHitBy(n) => jump_search_hit(app, true, n as i64)?,
        InputAction::PrevSearchHitBy(n) => jump_search_hit(app, false, -(n as i64))?,
        InputAction::FirstSearchHit => {
            if app.engine.search.first().is_some() {
                jump_to_cursor(app);
            } else {
                app.set_message("no search active");
            }
        }
        InputAction::LastSearchHit => {
            if app.engine.search.last().is_some() {
                jump_to_cursor(app);
            } else {
                app.set_message("no search active");
            }
        }
        InputAction::ScrollRight(n) => app.viewport.scroll_right(n),
        InputAction::ScrollLeft(n) => app.viewport.scroll_left(n),
        InputAction::ToggleTail => {
            app.tail_mode = !app.tail_mode;
            app.set_message(format!(
                "tail mode: {}",
                if app.tail_mode { "on" } else { "off" }
            ));
        }
        InputAction::ToggleHelp => {
            app.show_help = !app.show_help;
        }
        InputAction::Redraw => {
            app.engine.cache.invalidate_display();
        }
        InputAction::CancelInput => app.exit_input_mode(),
        InputAction::SubmitInput => match app.mode {
            Mode::Search => app.submit_search()?,
            Mode::SearchEdit => app.submit_search()?,
            Mode::Command => app.submit_command()?,
            _ => {}
        },
        InputAction::AppendChar(c) => {
            app.input_buffer.push(c);
            if app.mode == Mode::Search {
                app.on_search_buffer_change();
            }
        }
        InputAction::Backspace => {
            app.input_buffer.pop();
            if app.mode == Mode::Search {
                app.on_search_buffer_change();
            }
        }
        InputAction::OperatorDelete => {
            app.mode = Mode::Operator('d');
        }
        InputAction::OperatorYank => {
            app.mode = Mode::Operator('y');
        }
        InputAction::ApplyOperatorLine(op) => {
            let cur = app.cursor_line();
            match op {
                'd' => {
                    app.delete_logical_line(cur);
                }
                'y' => {
                    if app.mode == Mode::Visual {
                        if let Some(anchor) = app.visual_anchor {
                            let (lo, hi) = normalize_range(anchor, cur);
                            let mut lines = Vec::new();
                            for n in lo..=hi {
                                if let Some(b) = nonempty_bytes(&app.read_line(n).text) {
                                    lines.push(b);
                                }
                            }
                            if !lines.is_empty() {
                                app.engine.edits.yank_lines(lines);
                                app.set_message(format!("yanked {} lines", hi - lo + 1));
                            }
                        }
                        app.exit_visual();
                    } else {
                        app.yank_logical_line(cur);
                        app.set_message("yanked 1 line");
                    }
                }
                _ => {}
            }
            app.mode = Mode::Normal;
        }
        InputAction::PasteAfter => {
            app.paste_after(app.cursor_line());
        }
        InputAction::Undo => {
            app.undo_one();
        }
        InputAction::EnterVisual => {
            app.enter_visual(app.cursor_line());
        }
        InputAction::DeleteVisual => {
            app.delete_visual();
        }
    }
    Ok(())
}

fn normalize_range(a: u64, b: u64) -> (u64, u64) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

fn nonempty_bytes(s: &str) -> Option<Vec<u8>> {
    if s.is_empty() {
        None
    } else {
        Some(s.as_bytes().to_vec())
    }
}

fn jump_search_hit(app: &mut App, forward: bool, delta: i64) -> Result<()> {
    if app.engine.search.is_empty() {
        app.set_message("no search active (press /)");
        return Ok(());
    }
    let hit = if delta == 1 {
        if forward {
            app.engine.search.next()
        } else {
            app.engine.search.prev()
        }
    } else if delta == -1 {
        if forward {
            app.engine.search.prev()
        } else {
            app.engine.search.next()
        }
    } else {
        app.engine
            .search
            .jump_by(if forward { delta } else { -delta })
    };
    if let Some(h) = hit {
        let line = app.engine.index.line_of_byte(h.byte);
        app.viewport.center_on(line, app.effective_line_count());
        app.set_message(format!(
            "hit {}/{}",
            app.engine.search.cursor() + 1,
            app.engine.search.len()
        ));
    } else {
        app.set_message("at boundary");
    }
    Ok(())
}

fn jump_to_cursor(app: &mut App) {
    if let Some(h) = app.engine.search.current() {
        let line = app.engine.index.line_of_byte(h.byte);
        app.viewport.center_on(line, app.effective_line_count());
        app.set_message(format!(
            "hit {}/{}",
            app.engine.search.cursor() + 1,
            app.engine.search.len()
        ));
    }
}
