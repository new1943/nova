use crate::types::Usage;

/// High-level stream event emitted by ApiClient::stream()
#[derive(Debug)]
pub enum StreamEvent {
    /// Incremental text content
    TextDelta(String),
    /// Tool use block started
    ToolUseStart { id: String, name: String },
    /// Incremental tool input JSON
    ToolInputDelta(String),
    /// Tool use block finished — caller should parse accumulated JSON
    ToolUseEnd { index: usize },
    /// Message complete
    MessageStop { stop_reason: Option<String> },
    /// Token usage update
    Usage(Usage),
    /// Error from API
    Error(String),
}
