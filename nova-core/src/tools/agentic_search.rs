use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::info;

use crate::session::manager::SessionManager;
use crate::session::search::AgenticSessionSearch;
use crate::sidequery::SideQuery;
use crate::tools::Tool;

pub struct AgenticSearchTool {
    side_query: SideQuery,
    session_manager: SessionManager,
}

impl AgenticSearchTool {
    pub fn new(side_query: SideQuery, session_manager: SessionManager) -> Self {
        Self {
            side_query,
            session_manager,
        }
    }
}

#[async_trait]
impl Tool for AgenticSearchTool {
    fn name(&self) -> &str {
        "agentic_search"
    }

    fn description(&self) -> &str {
        "Search through all historical conversations and projects semantically. \
         Use this tool especially when the user asks questions like 'Do you remember...', \
         'Have we discussed...', or 'We talked about...?'. \
         It uses an agentic LLM-based semantic search to find the most relevant history."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query, topics, or keywords to search for across past sessions"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'query' parameter"))?;

        info!("Tool agentic_search invoked with query: {}", query);

        let searcher = AgenticSessionSearch::new(self.side_query.clone(), self.session_manager.clone());
        let results = searcher.search(query).await?;

        if results.is_empty() {
            return Ok(format!("No relevant sessions found for query: {}", query));
        }

        let mut output = format!("Found {} relevant sessions:\n\n", results.len());
        for (i, entry) in results.iter().enumerate() {
            output.push_str(&format!("--- Session {} (ID: {}) ---\n", i + 1, entry.session_id));
            if !entry.first_message.is_empty() {
                output.push_str(&format!("Title/First Message: {}\n", entry.first_message));
            }
            output.push_str(&format!("Metrics: {} turns, {} messages\n", entry.turn_count, entry.message_count));
            if !entry.transcript.is_empty() {
                output.push_str(&format!("Excerpt: {}\n", entry.transcript));
            }
            output.push('\n');
        }

        Ok(output)
    }
}
