//! AgenticSearchTool — exposes semantic session search as a tool.
//!
//! Lives in nova-daemon because it depends on both nova-memory (SideQuery, SessionManager)
//! and nova-tools (ToolHandler trait). Placing it in nova-tools would create a forbidden
//! dependency on nova-memory.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use nova_memory::session::manager::SessionManager;
use nova_memory::sidequery::SideQuery;
use nova_memory::session::AgenticSessionSearch;
use nova_tools::registry::{ToolHandler, ToolContext};

/// Agentic search tool — searches past sessions semantically.
pub struct AgenticSearchTool {
    side_query: SideQuery,
    session_manager: SessionManager,
}

impl AgenticSearchTool {
    pub fn new(side_query: SideQuery, session_manager: SessionManager) -> Self {
        Self { side_query, session_manager }
    }
}

#[async_trait]
impl ToolHandler for AgenticSearchTool {
    fn name(&self) -> &str {
        "agentic_search"
    }

    fn description(&self) -> &str {
        "Search past conversation sessions semantically. Use when the user references something from a previous conversation, or when you need context from earlier sessions."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query to find relevant past sessions"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> Result<String> {
        let query = input.get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if query.is_empty() {
            return Ok(json!({"error": "query is required"}).to_string());
        }

        let searcher = AgenticSessionSearch::new(
            self.side_query.clone(),
            self.session_manager.clone(),
        );

        match searcher.search(query).await {
            Ok(results) => {
                let formatted: Vec<Value> = results.iter().take(5).map(|r| {
                    json!({
                        "session_id": r.session_id,
                        "first_message": r.first_message,
                        "message_count": r.message_count,
                        "transcript": r.transcript.chars().take(2000).collect::<String>(),
                    })
                }).collect();
                Ok(json!({"results": formatted}).to_string())
            }
            Err(e) => Ok(json!({"error": format!("Search failed: {}", e)}).to_string()),
        }
    }
}
