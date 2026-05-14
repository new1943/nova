use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{warn, info, debug, error};

use nova_core::llm_backend::{
    CompletionContent, CompletionMessage, CompletionRequest, LlmBackend, StreamDelta, TokenUsage,
    ContentBlock as CoreContentBlock, ToolSchema as CoreToolSchema,
};

use crate::hooks::HookManager;
use nova_memory::memory::MemoryConsolidator;
use nova_memory::memory::daily::DailyNotes;
use nova_core::message::{Message, Role, ToolCall};
use nova_memory::session::manager::Session;
use nova_memory::sidequery::SideQuery;
use crate::token::budget::{BudgetCheck, TokenBudget};
use crate::token::compact::Compactor;
use nova_tools::registry::{ToolRegistry, ToolContext};

/// Event emitted by the query loop to the caller (TUI/daemon)
#[derive(Debug, Clone)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

#[derive(Debug)]
pub enum LoopEvent {
    TextDelta(String),
    ToolCallStart { id: String, name: String },
    ToolCallResult { id: String, content: String },
    TurnEnd,
    TokenUsage { input: u32, output: u32 },
    Error(String),
    CompactTriggered,
    FileOutput { path: String, filename: String },
    Embed {
        title: Option<String>,
        description: Option<String>,
        color: Option<u32>,
        fields: Vec<EmbedField>,
        image_url: Option<String>,
        footer: Option<String>,
    },
}

/// Query Loop configuration
#[derive(Clone)]
pub struct QueryLoopConfig {
    pub max_turns: usize,
    pub tool_timeout: Duration,
    pub model: String,
    pub max_tokens: u32,
    pub context_window: usize,
    pub budget_trigger_pct: f32,
    pub compact_target_pct: f32,
    /// Memories directory for diary writing (Layer 2 episodic memory)
    pub memories_dir: Option<std::path::PathBuf>,
}

impl Default for QueryLoopConfig {
    fn default() -> Self {
        Self {
            max_turns: 40,
            tool_timeout: Duration::from_secs(60),
            model: "MiniMax-M2.7".into(),
            max_tokens: 8192,
            context_window: 200_000,
            budget_trigger_pct: 0.9,
            compact_target_pct: 0.6,
            memories_dir: None,
        }
    }
}

/// The core Query Loop — v3 architecture (Main Loop + Tool Registry)
///
/// No Preflight. No Pipeline Stages. LLM sees all tools and decides what to do.
pub struct QueryLoop {
    pub config: QueryLoopConfig,
    pub backend: Arc<dyn LlmBackend>,
    pub tools: Arc<ToolRegistry>,
    pub hooks: HookManager,
    pub daily_notes: Option<DailyNotes>,
    pub side_query: Option<SideQuery>,
    pub consolidator: Option<MemoryConsolidator>,
    // Optional channel for emitting notifications (used by daemon)
    pub notify_tx: Option<mpsc::Sender<nova_core::executor::types::Notification>>,
    // Tool context for executing tools
    pub tool_context: ToolContext,
    // Optional approval handler for tool execution gating
    pub approval_handler: Option<Arc<dyn nova_core::approval::ApprovalHandler>>,
}

