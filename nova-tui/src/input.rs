use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::time::Duration;

/// Input action from keyboard — pure data, no app mutation
pub enum InputAction {
    Submit,
    Quit,
    Char(char),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    HistoryPrev,
    HistoryNext,
    ToggleFocus,
    Paste(String),
    None,
}

/// Poll keyboard for one event. Blocks up to 50ms.
/// Designed to run on a dedicated OS thread (not async).
pub fn poll_input() -> InputAction {
    if event::poll(Duration::from_millis(50)).unwrap_or(false) {
        if let Ok(Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        })) = event::read()
        {
            return match (modifiers, code) {
                (KeyModifiers::CONTROL, KeyCode::Char('c')) => InputAction::Quit,
                (KeyModifiers::CONTROL, KeyCode::Char('a')) => InputAction::Home,
                (KeyModifiers::CONTROL, KeyCode::Char('e')) => InputAction::End,
                (_, KeyCode::Enter) => InputAction::Submit,
                (_, KeyCode::Tab) => InputAction::ToggleFocus,
                (_, KeyCode::Backspace) => InputAction::Backspace,
                (_, KeyCode::Delete) => InputAction::Delete,
                (_, KeyCode::Left) => InputAction::Left,
                (_, KeyCode::Right) => InputAction::Right,
                (_, KeyCode::Home) => InputAction::Home,
                (_, KeyCode::End) => InputAction::End,
                // Up/Down: if in chat focus, scroll; otherwise history navigation
                // We'll let main.rs decide based on focus
                (KeyModifiers::SHIFT, KeyCode::Up) => InputAction::ScrollUp,
                (KeyModifiers::SHIFT, KeyCode::Down) => InputAction::ScrollDown,
                (_, KeyCode::Up) => InputAction::HistoryPrev,
                (_, KeyCode::Down) => InputAction::HistoryNext,
                (_, KeyCode::PageUp) => InputAction::PageUp,
                (_, KeyCode::PageDown) => InputAction::PageDown,
                (_, KeyCode::Char(c)) => InputAction::Char(c),
                _ => InputAction::None,
            };
        } else if let Ok(Event::Paste(s)) = event::read() {
            return InputAction::Paste(s);
        }
        // Consume non-Press events (Release, Repeat) silently
        return InputAction::None;
    }
    InputAction::None
}
