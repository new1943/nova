use std::sync::Arc;
use std::time::Duration;
use anyhow::Result;
use tokio::task::JoinHandle;
use tokio::sync::mpsc;
use tracing::{info, error, debug};
use serde_json::Value;

use nova_api::client::ApiClient;
use nova_api::types::{ApiMessage, ApiRequest, Content, ContentBlock};

use crate::models::{ShadowEvent, TaskAction};
use crate::tools::ToolRegistry;

/// [V4 Task 4.4] TaskProgress probe event for ShadowEvent bus
#[derive(Debug, Clone)]
pub struct TaskProgressProbe {
    pub task_id: String,
    pub action: String,  // "started", "tool_call", "completed"
    pub description: String,
}

/// Subagent capability level
#[derive(Debug, Clone, PartialEq)]
pub enum SubagentType {
    /// Read-only: can only research/search/plan
    ReadOnly,
    /// Full capability: can read/write files, execute commands
    Full,
}

/// Configuration for spawning a subagent
#[derive(Clone)]
pub struct SubagentConfig {
    pub name: String,
    pub agent_type: SubagentType,
    pub team_name: Option<String>,
    pub system_prompt: String,
    pub api_key: String,
    pub api_base_url: String,
    pub model: String,
    pub max_input_tokens: usize,
    /// [V4 Fix] Optional channel for emitting ShadowEvent::TaskProgress to ShadowEvent bus
    pub shadow_tx: Option<mpsc::Sender<ShadowEvent>>,
    /// [V4 Task 4.3] Optional ToolRegistry for Full phases (future: enable tool execution)
    pub tools: Option<Arc<ToolRegistry>>,
}

impl SubagentConfig {
    pub fn new(name: impl Into<String>, agent_type: SubagentType) -> Self {
        Self {
            name: name.into(),
            agent_type,
            team_name: None,
            system_prompt: String::new(),
            api_key: String::new(),
            api_base_url: "https://api.minimaxi.com/anthropic".into(),
            model: "MiniMax-M2.7".into(),
            max_input_tokens: 16_000,
            shadow_tx: None,
            tools: None,
        }
    }
}

/// Handle to a running subagent
pub struct SubagentHandle {
    pub name: String,
    pub join: JoinHandle<Result<String>>,
}

/// Subagent spawner — creates background agents
pub struct SubagentSpawner;

impl SubagentSpawner {
    /// Spawn a subagent that executes a single task and returns the result.
    /// Runs in background via tokio::spawn.
    /// [V4 Fix] Emits ShadowEvent::TaskProgress via shadow_tx if configured.
    /// [V4 Task 4.3] For Full phases with tools, enables actual tool execution loop.
    pub fn spawn(config: SubagentConfig, task: String) -> SubagentHandle {
        let name = config.name.clone();
        // Extract shadow_tx before moving config into async block
        let shadow_tx = config.shadow_tx.clone();
        let is_full = config.agent_type == SubagentType::Full;
        let has_tools = config.tools.is_some();
        let subagent_name = config.name.clone();
        let config_clone = config.clone();
        let join = tokio::spawn(async move {
            // [V4 Fix] Emit TaskProgress::Add when subagent starts
            if let Some(ref tx) = shadow_tx {
                if let Err(e) = tx.send(ShadowEvent::TaskProgress {
                    task_id: subagent_name.clone(),
                    action: TaskAction::Add,
                    description: task.chars().take(50).collect(),
                }).await {
                    debug!("[V4] TaskProgress Add failed: {}", e);
                }
            }

            info!("Subagent '{}' started: {}", subagent_name, task);

            let result = if is_full && has_tools {
                Self::run_with_tools_loop(config_clone, task).await
            } else {
                Self::run_simple(config_clone, task).await
            };

            // [V4 Fix] Emit TaskProgress::Complete when subagent finishes
            if let Some(ref tx) = shadow_tx {
                let desc = result.as_ref().map(|r| format!("{} chars", r.len())).unwrap_or_else(|e| e.to_string());
                if let Err(e) = tx.send(ShadowEvent::TaskProgress {
                    task_id: subagent_name.clone(),
                    action: TaskAction::Complete,
                    description: desc,
                }).await {
                    debug!("[V4] TaskProgress Complete failed: {}", e);
                }
            }

            info!("Subagent '{}' completed", subagent_name);
            result
        });

        SubagentHandle { name, join }
    }

    /// Simple single-API-call execution (for ReadOnly or no-tools Full)
    async fn run_simple(config: SubagentConfig, task: String) -> Result<String> {
        let name = config.name.clone();
        info!("[SubAgent:{}] Simple mode, sending single API request", name);
        let api = ApiClient::new(config.api_key.clone(), config.api_base_url.clone());

        let mut prompt = config.system_prompt;
        if config.agent_type == SubagentType::ReadOnly {
            prompt.push_str("\n\nYou are a read-only agent. You can only research, search, and plan. Do not modify any files.");
        }

        let req = ApiRequest {
            model: config.model.clone(),
            max_tokens: 4096,
            system: prompt,
            messages: vec![ApiMessage::User {
                content: Content::Text(task),
            }],
            tools: vec![],
            stream: false,
        };

        let resp = api.complete(&req).await?;

        let mut result = String::new();
        for block in &resp.content {
            if let ContentBlock::Text { text } = block {
                result.push_str(text);
            }
        }
        info!("[SubAgent:{}] Simple mode completed, output: {} chars", name, result.len());
        Ok(result)
    }

