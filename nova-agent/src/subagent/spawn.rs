use std::sync::Arc;
use std::time::Duration;
use anyhow::Result;
use tokio::task::JoinHandle;
use tokio::sync::mpsc;
use tracing::{info, error, debug};

use nova_llm::client::ApiClient;
use nova_llm::types::{ApiMessage, ApiRequest, Content, ContentBlock};

use nova_core::models::{ShadowEvent, TaskAction};
use nova_tools::registry::{ToolRegistry, ToolContext};

/// [V4 Task 4.4] TaskProgress probe event for ShadowEvent bus
#[derive(Debug, Clone)]
pub struct TaskProgressProbe {
    pub task_id: String,
    pub action: String,
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
    pub shadow_tx: Option<mpsc::Sender<ShadowEvent>>,
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
    pub fn spawn(config: SubagentConfig, task: String) -> SubagentHandle {
        let name = config.name.clone();
        let shadow_tx = config.shadow_tx.clone();
        let is_full = config.agent_type == SubagentType::Full;
        let has_tools = config.tools.is_some();
        let subagent_name = config.name.clone();
        let config_clone = config.clone();
        let join = tokio::spawn(async move {
            // Emit TaskProgress::Add when subagent starts
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

            // Emit TaskProgress::Complete when subagent finishes
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

    async fn run_with_tools_loop(config: SubagentConfig, task: String) -> Result<String> {
        use nova_llm::types::ToolSchema;
        use nova_llm::stream::StreamEvent;

        let name = config.name.clone();
        info!("[SubAgent:{}] Tool loop started, task: {} chars", name, task.len());

        let tools = config.tools.as_ref().unwrap();
        let tool_timeout = Duration::from_secs(60);
        let tool_context = ToolContext::new("subagent".to_string(), None);

        let prompt = config.system_prompt.clone();

        // Build initial request with tool schemas
        let tool_schemas: Vec<ToolSchema> = tools.as_api_schemas();
        let mut messages: Vec<ApiMessage> = vec![ApiMessage::User {
            content: Content::Text(task),
        }];

        let mut final_text = String::new();
        let mut loop_count = 0;
        const MAX_LOOPS: usize = 100;

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
                stream: true,
            };

            let (stream_tx, mut stream_rx) = mpsc::channel::<StreamEvent>(64);
            let api_key = config.api_key.clone();
            let api_base_url = config.api_base_url.clone();
            let stream_handle = tokio::spawn(async move {
                let api = ApiClient::new(api_key, api_base_url);
                api.stream(&req, stream_tx).await
            });

            // Collect streamed response
            let mut current_text = String::new();
            let mut tool_calls: Vec<nova_llm::stream::AccumulatedToolCall> = Vec::new();
            let mut current_tool: Option<nova_llm::stream::AccumulatedToolCall> = None;
            let mut assistant_content_blocks = Vec::new();

            while let Some(event) = stream_rx.recv().await {
                match event {
                    StreamEvent::TextDelta(text) => {
                        current_text.push_str(&text);
                        final_text.push_str(&text);
                    }
                    StreamEvent::ToolUseStart { id, name } => {
                        current_tool = Some(nova_llm::stream::AccumulatedToolCall {
                            id,
                            name,
                            input_json: String::new(),
                        });
                    }
                    StreamEvent::ToolInputDelta(json) => {
                        if let Some(ref mut tool) = current_tool {
                            tool.input_json.push_str(&json);
                        }
                    }
                    StreamEvent::ToolUseEnd { .. } => {
                        if let Some(tool) = current_tool.take() {
                            tool_calls.push(tool);
                        }
                    }
                    StreamEvent::Error(e) => {
                        error!("[SubAgent:{}] Stream error: {}", name, e);
                    }
                    _ => {}
                }
            }

            if let Err(e) = stream_handle.await {
                error!("[SubAgent:{}] Stream task panicked: {}", name, e);
            }

            // Fallback: Parse `<tool_call>` XML from text if proxy failed
            if tool_calls.is_empty() && current_text.contains("<tool_call>") {
                let mut temp_text = current_text.as_str();
                while let Some(start) = temp_text.find("<tool_call>") {
                    temp_text = &temp_text[start + "<tool_call>".len()..];
                    if let Some(end) = temp_text.find("</tool_call>") {
                        let inner = &temp_text[..end];
                        temp_text = &temp_text[end + "</tool_call>".len()..];
                        
                        if let Some(f_start) = inner.find("<function=") {
                            let f_rest = &inner[f_start + "<function=".len()..];
                            if let Some(f_end) = f_rest.find(">") {
                                let func_name = f_rest[..f_end].trim().to_string();
                                let mut args = serde_json::Map::new();
                                
                                let mut p_rest = &f_rest[f_end + 1..];
                                while let Some(p_start) = p_rest.find("<parameter=") {
                                    p_rest = &p_rest[p_start + "<parameter=".len()..];
                                    if let Some(p_end) = p_rest.find(">") {
                                        let p_name = p_rest[..p_end].trim().to_string();
                                        let v_rest = &p_rest[p_end + 1..];
                                        if let Some(v_end) = v_rest.find("</parameter>") {
                                            let p_val = v_rest[..v_end].trim().to_string();
                                            args.insert(p_name, serde_json::Value::String(p_val));
                                            p_rest = &v_rest[v_end + "</parameter>".len()..];
                                        } else { break; }
                                    } else { break; }
                                }
                                
                                tool_calls.push(nova_llm::stream::AccumulatedToolCall {
                                    id: format!("call_{}", uuid::Uuid::new_v4().to_string().replace("-", "")),
                                    name: func_name,
                                    input_json: serde_json::Value::Object(args).to_string(),
                                });
                            }
                        }
                    }
                }
            }

            if !current_text.is_empty() {
                assistant_content_blocks.push(ContentBlock::Text { text: current_text });
            }

            let mut tool_call_names = Vec::new();
            for tc in &tool_calls {
                tool_call_names.push(tc.name.clone());
                assistant_content_blocks.push(ContentBlock::ToolUse {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    input: tc.parse_input().unwrap_or_else(|_| serde_json::json!({})),
                });
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

            if tool_calls.is_empty() {
                info!("[SubAgent:{}] Loop {} — no tool calls, finishing", name, loop_count);
                break;
            }

            // Execute tool calls and add results
            for tc in tool_calls {
                let input_val = tc.parse_input().unwrap_or_else(|_| serde_json::json!({}));
                let input_preview: String = input_val.to_string().chars().take(200).collect();
                debug!("[SubAgent:{}] Executing tool '{}': {}", name, tc.name, input_preview);

                let tool_result = match tools.execute(&tc.name, input_val, &tool_context, tool_timeout).await {
                    Ok(r) => {
                        info!("[SubAgent:{}] Tool '{}' succeeded ({} chars)",
                            name, tc.name, r.len());
                        r
                    }
                    Err(e) => {
                        error!("[SubAgent:{}] Tool '{}' failed: {}", name, tc.name, e);
                        format!(r#"{{"error": "{}"}}"#, e)
                    }
                };

                // Add tool result to messages
                messages.push(ApiMessage::User {
                    content: Content::Blocks(vec![ContentBlock::ToolResult {
                        tool_use_id: tc.id,
                        content: tool_result,
                    }]),
                });
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
