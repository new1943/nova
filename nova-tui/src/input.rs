use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use std::time::Duration;

/// Input action from keyboard — pure data, no app mutation
pub enum InputAction {
    Submit,
    Quit,
    Char(char),
    Backspace,
    Left,
    Right,
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    ToggleFocus,
    None,
}

/// Poll keyboard for one event. Blocks up to 50ms.
/// Designed to run on a dedicated OS thread (not async).
pub fn poll_input() -> InputAction {
    if event::poll(Duration::from_millis(50)).unwrap_or(false) {
        if let Ok(Event::Key(key)) = event::read() {
            return match (key.modifiers, key.code) {
                (KeyModifiers::CONTROL, KeyCode::Char('c')) => InputAction::Quit,
                (_, KeyCode::Enter) => InputAction::Submit,
                (_, KeyCode::Tab) => InputAction::ToggleFocus,
                (_, KeyCode::Backspace) => InputAction::Backspace,
                (_, KeyCode::Left) => InputAction::Left,
                (_, KeyCode::Right) => InputAction::Right,
                (_, KeyCode::Up) => InputAction::ScrollUp,
                (_, KeyCode::Down) => InputAction::ScrollDown,
                (_, KeyCode::PageUp) => InputAction::PageUp,
                (_, KeyCode::PageDown) => InputAction::PageDown,
                (_, KeyCode::Char(c)) => InputAction::Char(c),
                _ => InputAction::None,
            };
        }
    }
    InputAction::None
}
