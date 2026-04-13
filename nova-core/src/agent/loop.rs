use anyhow::Result;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::warn;

use nova_api::stream::{AccumulatedToolCall, StreamEvent};
use nova_api::types::{
    ApiMessage, ApiRequest, Content, ContentBlock, Usage,
};

use crate::hooks::HookManager;
use crate::message::{Message, Role, ToolCall};
use crate::session::manager::Session;
use crate::token::budget::{BudgetCheck, TokenBudget};
use crate::token::compact::Compactor;
use crate::tools::registry::ToolRegistry;

/// Event emitted by the query loop to the caller (TUI/daemon)
#[derive(Debug)]
pub enum LoopEvent {
    TextDelta(String),
    ToolCallStart { id: String, name: String },
    ToolCallResult { id: String, content: String },
    TurnEnd,
    TokenUsage { input: u32, output: u32 },
    Error(String),
    CompactTriggered,
}

/// Query Loop configuration
#[derive(Clone)]
pub struct QueryLoopConfig {
    pub max_turns: usize,
    pub tool_timeout: Duration,
    pub model: String,
    pub max_tokens: u32,
    pub api_key: String,
    pub api_base_url: String,
    pub context_window: usize,
    pub budget_trigger_pct: f32,
    pub compact_target_pct: f32,
}

impl Default for QueryLoopConfig {
    fn default() -> Self {
        Self {
            max_turns: 20,
            tool_timeout: Duration::from_secs(60),
            model: "MiniMax-M2.7".into(),
            max_tokens: 8192,
            api_key: String::new(),
            api_base_url: "https://api.minimaxi.com/anthropic".into(),
            context_window: 200_000,
            budget_trigger_pct: 0.9,
            compact_target_pct: 0.6,
        }
    }
}

/// The core Query Loop — strategies 1+2+3+5+6 integrated
pub struct QueryLoop {
    pub config: QueryLoopConfig,
    pub tools: ToolRegistry,
    pub hooks: HookManager,
}

impl QueryLoop {
    pub fn new(tools: ToolRegistry, hooks: HookManager, config: QueryLoopConfig) -> Self {
        Self { config, tools, hooks }
    }

