use serde::{Deserialize, Serialize};

/// TUI → Daemon requests
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Request {
    UserMessage { content: String },
    ResumeSession,
    NewSession,
    SearchSessions { query: String },
    Shutdown,
}

/// Daemon → TUI events (streamed)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Event {
    TextDelta { content: String },
    ToolCallStart { id: String, name: String },
    ToolCallResult { id: String, content: String },
    TurnEnd,
    Notification { message: String },
    Error { message: String },
    TokenUsage { input: u32, output: u32, budget_pct: f32 },
    SessionRestored { session_id: String, message_count: usize },
    SessionCreated { session_id: String },
    SearchResults { results: Vec<SearchResultEntry> },
}

/// A single search result entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultEntry {
    pub session_id: String,
    pub title: String,
    pub message_count: usize,
}
