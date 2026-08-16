//! Input mapping.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::Mode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    Quit,
    ScrollDown(u64),
    ScrollUp(u64),
    PageDown,
    PageUp,
    HalfPageDown,
    HalfPageUp,
    ToTop,
    ToBottom,
    GotoLine,
    EnterSearch,
    EnterCommand,
    NextSearchHit,
    PrevSearchHit,
    /// Jump forward by 10 hits.
    NextSearchHitBy(usize),
    /// Jump backward by 10 hits.
    PrevSearchHitBy(usize),
    /// Jump to first hit.
    FirstSearchHit,
    /// Jump to last hit.
    LastSearchHit,
    ScrollRight(u16),
    ScrollLeft(u16),
    CancelInput,
    SubmitInput,
    AppendChar(char),
    Backspace,
    ToggleTail,
    /// Force redraw.
    Redraw,
    /// Toggle help overlay.
    ToggleHelp,
    // ---- edit operations ----
    /// Start the `d` operator (waiting for second key, usually `d`).
    OperatorDelete,
    /// Start the `y` operator.
    OperatorYank,
    /// Apply pending operator on current line (e.g. `dd`, `yy`).
    ApplyOperatorLine(char),
    PasteAfter,
    Undo,
    EnterVisual,
    /// Delete the current visual selection.
    DeleteVisual,
    None,
}

pub fn map_key(mode: Mode, key: KeyEvent) -> InputAction {
    match mode {
        Mode::Normal => map_normal(key),
        Mode::Search => map_input(key),
        Mode::SearchEdit => map_input_editing(key),
        Mode::Command => map_input(key),
        Mode::Operator(_) => map_operator(key),
        Mode::Visual => map_visual(key),
    }
}

fn map_normal(key: KeyEvent) -> InputAction {
    match key.code {
        KeyCode::Char('q') => InputAction::Quit,
        KeyCode::Char('j') | KeyCode::Down => InputAction::ScrollDown(1),
        KeyCode::Char('k') | KeyCode::Up => InputAction::ScrollUp(1),
        KeyCode::Char('d') => InputAction::OperatorDelete,
        KeyCode::Char('y') => InputAction::OperatorYank,
        KeyCode::Char('p') => InputAction::PasteAfter,
        KeyCode::Char('u') => InputAction::Undo,
        KeyCode::Char(' ') | KeyCode::PageDown => InputAction::PageDown,
        KeyCode::PageUp => InputAction::PageUp,
        KeyCode::Char('g') => InputAction::ToTop,
        KeyCode::Char('G') => InputAction::ToBottom,
        KeyCode::Char(':') => InputAction::EnterCommand,
        KeyCode::Char('/') => InputAction::EnterSearch,
        KeyCode::Char('n') => InputAction::NextSearchHit,
        KeyCode::Char('N') => InputAction::PrevSearchHit,
        KeyCode::Char(']') => InputAction::NextSearchHitBy(10),
        KeyCode::Char('[') => InputAction::PrevSearchHitBy(10),
        KeyCode::Char('}') => InputAction::LastSearchHit,
        KeyCode::Char('{') => InputAction::FirstSearchHit,
        KeyCode::Char('h') | KeyCode::Left => InputAction::ScrollLeft(8),
        KeyCode::Char('l') | KeyCode::Right => InputAction::ScrollRight(8),
        KeyCode::Char('v') => InputAction::EnterVisual,
        KeyCode::Char('F') => InputAction::ToggleTail,
        KeyCode::Char('?') => InputAction::ToggleHelp,
        _ => InputAction::None,
    }
}

fn map_operator(key: KeyEvent) -> InputAction {
    match key.code {
        KeyCode::Char('d') => InputAction::ApplyOperatorLine('d'),
        KeyCode::Char('y') => InputAction::ApplyOperatorLine('y'),
        KeyCode::Esc => InputAction::CancelInput,
        _ => InputAction::CancelInput,
    }
}

fn map_visual(key: KeyEvent) -> InputAction {
    match key.code {
        KeyCode::Esc => InputAction::CancelInput,
        KeyCode::Char('d') => InputAction::DeleteVisual,
        KeyCode::Char('y') => InputAction::ApplyOperatorLine('y'),
        KeyCode::Char('j') | KeyCode::Down => InputAction::ScrollDown(1),
        KeyCode::Char('k') | KeyCode::Up => InputAction::ScrollUp(1),
        KeyCode::Char('G') => InputAction::ToBottom,
        KeyCode::Char('g') => InputAction::ToTop,
        KeyCode::Char(' ') | KeyCode::PageDown => InputAction::PageDown,
        KeyCode::PageUp => InputAction::PageUp,
        _ => InputAction::None,
    }
}

fn map_input(key: KeyEvent) -> InputAction {
    match key.code {
        KeyCode::Esc => InputAction::CancelInput,
        KeyCode::Enter => InputAction::SubmitInput,
        KeyCode::Backspace => InputAction::Backspace,
        KeyCode::Char(c) => InputAction::AppendChar(c),
        _ => InputAction::None,
    }
}

/// In SearchEdit mode, every char backspaces updates the buffer and refreshes
/// highlight live; Enter commits to a real search.
fn map_input_editing(key: KeyEvent) -> InputAction {
    map_input(key)
}

pub fn is_ctrl_c(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
}