    /// Run a full query loop turn for user input.
    pub async fn run(
        &self,
        session: &mut Session,
        system_prompt: &str,
        event_tx: mpsc::Sender<LoopEvent>,
    ) -> Result<()> {
        let tool_schemas = self.tools.as_api_schemas();
        let mut budget = TokenBudget::new(
            self.config.context_window,
            self.config.budget_trigger_pct,
        );
        let compactor = Compactor::new(
            self.config.compact_target_pct,
            self.config.api_key.clone(),
            self.config.api_base_url.clone(),
            self.config.model.clone(),
        );
        let mut empty_retries: u32 = 0;
        const MAX_EMPTY_RETRIES: u32 = 3;

        loop {
            if !session.increment_turn() {
                let _ = event_tx.send(LoopEvent::Error(
                    format!("Max turns ({}) reached", self.config.max_turns),
                )).await;
                break;
            }

            // --- Strategy 2: Pre-flight budget check using cumulative tokens ---
            // Only check capacity threshold here; diminishing returns is checked post-flight
            // with per-turn values.
            let cumulative = session.token_stats.total_input_tokens as usize;
            if cumulative > 0 && budget.needs_compact(cumulative) {
                let _ = event_tx.send(LoopEvent::CompactTriggered).await;
                match compactor.compact(&session.messages, self.config.context_window).await {
                    Ok(compacted) => session.messages = compacted,
                    Err(e) => warn!("Pre-flight compact failed: {}", e),
                }
            }

            // Build request
            let api_messages = build_api_messages(&session.messages);
            let req = ApiRequest {
                model: self.config.model.clone(),
                max_tokens: self.config.max_tokens,
                system: system_prompt.to_string(),
                messages: api_messages,
                tools: tool_schemas.clone(),
                stream: true,
            };

            // Stream API response
            let (stream_tx, mut stream_rx) = mpsc::channel::<StreamEvent>(64);
            let stream_handle = tokio::spawn({
                let api = nova_api::client::ApiClient::new(
                    self.config.api_key.clone(),
                    self.config.api_base_url.clone(),
                );
                async move { api.stream(&req, stream_tx).await }
            });

            // Collect streamed response
            let mut text_content = String::new();
            let mut tool_calls: Vec<AccumulatedToolCall> = Vec::new();
            let mut current_tool: Option<AccumulatedToolCall> = None;
            let mut usage = Usage::default();
            let mut context_overflow = false;

            while let Some(event) = stream_rx.recv().await {
                match event {
                    StreamEvent::TextDelta(text) => {
                        text_content.push_str(&text);
                        let _ = event_tx.send(LoopEvent::TextDelta(text)).await;
                    }
                    StreamEvent::ToolUseStart { id, name } => {
                        let _ = event_tx.send(LoopEvent::ToolCallStart {
                            id: id.clone(), name: name.clone(),
                        }).await;
                        current_tool = Some(AccumulatedToolCall {
                            id, name, input_json: String::new(),
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
                    StreamEvent::Usage(u) => {
                        usage = u.clone();
                        let _ = event_tx.send(LoopEvent::TokenUsage {
                            input: u.input_tokens, output: u.output_tokens,
                        }).await;
                    }
                    StreamEvent::MessageStop { .. } => {}
                    StreamEvent::Error(e) => {
                        // GAP 2: detect context-overflow for auto compact+retry
                        let lower = e.to_lowercase();
                        if lower.contains("context_length") || lower.contains("too many tokens")
                            || lower.contains("max_tokens") || lower.contains("context window")
                        {
                            context_overflow = true;
                        }
                        let _ = event_tx.send(LoopEvent::Error(e)).await;
                    }
                }
            }

            // Check if the stream task itself errored
            if let Ok(Err(e)) = stream_handle.await {
                if text_content.is_empty() && tool_calls.is_empty() {
                    let _ = event_tx.send(LoopEvent::Error(format!("API stream error: {}", e))).await;
                    break;
                }
            }

            // If the stream produced nothing at all, retry automatically
            if text_content.is_empty() && tool_calls.is_empty() && !context_overflow && usage.input_tokens == 0 {
                empty_retries += 1;
                if empty_retries >= MAX_EMPTY_RETRIES {
                    let _ = event_tx.send(LoopEvent::Error(
                        format!("API returned empty response {} times — giving up", MAX_EMPTY_RETRIES),
                    )).await;
                    break;
                }
                // Undo turn increment and retry after a short delay
                session.turn_count = session.turn_count.saturating_sub(1);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
            // Got a real response — reset retry counter
            empty_retries = 0;

            // GAP 2: context-overflow → compact and retry this turn
            if context_overflow {
                let _ = event_tx.send(LoopEvent::CompactTriggered).await;
                match compactor.compact(&session.messages, self.config.context_window).await {
                    Ok(compacted) => {
                        session.messages = compacted;
                        // Undo turn increment so retry doesn't waste a turn
                        session.turn_count = session.turn_count.saturating_sub(1);
                        continue;
                    }
                    Err(e) => {
                        warn!("Context-overflow compact failed: {}", e);
                        break;
                    }
                }
            }

            // Update token stats
            session.token_stats.total_input_tokens += usage.input_tokens;
            session.token_stats.total_output_tokens += usage.output_tokens;

            // Post-flight budget check + record turn
            let input_tokens = usage.input_tokens as usize;
            match budget.check(input_tokens) {
                BudgetCheck::NeedsCompact => {
                    let _ = event_tx.send(LoopEvent::CompactTriggered).await;
                    match compactor.compact(&session.messages, self.config.context_window).await {
                        Ok(compacted) => session.messages = compacted,
                        Err(e) => warn!("Compact failed: {}", e),
                    }
                }
                BudgetCheck::Diminishing => {
                    let _ = event_tx.send(LoopEvent::Error(
                        "Token budget: diminishing returns, stopping".into(),
                    )).await;
                    break;
                }
                BudgetCheck::Ok => {}
            }
            budget.record_turn(input_tokens);

            // Record assistant message
            let msg_tool_calls = if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls.iter().map(|tc| ToolCall {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    arguments: tc.parse_input().unwrap_or_default(),
                }).collect())
            };
            let content = if text_content.is_empty() { None } else { Some(text_content) };
            let assistant_msg = Message::assistant(content, msg_tool_calls);

            // --- Strategy 5: PostSampling Hooks (async, non-blocking) ---
            self.hooks.fire_post_sampling(&assistant_msg, session).await;

            session.add_message(assistant_msg);

            // No tool calls → turn done
            if tool_calls.is_empty() {
                // --- Strategy 6: StopHooks (serial, blocking) ---
                self.hooks.fire_stop(session).await;
                let _ = event_tx.send(LoopEvent::TurnEnd).await;
                break;
            }

            // Execute each tool call with timeout
            for tc in &tool_calls {
                let input = tc.parse_input().unwrap_or_default();
                let result = match self.tools.execute(&tc.name, input, self.config.tool_timeout).await {
                    Ok(r) => r,
                    Err(e) => format!("{{\"error\": \"{}\"}}", e),
                };
                let _ = event_tx.send(LoopEvent::ToolCallResult {
                    id: tc.id.clone(), content: result.clone(),
                }).await;
                session.add_message(Message::tool_result(&tc.id, &result));
            }
        }

        Ok(())
    }
}

/// Convert session Messages to Anthropic API message format
fn build_api_messages(messages: &[Message]) -> Vec<ApiMessage> {
    let mut api_msgs = Vec::new();
    for msg in messages {
        match msg.role {
            Role::System => {}
            Role::User => {
                if let Some(ref content) = msg.content {
                    api_msgs.push(ApiMessage::User {
                        content: Content::Text(content.clone()),
                    });
                }
            }
            Role::Assistant => {
                let mut blocks = Vec::new();
                if let Some(ref text) = msg.content {
                    if !text.is_empty() {
                        blocks.push(ContentBlock::Text { text: text.clone() });
                    }
                }
                if let Some(ref tcs) = msg.tool_calls {
                    for tc in tcs {
                        blocks.push(ContentBlock::ToolUse {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            input: tc.arguments.clone(),
                        });
                    }
                }
                if !blocks.is_empty() {
                    api_msgs.push(ApiMessage::Assistant {
                        content: Content::Blocks(blocks),
                    });
                }
            }
            Role::Tool => {
                if let (Some(ref id), Some(ref content)) = (&msg.tool_call_id, &msg.content) {
                    api_msgs.push(ApiMessage::User {
                        content: Content::Blocks(vec![ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: content.clone(),
                        }]),
                    });
                }
            }
        }
    }
    api_msgs
}
