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
use nova_core::models::ShadowEvent;
use nova_memory::session::manager::Session;
use nova_memory::sidequery::SideQuery;
use crate::token::budget::{BudgetCheck, TokenBudget};
use crate::token::compact::Compactor;
use nova_tools::registry::{ToolRegistry, ToolContext};

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
    pub context_window: usize,
    pub budget_trigger_pct: f32,
    pub compact_target_pct: f32,
    /// Memories directory for diary writing (Layer 2 episodic memory)
    pub memories_dir: Option<std::path::PathBuf>,
    /// Optional pre-flight checker for state machine interception
    /// If None, pre-flight check is skipped (legacy mode)
    pub preflight_checker: Option<crate::preflight::PreFlightChecker>,
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
            preflight_checker: None,
        }
    }
}

/// The core Query Loop — strategies 1+2+3+5+6 integrated
pub struct QueryLoop {
    pub config: QueryLoopConfig,
    pub backend: Arc<dyn LlmBackend>,
    pub tools: Arc<ToolRegistry>,
    pub hooks: HookManager,
    pub daily_notes: Option<DailyNotes>,
    pub side_query: Option<SideQuery>,
    pub consolidator: Option<MemoryConsolidator>,
    // v2 Phase 1.5+ trackers
    pub topic_tracker: Option<std::sync::Arc<tokio::sync::RwLock<nova_memory::TopicTracker>>>,
    pub tension_tracker: Option<std::sync::Arc<nova_memory::TensionTracker>>,
    pub memory_board: Option<std::sync::Arc<tokio::sync::RwLock<nova_memory::MemoryBoard>>>,
    // [V4 Phase 4.1] Cached complexity from previous turn for tool filtering
    cached_complexity: tokio::sync::Mutex<Option<nova_core::preflight_types::Complexity>>,
    // [V4 Task 3.2] Optional channel for emitting ShadowEvent::TopicArchived
    #[allow(dead_code)]
    shadow_tx: Option<mpsc::Sender<ShadowEvent>>,
    // [R4] Optional shared skills loader for progressive disclosure
    pub skills_loader: Option<nova_tools::skills::cache::SharedSkillsLoader>,
    // Tool context for executing tools
    pub tool_context: ToolContext,
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
        topic_tracker: Option<std::sync::Arc<tokio::sync::RwLock<nova_memory::TopicTracker>>>,
        tension_tracker: Option<std::sync::Arc<nova_memory::TensionTracker>>,
        memory_board: Option<std::sync::Arc<tokio::sync::RwLock<nova_memory::MemoryBoard>>>,
        shadow_tx: Option<mpsc::Sender<ShadowEvent>>,
        skills_loader: Option<nova_tools::skills::cache::SharedSkillsLoader>,
        tool_context: ToolContext,
    ) -> Self {
        Self {
            config, backend, tools, hooks, daily_notes, side_query, consolidator,
            topic_tracker, tension_tracker, memory_board,
            cached_complexity: tokio::sync::Mutex::new(None),
            shadow_tx,
            skills_loader,
            tool_context,
        }
    }

    /// Run a full query loop turn for user input.
    pub async fn run_turn(
        &self,
        mut session: Session,
        system_prompt: &str,
        event_tx: mpsc::Sender<LoopEvent>,
    ) -> Result<(Session, Vec<Message>, Option<nova_core::preflight_types::PreFlightCheckResult>)> {
        // Reset turn counter
        session.reset_turns();
        let mut newly_added_messages = Vec::new();
        info!("--- Starting new query loop for user input ---");

        // ── TurnPipeline: 所有策略通过共享 TurnContext 协作 ─────────────
        let preflight_user_input = session.messages.last()
            .and_then(|m| m.content.as_ref().cloned())
            .unwrap_or_default();
        let preflight_recent_msgs = session.messages.iter().rev().take(10).cloned().collect::<Vec<_>>();

        let mut turn_ctx = nova_core::pipeline::TurnContext::new(
            preflight_user_input,
            preflight_recent_msgs,
        );

        // Build and run the pipeline
        let pipeline = self.build_pipeline();
        if let Err(e) = pipeline.run(&mut turn_ctx).await {
            warn!("TurnPipeline error: {}, falling back to defaults", e);
        }

        // Read pipeline results
        let preflight_result = turn_ctx.preflight_result.clone();

        // Generate tool schemas from GateStage result
        let tool_schemas = if let Some(ref allowed) = turn_ctx.allowed_tools {
            let allowed = allowed.clone();
            self.tools.as_api_schemas_filtered(|name| allowed.contains(&name.to_string()))
        } else {
            self.tools.as_api_schemas()
        };

        // Build injection string from InjectStage result
        let injection_string = turn_ctx.build_injection_string();

        // Cache complexity for next turn fallback
        if let Some(ref result) = preflight_result {
            *self.cached_complexity.lock().await = Some(result.complexity);
        }

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

            // --- Strategy 2: Pre-flight budget check ---
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
            let effective_system = format!("{}{}", system_prompt, injection_string);

            // Convert nova_llm ToolSchema to nova_core ToolSchema
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
                system: effective_system,
                messages: completion_messages,
                tools: core_tool_schemas,
                stream: true,
            };

            // Stream API response via LlmBackend trait
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

            // Check if the stream task itself errored
            if let Ok(Err(e)) = stream_handle.await {
                error!("API stream fatal error: {}", e);
                if text_content.is_empty() && tool_calls.is_empty() {
                    let _ = event_tx.send(LoopEvent::Error(format!("API stream error: {}", e))).await;
                    break;
                }
            }

            debug!("API response: text_content_len={}, tool_calls={}, usage=input:{} output:{}",
                text_content.len(), tool_calls.len(), usage.input_tokens, usage.output_tokens);

            // If the stream produced nothing at all, retry automatically
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

            // GAP 2: context-overflow → compact and retry this turn
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
                            "Context size exceeded model limits, but cannot safely compact further because the most recent messages are too large! Please type /new to start a fresh session.".into()
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

            // Post-flight budget check + record turn
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

            // --- Strategy 5: PostSampling Hooks (async, non-blocking) ---
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

            // Execute each tool call with timeout
            for tc in &tool_calls {
                // ── v2 Phase 1.5: Reset topic inactive turns on tool use ──────
                if let Some(ref tt) = self.topic_tracker {
                    tt.write().await.on_tool_call().await;
                }

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

                // [V6] Hard gate 校验
                let is_allowed = tool_schemas.iter().any(|s| s.name == tc.name);
                
                let mut result = if !is_allowed {
                    let allowed_names: Vec<&str> = tool_schemas.iter().map(|s| s.name.as_str()).collect();
                    let msg = format!("[V6 Hard Gate] Tool `{}` is blocked by current complexity gate. Only allowed tools: {:?}", tc.name, allowed_names);
                    warn!("{}", msg);
                    format!("{{\"error\": \"{}\"}}", msg)
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

                // Tool-Level Output Truncation
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

            // Pipeline ExecuteConfig: 委派工具执行后立即结束循环
            let delegated = turn_ctx.should_terminate_after_tool && tool_calls.iter().any(|tc| {
                tc.name == "delegate_task" || tc.name == "delegate_complex_project"
            });
            if delegated {
                if text_len == 0 {
                    let fallback = "好的，任务已派发给后台处理，完成后会自动通知您。";
                    let _ = event_tx.send(LoopEvent::TextDelta(fallback.to_string())).await;
                }
                info!("Pipeline: delegation tool executed, ending query loop");
                self.hooks.fire_stop(&mut session).await;
                let _ = event_tx.send(LoopEvent::TurnEnd).await;
                break;
            }
        }

        Ok((session, newly_added_messages, preflight_result))
    }

    /// Build a TurnPipeline from QueryLoop's optional components.
    fn build_pipeline(&self) -> nova_core::pipeline::TurnPipeline {
        use crate::stages::*;
        let mut pipeline = nova_core::pipeline::TurnPipeline::new();

        // Stage 1: Classify
        if let Some(ref checker) = self.config.preflight_checker {
            pipeline.add_stage(Box::new(classify::ClassifyStage::new(
                std::sync::Arc::new(checker.clone()),
                30,
            )));
        }

        // Stage 2: Track
        if let (Some(ref tt), Some(ref tens)) = (&self.topic_tracker, &self.tension_tracker) {
            pipeline.add_stage(Box::new(track::TrackStage::new(
                tt.clone(),
                tens.clone(),
            )));
        }

        // Stage 3: Gate
        pipeline.add_stage(Box::new(gate::GateStage::new()));

        // Stage 4: Inject
        pipeline.add_stage(Box::new(inject::InjectStage::new(
            self.config.memories_dir.clone(),
            self.skills_loader.clone(),
        )));

        // Stage 5: PlatformHint
        pipeline.add_stage(Box::new(platform_hint::PlatformHintStage::new()));

        // Stage 6: ExecuteConfig
        pipeline.add_stage(Box::new(execute_config::ExecuteConfigStage::new()));

        pipeline
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
                if let Some(ref content) = msg.content {
                    msgs.push(CompletionMessage::User {
                        content: CompletionContent::Text(content.clone()),
                    });
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