impl QueryLoop {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        backend: Arc<dyn LlmBackend>,
        tools: Arc<ToolRegistry>,
        hooks: HookManager,
        config: QueryLoopConfig,
        daily_notes: Option<DailyNotes>,
        side_query: Option<SideQuery>,
        consolidator: Option<MemoryConsolidator>,
        notify_tx: Option<mpsc::Sender<nova_core::executor::types::Notification>>,
        tool_context: ToolContext,
    ) -> Self {
        Self {
            config, backend, tools, hooks, daily_notes, side_query, consolidator,
            notify_tx,
            tool_context,
            approval_handler: None,
        }
    }

    pub fn with_approval_handler(mut self, handler: Arc<dyn nova_core::approval::ApprovalHandler>) -> Self {
        self.approval_handler = Some(handler);
        self
    }

    /// Run a full query loop turn for user input.
    ///
    /// v3 architecture: no Pipeline, no Preflight. LLM sees all tools and decides.
    pub async fn run_turn(
        &self,
        mut session: Session,
        system_prompt: &str,
        event_tx: mpsc::Sender<LoopEvent>,
    ) -> Result<(Session, Vec<Message>)> {
        session.reset_turns();
        let mut newly_added_messages = Vec::new();
        info!("--- Starting new query loop for user input ---");

        // v3: All tools are always visible to the LLM
        let tool_schemas = self.tools.as_api_schemas();

        let mut budget = TokenBudget::new(
            self.config.context_window,
            self.config.budget_trigger_pct,
        );
        let compactor = Compactor::new(
            self.config.compact_target_pct,
        );
        let mut empty_retries: u32 = 0;
        let mut compaction_exhausted = false;
        const MAX_EMPTY_RETRIES: u32 = 3;

        loop {
            if !session.increment_turn() {
                let _ = event_tx.send(LoopEvent::Error(
                    format!("Max turns ({}) reached", self.config.max_turns),
                )).await;
                break;
            }

            // Pre-flight budget check
            let estimated_tokens = estimate_message_tokens(&session.messages);
            let budget_pct = estimated_tokens as f32 / self.config.context_window as f32;
            debug!("Pre-flight budget: estimated_tokens={}, budget_pct={:.1}%, compact_threshold={:.1}%",
                estimated_tokens, budget_pct * 100.0, self.config.budget_trigger_pct * 100.0);
            if estimated_tokens > 0 && budget.needs_compact(estimated_tokens) {
                let _ = event_tx.send(LoopEvent::CompactTriggered).await;
                self.run_consolidation(&mut session).await;
                self.write_compact_diary(&session.messages).await;
                match compactor.compact_full(&session.messages, self.config.context_window, budget_pct).await {
                    Ok((compacted, _result_opt)) => {
                        debug!("Precompact triggered: messages {} -> {}", session.messages.len(), compacted.len());
                        session.messages = compacted;
                    }
                    Err(e) => warn!("Pre-flight compact failed: {}", e),
                }
            }

            // Build request
            let completion_messages = build_completion_messages(&session.messages);
            info!("Sending API request to {} ({} messages, estimated {} tokens)", self.config.model, completion_messages.len(), estimated_tokens);

            let core_tool_schemas: Vec<CoreToolSchema> = tool_schemas.iter().map(|ts| {
                CoreToolSchema {
                    name: ts.name.clone(),
                    description: ts.description.clone(),
                    input_schema: ts.input_schema.clone(),
                }
            }).collect();

            let req = CompletionRequest {
                model: self.config.model.clone(),
                max_tokens: self.config.max_tokens,
                system: system_prompt.to_string(),
                messages: completion_messages,
                tools: core_tool_schemas,
                stream: true,
            };

            // Stream API response
            let (stream_tx, mut stream_rx) = mpsc::channel::<StreamDelta>(64);
            let stream_handle = tokio::spawn({
                let backend = self.backend.clone();
                let req = req.clone();
                async move { backend.stream(&req, stream_tx).await }
            });

            // Collect streamed response
            let mut text_content = String::new();
            let mut tool_calls: Vec<AccumulatedToolCall> = Vec::new();
            let mut current_tool: Option<AccumulatedToolCall> = None;
            let mut usage = TokenUsage::default();
            let mut context_overflow = false;

            while let Some(delta) = stream_rx.recv().await {
                match delta {
                    StreamDelta::TextDelta(text) => {
                        text_content.push_str(&text);
                        let _ = event_tx.send(LoopEvent::TextDelta(text)).await;
                    }
                    StreamDelta::ToolUseStart { id, name } => {
                        let _ = event_tx.send(LoopEvent::ToolCallStart {
                            id: id.clone(), name: name.clone(),
                        }).await;
                        current_tool = Some(AccumulatedToolCall {
                            id, name, input_json: String::new(),
                        });
                    }
                    StreamDelta::ToolInputDelta(json) => {
                        if let Some(ref mut tool) = current_tool {
                            tool.input_json.push_str(&json);
                        }
                    }
                    StreamDelta::ToolUseEnd { .. } => {
                        if let Some(tool) = current_tool.take() {
                            tool_calls.push(tool);
                        }
                    }
                    StreamDelta::Usage(u) => {
                        let _ = event_tx.send(LoopEvent::TokenUsage {
                            input: u.input_tokens, output: u.output_tokens,
                        }).await;
                        usage = u;
                    }
                    StreamDelta::MessageStop { .. } => {}
                    StreamDelta::Error(e) => {
                        error!("API stream error: {}", e);
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

            if let Ok(Err(e)) = stream_handle.await {
                error!("API stream fatal error: {}", e);
                if text_content.is_empty() && tool_calls.is_empty() {
                    let _ = event_tx.send(LoopEvent::Error(format!("API stream error: {}", e))).await;
                    break;
                }
            }

            debug!("API response: text_content_len={}, tool_calls={}, usage=input:{} output:{}",
                text_content.len(), tool_calls.len(), usage.input_tokens, usage.output_tokens);

            // Empty response retry
            if text_content.is_empty() && tool_calls.is_empty() && !context_overflow && usage.input_tokens == 0 {
                warn!("Empty API response, retry={}/{}", empty_retries, MAX_EMPTY_RETRIES);
                empty_retries += 1;
                if empty_retries >= MAX_EMPTY_RETRIES {
                    let _ = event_tx.send(LoopEvent::Error(
                        format!("API returned empty response {} times — giving up", MAX_EMPTY_RETRIES),
                    )).await;
                    break;
                }
                session.turn_count = session.turn_count.saturating_sub(1);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
            empty_retries = 0;

            // Context overflow → compact and retry
            if context_overflow {
                warn!("Context overflow detected, attempting compact...");
                let _ = event_tx.send(LoopEvent::CompactTriggered).await;
                self.write_compact_diary(&session.messages).await;
                match compactor.compact_full(&session.messages, self.config.context_window, 0.96).await {
                    Ok((compacted, _result_opt)) if compacted.len() < session.messages.len() => {
                        debug!("Context overflow compact: messages {} -> {}", session.messages.len(), compacted.len());
                        session.messages = compacted;
                        session.turn_count = session.turn_count.saturating_sub(1);
                        continue;
                    }
                    Ok(_) => {
                        let _ = event_tx.send(LoopEvent::Error(
                            "Context size exceeded model limits. Please type /new to start a fresh session.".into()
                        )).await;
                        break;
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

            // Post-flight budget check
            let input_tokens = usage.input_tokens as usize;
            let budget_pct = input_tokens as f32 / self.config.context_window as f32;
            debug!("Post-flight budget: input_tokens={}, budget_pct={:.1}%, check={:?}",
                input_tokens, budget_pct * 100.0, budget.check(input_tokens));
            match budget.check(input_tokens) {
                BudgetCheck::NeedsCompact if !compaction_exhausted => {
                    debug!("Post-flight NeedsCompact triggered");
                    let _ = event_tx.send(LoopEvent::CompactTriggered).await;
                    self.run_consolidation(&mut session).await;
                    self.write_compact_diary(&session.messages).await;
                    match compactor.compact_full(&session.messages, self.config.context_window, budget_pct).await {
                        Ok((compacted, _result_opt)) => {
                            debug!("Post-flight compact: messages {} -> {}", session.messages.len(), compacted.len());
                            if compacted.len() >= session.messages.len() {
                                compaction_exhausted = true;
                                let _ = event_tx.send(LoopEvent::Error(
                                    "[Warning] Auto-compaction cannot reduce size further. Approaching hard limits!".into()
                                )).await;
                            }
                            session.messages = compacted;
                        }
                        Err(e) => warn!("Compact failed: {}", e),
                    }
                }
                BudgetCheck::NeedsCompact => {}
                BudgetCheck::Diminishing => {
                    warn!("Budget diminishing returns, breaking loop");
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
                    arguments: tc.parse_input().unwrap_or_else(|_| serde_json::json!({})),
                }).collect())
            };
            let text_len = text_content.len();
            let content = if text_content.is_empty() { None } else { Some(text_content) };
            let assistant_msg = Message::assistant(content, msg_tool_calls);

            // PostSampling Hooks
            self.hooks.fire_post_sampling(&assistant_msg, &session).await;

            newly_added_messages.push(assistant_msg.clone());
            session.add_message(assistant_msg);

            // No tool calls → turn done
            if tool_calls.is_empty() {
                debug!("Loop exit: no tool_calls, text_content_len={}", text_len);
                self.hooks.fire_stop(&mut session).await;
                let _ = event_tx.send(LoopEvent::TurnEnd).await;
                break;
            }

            // Execute each tool call
            for tc in &tool_calls {
                let input = tc.parse_input().unwrap_or_else(|_| serde_json::json!({}));
                let input_str = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                let input_preview = if input_str.len() > 200 {
                    let mut byte_index = 0;
                    for (char_count, (i, _)) in input_str.char_indices().enumerate() {
                        if char_count == 200 {
                            byte_index = i;
                            break;
                        }
                    }
                    if byte_index > 0 {
                        format!("{}...[truncated {} chars]", &input_str[..byte_index], input_str.len() - byte_index)
                    } else {
                        input_str.clone()
                    }
                } else {
                    input_str
                };
                debug!("Tool call: name={}, args={}", tc.name, input_preview);

                // Check if tool is allowed
                let is_allowed = tool_schemas.iter().any(|s| s.name == tc.name);

                // Approval check for sensitive tools
                let approval_denied = if is_allowed {
                    if let Some(ref handler) = self.approval_handler {
                        if nova_core::approval::requires_approval(&tc.name) {
                            let decision = handler.request_approval(&tc.name, &input).await;
                            match decision {
                                nova_core::approval::ApprovalDecision::Allow => false,
                                nova_core::approval::ApprovalDecision::AllowSession => false,
                                nova_core::approval::ApprovalDecision::Deny => true,
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };

                let mut result = if !is_allowed {
                    let allowed_names: Vec<&str> = tool_schemas.iter().map(|s| s.name.as_str()).collect();
                    let msg = format!("Tool `{}` is not available. Available tools: {:?}", tc.name, allowed_names);
                    warn!("{}", msg);
                    format!("{{\"error\": \"{}\"}}", msg)
                } else if approval_denied {
                    warn!("Tool `{}` denied by user approval", tc.name);
                    format!("{{\"error\": \"Tool '{}' was denied by user\"}}", tc.name)
                } else {
                    match self.tools.execute(&tc.name, input.clone(), &self.tool_context, self.config.tool_timeout).await {
                        Ok(r) => {
                            debug!("Tool result: name={}, result_len={}", tc.name, r.len());
                            r
                        }
                        Err(e) => {
                            error!("Tool `{}` failed: {}", tc.name, e);
                            format!("{{\"error\": \"{}\"}}", e)
                        }
                    }
                };

                // Tool output truncation
                const MAX_TOOL_CHARS: usize = 30000;
                if result.len() > MAX_TOOL_CHARS {
                    let omitted = result.len() - MAX_TOOL_CHARS;
                    let mut byte_index = 0;
                    for (char_count, (i, _)) in result.char_indices().enumerate() {
                        if char_count == MAX_TOOL_CHARS {
                            byte_index = i;
                            break;
                        }
                    }
                    if byte_index > 0 {
                        result.truncate(byte_index);
                        result.push_str(&format!("\n\n... [OUTPUT TRUNCATED - {omitted} chars omitted! Result is too massive. The response has been forcefully truncated to protect token limits.]"));
                    }
                }
                let _ = event_tx.send(LoopEvent::ToolCallResult {
                    id: tc.id.clone(), content: result.clone(),
                }).await;
                let tool_msg = Message::tool_result(&tc.id, &result);
                newly_added_messages.push(tool_msg.clone());
                session.add_message(tool_msg);
            }
        }

        Ok((session, newly_added_messages))
    }

    async fn write_compact_diary(&self, messages: &[Message]) {
        let daily = match &self.daily_notes {
            Some(d) => d,
            None => return,
        };
        let sq = match &self.side_query {
            Some(s) => s,
            None => return,
        };

        let recent: Vec<String> = messages.iter()
            .rev()
            .take(20)
            .filter_map(|m| {
                let role = match m.role {
                    Role::User => "User",
                    Role::Assistant => "Assistant",
                    Role::Tool => "Tool",
                    Role::System => return None,
                };
                let content = m.content.as_deref().unwrap_or("");
                if content.is_empty() {
                    None
                } else {
                    Some(format!("{}: {}", role, content))
                }
            })
            .collect();

        if recent.len() < 3 {
            return;
        }

        let conversation = recent.join("\n");
        let system = "Summarize the following conversation in 1-3 concise Chinese sentences. \
    Focus on: key decisions, important findings, user preferences mentioned. \
    Output only the summary, no labels.";

        let prompt = format!("Conversation:\n{}", conversation);

        match sq.query_await(system, &prompt).await {
            Ok(summary) if !summary.trim().is_empty() => {
                if let Err(e) = daily.append_compact(&summary) {
                    warn!("Failed to write compact diary: {}", e);
                } else {
                    tracing::info!("Compact diary written: {} chars", summary.len());
                }
            }
            Ok(_) => {}
            Err(e) => warn!("Compact diary summary failed: {}", e),
        }
    }

    async fn run_consolidation(&self, session: &mut Session) {
        let consolidator = match &self.consolidator {
            Some(c) => c,
            None => return,
        };

        match consolidator.consolidate(
            &session.messages,
            session.last_memory_sweep_index,
            session.memory_updated_mutex,
        ).await {
            Ok(_updated) => {
                session.last_memory_sweep_index = session.messages.len();
                session.memory_updated_mutex = false;
            }
            Err(e) => warn!("T23: Memory consolidation failed: {}", e),
        }
    }
}

/// Accumulated tool call from streaming
#[derive(Debug, Clone)]
pub struct AccumulatedToolCall {
    pub id: String,
    pub name: String,
    pub input_json: String,
}

impl AccumulatedToolCall {
    pub fn parse_input(&self) -> anyhow::Result<serde_json::Value> {
        Ok(serde_json::from_str(&self.input_json)?)
    }
}

/// Count the token count of current messages using tiktoken (cl100k_base encoding).
fn estimate_message_tokens(messages: &[Message]) -> usize {
    use crate::token::counter::count_tokens;

    let mut total_tokens = 0;
    for m in messages.iter() {
        if let Some(ref content) = m.content {
            total_tokens += count_tokens(content);
        }
        if let Some(ref tool_calls) = m.tool_calls {
            for tc in tool_calls.iter() {
                total_tokens += count_tokens(&tc.name);
                total_tokens += count_tokens(&tc.arguments.to_string());
            }
        }
    }
    total_tokens
}

/// Convert session Messages to CompletionMessage format (provider-agnostic)
fn build_completion_messages(messages: &[Message]) -> Vec<CompletionMessage> {
    let mut msgs = Vec::new();
    for msg in messages {
        match msg.role {
            Role::System => {}
            Role::User => {
                if msg.attachments.is_empty() {
                    if let Some(ref content) = msg.content {
                        msgs.push(CompletionMessage::User {
                            content: CompletionContent::Text(content.clone()),
                        });
                    }
                } else {
                    let mut blocks = Vec::new();
                    if let Some(ref content) = msg.content {
                        if !content.is_empty() {
                            blocks.push(CoreContentBlock::Text { text: content.clone() });
                        }
                    }
                    for att in &msg.attachments {
                        if att.media_type.starts_with("image/") {
                            use base64::Engine as _;
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&att.data);
                            blocks.push(CoreContentBlock::Image {
                                source: nova_core::llm_backend::ImageSource {
                                    source_type: "base64".into(),
                                    media_type: att.media_type.clone(),
                                    data: b64,
                                },
                            });
                        } else {
                            blocks.push(CoreContentBlock::Text {
                                text: format!("[Attached file: {} ({} bytes, {})]", att.filename, att.data.len(), att.media_type),
                            });
                        }
                    }
                    if !blocks.is_empty() {
                        msgs.push(CompletionMessage::User {
                            content: CompletionContent::Blocks(blocks),
                        });
                    }
                }
            }
            Role::Assistant => {
                let mut blocks = Vec::new();
                if let Some(ref text) = msg.content {
                    if !text.is_empty() {
                        blocks.push(CoreContentBlock::Text { text: text.clone() });
                    }
                }
                if let Some(ref tcs) = msg.tool_calls {
                    for tc in tcs {
                        let input = if tc.arguments.is_null() {
                            serde_json::json!({})
                        } else {
                            tc.arguments.clone()
                        };
                        blocks.push(CoreContentBlock::ToolUse {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            input,
                        });
                    }
                }
                if !blocks.is_empty() {
                    msgs.push(CompletionMessage::Assistant {
                        content: CompletionContent::Blocks(blocks),
                    });
                }
            }
            Role::Tool => {
                if let (Some(ref id), Some(ref content)) = (&msg.tool_call_id, &msg.content) {
                    msgs.push(CompletionMessage::User {
                        content: CompletionContent::Blocks(vec![CoreContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: content.clone(),
                        }]),
                    });
                }
            }
        }
    }
    msgs
}