    /// [V4 Task 4.3] Full tool execution loop for Full phases with ToolRegistry.
    /// Streams API responses, executes tool calls, and continues until no more tool calls.
    async fn run_with_tools_loop(config: SubagentConfig, task: String) -> Result<String> {
        use nova_api::types::ToolSchema;

        let name = config.name.clone();
        info!("[SubAgent:{}] Tool loop started, task: {} chars", name, task.len());

        let api = ApiClient::new(config.api_key.clone(), config.api_base_url.clone());
        let tools = config.tools.as_ref().unwrap();
        let tool_timeout = Duration::from_secs(60);

        let prompt = config.system_prompt.clone();

        // Build initial request with tool schemas
        let tool_schemas: Vec<ToolSchema> = tools.as_api_schemas();
        let mut messages: Vec<ApiMessage> = vec![ApiMessage::User {
            content: Content::Text(task),
        }];

        let mut final_text = String::new();
        let mut loop_count = 0;
        const MAX_LOOPS: usize = 100; // Safety limit to prevent infinite loops

        loop {
            loop_count += 1;
            if loop_count > MAX_LOOPS {
                error!("[SubAgent:{}] Tool loop exceeded max iterations ({})", name, MAX_LOOPS);
                anyhow::bail!("Tool execution loop exceeded max iterations ({})", MAX_LOOPS);
            }

            info!("[SubAgent:{}] Loop {}/{}, sending API request ({} messages)",
                name, loop_count, MAX_LOOPS, messages.len());

            let req = ApiRequest {
                model: config.model.clone(),
                max_tokens: 4096,
                system: prompt.clone(),
                messages: messages.clone(),
                tools: tool_schemas.clone(),
                stream: false,
            };

            let resp = api.complete(&req).await?;

            // Collect text content and check for tool calls
            let mut has_tool_calls = false;
            let mut tool_call_names = Vec::new();
            let mut assistant_content_blocks = Vec::new();

            for block in &resp.content {
                match block {
                    ContentBlock::Text { text } => {
                        final_text.push_str(text);
                        assistant_content_blocks.push(block.clone());
                    }
                    ContentBlock::ToolUse { name: tool_name, .. } => {
                        has_tool_calls = true;
                        tool_call_names.push(tool_name.clone());
                        assistant_content_blocks.push(block.clone());
                    }
                    ContentBlock::Thinking { .. } => {
                        // Preserve thinking blocks
                        assistant_content_blocks.push(block.clone());
                    }
                    _ => {}
                }
            }

            if !tool_call_names.is_empty() {
                info!("[SubAgent:{}] Loop {} — tool calls: [{}]",
                    name, loop_count, tool_call_names.join(", "));
            }

            // Add assistant message to history
            if !assistant_content_blocks.is_empty() {
                messages.push(ApiMessage::Assistant {
                    content: Content::Blocks(assistant_content_blocks),
                });
            }

            if !has_tool_calls {
                // No tool calls - we're done
                info!("[SubAgent:{}] Loop {} — no tool calls, finishing", name, loop_count);
                break;
            }

            // [V4 Task 4.3] Execute tool calls and add results
            for block in &resp.content {
                if let ContentBlock::ToolUse { id, name: tool_name, input } = block {
                    // Execute the tool
                    let input_val: Value = input.clone();
                    let input_preview: String = input_val.to_string().chars().take(200).collect();
                    debug!("[SubAgent:{}] Executing tool '{}': {}", name, tool_name, input_preview);

                    let tool_result = match tools.execute(tool_name, input_val, tool_timeout).await {
                        Ok(r) => {
                            info!("[SubAgent:{}] Tool '{}' succeeded ({} chars)",
                                name, tool_name, r.len());
                            r
                        }
                        Err(e) => {
                            error!("[SubAgent:{}] Tool '{}' failed: {}", name, tool_name, e);
                            format!(r#"{{"error": "{}"}}"#, e)
                        }
                    };

                    // Add tool result to messages
                    messages.push(ApiMessage::User {
                        content: Content::Blocks(vec![ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: tool_result,
                        }]),
                    });
                }
            }
        }

        info!("[SubAgent:{}] Tool loop completed in {} loops, output: {} chars",
            name, loop_count, final_text.len());
        Ok(final_text)
    }

    /// Spawn multiple subagents in parallel, collect results
    pub async fn spawn_parallel(
        configs: Vec<(SubagentConfig, String)>,
    ) -> Vec<(String, Result<String>)> {
        let handles: Vec<SubagentHandle> = configs
            .into_iter()
            .map(|(cfg, task)| Self::spawn(cfg, task))
            .collect();

        let mut results = Vec::new();
        for handle in handles {
            let name = handle.name;
            let result = handle.join.await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("Join error: {}", e)));
            results.push((name, result));
        }
        results
    }
}
