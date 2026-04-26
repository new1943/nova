use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{warn, info, debug, error};

use nova_api::stream::{AccumulatedToolCall, StreamEvent};
use nova_api::types::{
    ApiMessage, ApiRequest, Content, ContentBlock, Usage,
};

use crate::hooks::HookManager;
use crate::memory::consolidate::MemoryConsolidator;
use crate::memory::daily::DailyNotes;
use crate::message::{Message, Role, ToolCall};
use crate::models::ShadowEvent;
use crate::session::manager::Session;
use crate::sidequery::SideQuery;
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
    /// Memories directory for diary writing (Layer 2 episodic memory)
    pub memories_dir: Option<std::path::PathBuf>,
    /// Optional pre-flight checker for state machine interception
    /// If None, pre-flight check is skipped (legacy mode)
    pub preflight_checker: Option<crate::agent::preflight::PreFlightChecker>,
}

impl Default for QueryLoopConfig {
    fn default() -> Self {
        Self {
            max_turns: 40,
            tool_timeout: Duration::from_secs(60),
            model: "MiniMax-M2.7".into(),
            max_tokens: 8192,
            api_key: String::new(),
            api_base_url: "https://api.minimaxi.com/anthropic".into(),
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
    pub tools: Arc<ToolRegistry>,
    pub hooks: HookManager,
    pub daily_notes: Option<DailyNotes>,
    pub side_query: Option<SideQuery>,
    pub consolidator: Option<MemoryConsolidator>,
    // v2 Phase 1.5+ trackers
    pub topic_tracker: Option<std::sync::Arc<tokio::sync::RwLock<crate::memory::TopicTracker>>>,
    pub tension_tracker: Option<std::sync::Arc<crate::memory::TensionTracker>>,
    // [V4] ModeRouter disabled — nova_os deprecated
    // pub mode_router: Option<std::sync::Arc<tokio::sync::RwLock<crate::memory::ModeRouter>>>,
    pub memory_board: Option<std::sync::Arc<tokio::sync::RwLock<crate::memory::MemoryBoard>>>,
    // [V4 Phase 4.1] Cached complexity from previous turn for tool filtering
    // Use tokio::sync::Mutex (Send-safe, async-aware)
    cached_complexity: tokio::sync::Mutex<Option<crate::agent::preflight::Complexity>>,
    // [V4 Task 3.2] Optional channel for emitting ShadowEvent::TopicArchived
    shadow_tx: Option<mpsc::Sender<ShadowEvent>>,
}

impl QueryLoop {
    pub fn new(
        tools: Arc<ToolRegistry>,
        hooks: HookManager,
        config: QueryLoopConfig,
        daily_notes: Option<DailyNotes>,
        side_query: Option<SideQuery>,
        consolidator: Option<MemoryConsolidator>,
        topic_tracker: Option<std::sync::Arc<tokio::sync::RwLock<crate::memory::TopicTracker>>>,
        tension_tracker: Option<std::sync::Arc<crate::memory::TensionTracker>>,
        // [V4] ModeRouter disabled
        // mode_router: Option<std::sync::Arc<tokio::sync::RwLock<crate::memory::ModeRouter>>>,
        memory_board: Option<std::sync::Arc<tokio::sync::RwLock<crate::memory::MemoryBoard>>>,
        shadow_tx: Option<mpsc::Sender<ShadowEvent>>,
    ) -> Self {
        Self {
            config, tools, hooks, daily_notes, side_query, consolidator,
            topic_tracker, tension_tracker, /* mode_router, */ memory_board,
            cached_complexity: tokio::sync::Mutex::new(None),
            shadow_tx,
        }
    }

    /// Run a full query loop turn for user input.
    /// Returns (session, new_messages, preflight_result) where preflight_result
    /// is the classification for THIS turn's input — caller should store it
    /// and emit ShadowEvent::TopicArchived at the START of the NEXT turn.
    pub async fn run_turn(
        &self,
        mut session: Session,
        system_prompt: &str,
        event_tx: mpsc::Sender<LoopEvent>,
    ) -> Result<(Session, Vec<Message>, Option<crate::agent::preflight::PreFlightCheckResult>)> {
        // Reset turn counter — max_turns limits loop depth per user message, not per session
        session.reset_turns();
        let mut newly_added_messages = Vec::new();
        info!("--- Starting new query loop for user input ---");

        // ── [V4 Phase 3.1] Pre-flight check — blocking wait, result used immediately ─
        // Extract user input from the last message for pre-flight classification
        let preflight_user_input = session.messages.last()
            .and_then(|m| m.content.as_ref().cloned())
            .unwrap_or_default();
        let preflight_recent_msgs = session.messages.iter().rev().take(10).cloned().collect::<Vec<_>>();

        // [V4 Fix] Block on preflight result (30s timeout), use it immediately for tool filtering
        let preflight_result = if let Some(ref checker) = self.config.preflight_checker {
            let result = tokio::time::timeout(
                Duration::from_secs(30),
                checker.check(&preflight_user_input, &preflight_recent_msgs),
            ).await.unwrap_or_else(|_| crate::agent::preflight::PreFlightCheckResult::default());
            info!("[V4] Preflight: complexity={:?}, topic_shift={}, reason={}", result.complexity, result.topic_shift, result.reason);
            info!("[V4] Preflight thinking: {}", result.thinking);
            Some(result)
        } else {
            None
        };

        // ── v2 Phase 1.5: Topic/Tension/Mode tracking ──────────────────────────
        // Process user message through trackers to detect topic switches, update tension, and mode
        if let Some(last_msg) = session.messages.last() {
            if let Some(ref content) = last_msg.content {
                if last_msg.role == crate::message::Role::User {
                    // TopicTracker: detect topic transitions from signal words
                    if let Some(ref tt) = self.topic_tracker {
                        let transition = tt.write().await.on_user_message(content).await;
                        if matches!(transition, crate::memory::TopicTransition::Archive) {
                            info!("Topic archived via user signal");
                        } else if matches!(transition, crate::memory::TopicTransition::NewTopic) {
                            info!("New topic started via user signal");
                        }
                    }
                    // TensionTracker: update emotional state from message
                    if let Some(ref tt) = self.tension_tracker {
                        tt.update_from_message(content).await;
                    }
                    // [V4] ModeRouter disabled — nova_os deprecated
                    // if let Some(ref mr) = self.mode_router {
                    //     let mode = mr.write().await.process(content).await;
                    //     info!("Mode: {:?}", mode);
                    // }
                }
            }
        }

        // [V6] Hard Gate: 根据 complexity 物理隔离工具，LLM 无法绕过
        use crate::agent::preflight::Complexity;
        let tool_schemas = match preflight_result.as_ref().map(|r| r.complexity) {
            Some(Complexity::High) => {
                info!("[V6] Hard gate: High → only delegate_complex_project + cancel");
                self.tools.as_api_schemas_filtered(|name| {
                    name == "delegate_complex_project" || name == "cancel_delegated_project"
                })
            }
            Some(Complexity::Medium) => {
                info!("[V6] Hard gate: Medium → only delegate_task + cancel");
                self.tools.as_api_schemas_filtered(|name| {
                    name == "delegate_task" || name == "cancel_delegated_project"
                })
            }
            _ => {
                // Low 或无 preflight 结果: 所有工具可见
                self.tools.as_api_schemas()
            }
        };

        // [V5] 注入 <preflight> 标签到 system prompt
        let preflight_injection = if let Some(ref result) = preflight_result {
            format!(
                "\n\n<preflight>\ncomplexity: {:?}\ntopic_shift: {}\nreason: {}\n</preflight>",
                result.complexity, result.topic_shift, result.reason
            )
        } else {
            String::new()
        };
        // Cache for next turn (fallback if preflight doesn't complete in time)
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
            // Estimate current context size from message content rather than cumulative
            // token counter (which only grows). This avoids re-triggering compact every
            // turn after the first compaction since total_input_tokens is never reset.
            let estimated_tokens = estimate_message_tokens(&session.messages);
            let budget_pct = estimated_tokens as f32 / self.config.context_window as f32;
            debug!("Pre-flight budget: estimated_tokens={}, budget_pct={:.1}%, compact_threshold={:.1}%",
                estimated_tokens, budget_pct * 100.0, self.config.budget_trigger_pct * 100.0);
            if estimated_tokens > 0 && budget.needs_compact(estimated_tokens) {
                let _ = event_tx.send(LoopEvent::CompactTriggered).await;
                // T23: Run memory consolidation before compacting (space-triggered)
                self.run_consolidation(&mut session).await;
                // T21.1: Write diary before compacting (messages will be summarized)
                self.write_compact_diary(&session.messages).await;
                // ── v2 Phase 1.5: Compact with structured result ─────────────
                match compactor.compact_full(&session.messages, self.config.context_window, budget_pct).await {
                    Ok((compacted, _result_opt)) => {
                        debug!("Precompact triggered: messages {} -> {}", session.messages.len(), compacted.len());
                        session.messages = compacted;
                        // [V4 DEPRECATED] Memory extraction moved to MemoryKeeper via ShadowEvent bus
                    }
                    Err(e) => warn!("Pre-flight compact failed: {}", e),
                }
            }

            // Build request
            let api_messages = build_api_messages(&session.messages);
            info!("Sending API request to {} ({} messages, estimated {} tokens)", self.config.model, api_messages.len(), estimated_tokens);
            // [V6] 注入 <preflight> 标签 + hard gate 工具过滤
            let effective_system = format!("{}{}", system_prompt, preflight_injection);

            let req = ApiRequest {
                model: self.config.model.clone(),
                max_tokens: self.config.max_tokens,
                system: effective_system,
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
                        error!("API stream error: {}", e);
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
                // Undo turn increment and retry after a short delay
                session.turn_count = session.turn_count.saturating_sub(1);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
            // Got a real response — reset retry counter
            empty_retries = 0;

            // GAP 2: context-overflow → compact and retry this turn
            if context_overflow {
                warn!("Context overflow detected, attempting compact...");
                let _ = event_tx.send(LoopEvent::CompactTriggered).await;
                // T21.1: Write diary before compacting
                self.write_compact_diary(&session.messages).await;
                // Context overflow means we're at >100%, use forceful mode
                match compactor.compact_full(&session.messages, self.config.context_window, 0.96).await {
                    Ok((compacted, _result_opt)) if compacted.len() < session.messages.len() => {
                        debug!("Context overflow compact: messages {} -> {}", session.messages.len(), compacted.len());
                        session.messages = compacted;
                        // [V4 DEPRECATED] Memory extraction moved to MemoryKeeper via ShadowEvent bus
                        // Undo turn increment so retry doesn't waste a turn
                        session.turn_count = session.turn_count.saturating_sub(1);
                        continue; // Success, retry the API request
                    }
                    Ok(_) => {
                        let _ = event_tx.send(LoopEvent::Error(
                            "Context size exceeded model limits, but cannot safely compact further because the most recent messages are too large! Please type /new to start a fresh session.".into()
                        )).await;
                        break; // Failed to compact due to massive recent messages, stop infinite loop
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
                    // T23: Run memory consolidation before compacting
                    self.run_consolidation(&mut session).await;
                    // T21.1: Write diary before compacting
                    self.write_compact_diary(&session.messages).await;
                    // ── v2 Phase 1.5: Compact with structured result ─────────────
                    match compactor.compact_full(&session.messages, self.config.context_window, budget_pct).await {
                        Ok((compacted, _result_opt)) => {
                            debug!("Post-flight compact: messages {} -> {}", session.messages.len(), compacted.len());
                            if compacted.len() >= session.messages.len() {
                                // Compaction reached its limit, do not attempt to auto-compact again this session
                                compaction_exhausted = true;
                                let _ = event_tx.send(LoopEvent::Error(
                                    "[Warning] Auto-compaction cannot reduce size further. Approaching hard limits!".into()
                                )).await;
                            }
                            session.messages = compacted;
                            // [V4 DEPRECATED] Memory extraction moved to MemoryKeeper via ShadowEvent bus
                        }
                        Err(e) => warn!("Compact failed: {}", e),
                    }
                }
                BudgetCheck::NeedsCompact => {
                    // Do nothing, we already know we can't compact it further. Wait for hard overflow.
                }
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
                // --- Strategy 6: StopHooks (serial, blocking) ---
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
                    // Truncate at character boundary to avoid half-character corruption
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

                // [V6] Hard gate 已在 tool_schemas 层面完成过滤，此处正常执行
                let mut result = match self.tools.execute(&tc.name, input.clone(), self.config.tool_timeout).await {
                    Ok(r) => {
                        debug!("Tool result: name={}, result_len={}", tc.name, r.len());
                        r
                    }
                    Err(e) => {
                        error!("Tool `{}` failed: {}", tc.name, e);
                        format!("{{\"error\": \"{}\"}}", e)
                    }
                };

                // ==== 工业级核心护城河：Tool-Level Output Truncation ====
                // 强制对所有工具的单个执行结果进行字符上限管控，防死循环+防爆显存
                const MAX_TOOL_CHARS: usize = 30000;
                // 按字节数估算，如果真超了，安全按字符截断
                if result.len() > MAX_TOOL_CHARS {
                    let omitted = result.len() - MAX_TOOL_CHARS;
                    // 确保按 UTF-8 字符边界截断，防止半个中文乱码
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

            // [V6] 委派工具执行后立即结束循环，不再发第二轮 API 请求
            // LLM 的第一轮文字回复已经告知用户，无需再来一轮
            let delegated = tool_calls.iter().any(|tc| {
                tc.name == "delegate_task" || tc.name == "delegate_complex_project"
            });
            if delegated {
                // 如果 LLM 没有生成任何文字（直接调了工具），补一条默认通知
                if text_len == 0 {
                    let fallback = "好的，任务已派发给后台处理，完成后会自动通知您。";
                    let _ = event_tx.send(LoopEvent::TextDelta(fallback.to_string())).await;
                }
                info!("[V6] Delegation tool executed, ending query loop (no second round-trip)");
                self.hooks.fire_stop(&mut session).await;
                let _ = event_tx.send(LoopEvent::TurnEnd).await;
                break;
            }
        }

        // [V4 Phase 3.1] Preflight result already captured at start of turn
        // [V4 Phase 4.1] Cached for next turn already done above
        Ok((session, newly_added_messages, preflight_result))
    }

    // T21.1: Write diary entry before compacting messages.
    // Writes a simple session summary to the diary.
    async fn write_compact_diary(&self, messages: &[Message]) {
        let daily = match &self.daily_notes {
            Some(d) => d,
            None => return,
        };
        let sq = match &self.side_query {
            Some(s) => s,
            None => return,
        };

        // Collect recent messages for summarization (last 20)
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
            return; // Not enough to summarize
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

    /// T23: Run memory consolidation — checks mutex, calls SideQuery if needed,
    /// then advances the sweep cursor.
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
                // Advance cursor regardless of whether update happened
                session.last_memory_sweep_index = session.messages.len();
                session.memory_updated_mutex = false;
            }
            Err(e) => warn!("T23: Memory consolidation failed: {}", e),
        }
    }

    // [V4 DEPRECATED] apply_compact_result removed
    // Memory extraction is now handled by MemoryKeeper via ShadowEvent bus (Phase 2)
}

/// Count the token count of current messages using tiktoken (cl100k_base encoding).
/// Falls back to character/4 estimation if tiktoken is not available.
fn estimate_message_tokens(messages: &[Message]) -> usize {
    use crate::token::counter::count_tokens;

    let mut total_tokens = 0;
    for m in messages.iter() {
        if let Some(ref content) = m.content {
            total_tokens += count_tokens(content);
        }
        if let Some(ref tool_calls) = m.tool_calls {
            for tc in tool_calls.iter() {
                // Count tool name and arguments as separate strings
                total_tokens += count_tokens(&tc.name);
                total_tokens += count_tokens(&tc.arguments.to_string());
            }
        }
    }
    total_tokens
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
                        let input = if tc.arguments.is_null() {
                            serde_json::json!({})
                        } else {
                            tc.arguments.clone()
                        };
                        blocks.push(ContentBlock::ToolUse {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            input,
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

