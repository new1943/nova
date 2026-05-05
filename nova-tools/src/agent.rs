//! AgentTool — spawn subagents for parallel/background tasks.
//!
//! This is a thin wrapper that delegates to SubagentSpawner in nova-agent.
//! It lives in nova-tools to avoid circular dependencies (nova-agent depends on nova-tools).
//! The actual spawning logic is injected via a trait object.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tracing::info;

use nova_core::models::ShadowEvent;
use crate::registry::{ToolHandler, ToolContext};

/// AgentTool — spawns background subagents for parallel task execution.
///
/// This tool provides a way for the main agent to spawn background workers.
/// The actual SubagentSpawner implementation is in nova-agent; this tool
/// stores the configuration needed to spawn subagents.
pub struct AgentTool {
    api_key: String,
    api_base_url: String,
    model: String,
    shadow_tx: Option<mpsc::Sender<ShadowEvent>>,
}

impl AgentTool {
    pub fn new(api_key: String, api_base_url: String, model: String) -> Self {
        Self {
            api_key,
            api_base_url,
            model,
            shadow_tx: None,
        }
    }

    pub fn with_shadow_tx(mut self, tx: mpsc::Sender<ShadowEvent>) -> Self {
        self.shadow_tx = Some(tx);
        self
    }
}

#[async_trait]
impl ToolHandler for AgentTool {
    fn name(&self) -> &str {
        "agent"
    }

    fn description(&self) -> &str {
        "Spawn a background subagent to handle a task in parallel. The subagent runs independently and reports results when done. Use for research, analysis, or tasks that don't need immediate results."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The task description for the subagent"
                },
                "name": {
                    "type": "string",
                    "description": "Optional name for the subagent (for tracking)"
                }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> Result<String> {
        let task = input.get("task")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let name = input.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("subagent");

        if task.is_empty() {
            return Ok(json!({"error": "task is required"}).to_string());
        }

        info!("AgentTool: spawning subagent '{}' for task: {}", name, task.chars().take(100).collect::<String>());

        // Note: The actual SubagentSpawner is in nova-agent.
        // This tool is registered by nova-daemon's tool_factory which has access to both.
        // For now, we use a simple single-call approach via nova-llm directly.
        let api = nova_llm::client::ApiClient::new(
            self.api_key.clone(),
            self.api_base_url.clone(),
        );

        let req = nova_llm::types::ApiRequest {
            model: self.model.clone(),
            max_tokens: 4096,
            system: "You are a helpful research assistant. Complete the given task concisely.".to_string(),
            messages: vec![nova_llm::types::ApiMessage::User {
                content: nova_llm::types::Content::Text(task.to_string()),
            }],
            tools: vec![],
            stream: false,
        };

        let shadow_tx = self.shadow_tx.clone();
        let task_id = format!("agent-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let task_id_clone = task_id.clone();
        let task_desc: String = task.chars().take(50).collect();

        // Emit task start
        if let Some(ref tx) = shadow_tx {
            let _ = tx.send(ShadowEvent::TaskProgress {
                task_id: task_id.clone(),
                action: nova_core::models::TaskAction::Add,
                description: task_desc.clone(),
            }).await;
        }

        let resp = api.complete(&req).await;

        match resp {
            Ok(response) => {
                let mut result = String::new();
                for block in &response.content {
                    if let nova_llm::types::ContentBlock::Text { text } = block {
                        result.push_str(text);
                    }
                }

                // Emit task complete
                if let Some(ref tx) = shadow_tx {
                    let _ = tx.send(ShadowEvent::TaskProgress {
                        task_id: task_id_clone,
                        action: nova_core::models::TaskAction::Complete,
                        description: format!("{} chars", result.len()),
                    }).await;
                }

                Ok(json!({
                    "status": "completed",
                    "agent_name": name,
                    "result": result
                }).to_string())
            }
            Err(e) => {
                // Emit task complete (with error)
                if let Some(ref tx) = shadow_tx {
                    let _ = tx.send(ShadowEvent::TaskProgress {
                        task_id: task_id_clone,
                        action: nova_core::models::TaskAction::Complete,
                        description: format!("Error: {}", e),
                    }).await;
                }

                Ok(json!({
                    "status": "error",
                    "agent_name": name,
                    "error": format!("{}", e)
                }).to_string())
            }
        }
    }
}
