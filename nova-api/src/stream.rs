use serde_json::Value;

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

/// Accumulated tool call from streaming
#[derive(Debug, Clone)]
pub struct AccumulatedToolCall {
    pub id: String,
    pub name: String,
    pub input_json: String,
}

impl AccumulatedToolCall {
    pub fn parse_input(&self) -> anyhow::Result<Value> {
        Ok(serde_json::from_str(&self.input_json)?)
    }
}
