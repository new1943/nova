use std::sync::Arc;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tracing::info;

use crate::models::ShadowEvent;
use crate::subagent::{SubagentConfig, SubagentSpawner, SubagentType};
use crate::tools::Tool;

/// Tool that allows the LLM to spawn subagents for parallel or background tasks.
/// Each subagent runs independently with its own API call and returns a result string.
pub struct AgentTool {
    api_key: String,
    api_base_url: String,
    model: String,
    default_system_prompt: String,
    /// [V4 Fix] Channel for SubAgent TaskProgress events → Dispatcher → TaskManager
    shadow_tx: Option<mpsc::Sender<ShadowEvent>>,
}

impl AgentTool {
    pub fn new(
        api_key: String,
        api_base_url: String,
        model: String,
    ) -> Self {
        Self {
            api_key,
            api_base_url,
            model,
            default_system_prompt: String::new(),
            shadow_tx: None,
        }
    }

    pub fn with_shadow_tx(mut self, tx: mpsc::Sender<ShadowEvent>) -> Self {
        self.shadow_tx = Some(tx);
        self
    }

    pub fn with_system_prompt(mut self, prompt: String) -> Self {
        self.default_system_prompt = prompt;
        self
    }
}

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &str { "agent" }

    fn description(&self) -> &str {
        "Spawn a subagent to perform a task in parallel or in the background. \
         Use this when a task can be done independently — research, code generation, \
         file operations, etc. — and the results can be combined later. \
         The subagent makes its own API call and returns the result as a string."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The task or question for the subagent to perform"
                },
                "name": {
                    "type": "string",
                    "description": "Optional name for this subagent (for debugging/logging)"
                },
                "agent_type": {
                    "type": "string",
                    "enum": ["readonly", "full"],
                    "description": "'readonly' = can only research/search/plan (default), 'full' = can read/write/execute"
                }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let task = args.get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'task' field"))?;

        let name = args.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("agent");

        let agent_type = args.get("agent_type")
            .and_then(|v| v.as_str())
            .map(|t| match t {
                "full" => SubagentType::Full,
                _ => SubagentType::ReadOnly,
            })
            .unwrap_or(SubagentType::ReadOnly);

        let type_str = match &agent_type {
            SubagentType::ReadOnly => "readonly",
            SubagentType::Full => "full",
        };
        info!("AgentTool spawning subagent: {} (type: {})", name, type_str);

        let system_prompt = if self.default_system_prompt.is_empty() {
            String::new()
        } else {
            self.default_system_prompt.clone()
        };

        let config = SubagentConfig {
            name: name.to_string(),
            agent_type,
            team_name: None,
            system_prompt,
            api_key: self.api_key.clone(),
            api_base_url: self.api_base_url.clone(),
            model: self.model.clone(),
            max_input_tokens: 16_000,
            shadow_tx: None, // [V4 Fix] Can be wired to ShadowEvent bus
            tools: None, // [V4 Task 4.3] Future: enable tools for Full phases
        };

        let handle = SubagentSpawner::spawn(config, task.to_string());
        let result = handle.join.await??;

        Ok(result)
    }
}
