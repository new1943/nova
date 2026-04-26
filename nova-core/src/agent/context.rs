//! CallerContext — identifies who should receive a notification.

/// [V4 Fix] Context for project completion notifications.
/// Determines which interface (TUI/Discord) should receive the notification.
#[derive(Debug, Clone)]
pub enum CallerContext {
    /// Send to TUI via IPC push channel
    Tui,
    /// Send to Discord channel
    Discord { channel_id: String },
}
