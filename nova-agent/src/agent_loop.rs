use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{warn, info, debug};

use nova_core::llm_backend::LlmBackend;
use crate::hooks::HookManager;
use crate::memory::{EpisodicMemory, ConsolidationMemory};
use nova_core::message::{Message, Role};
use nova_memory::session::manager::Session;
use nova_memory::sidequery::SideQuery;
use crate::token::budget::{BudgetCheck, TokenBudget};
use crate::token::compact::Compactor;
use nova_tools::registry::{ToolRegistry, ToolContext};
use nova_core::policy::AgentPolicy;

use crate::pipeline::TurnPipeline;
use crate::pipeline::context::TurnContext;
use crate::pipeline::stages::{PolicyStage, InjectStage, BudgetStage, StreamStage, ExecuteStage};
use crate::pipeline::stages::budget::estimate_message_tokens;

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

/// The core Query Loop — uses Pipeline stages for modular processing
pub struct QueryLoop {
    pub config: QueryLoopConfig,
    pub backend: Arc<dyn LlmBackend>,
    pub tools: Arc<ToolRegistry>,
    pub hooks: HookManager,
    pub episodic: Option<EpisodicMemory>,
    pub side_query: Option<SideQuery>,
    pub consolidation: Option<ConsolidationMemory>,
    pub notify_tx: Option<mpsc::Sender<nova_core::executor::types::Notification>>,
    pub tool_context: ToolContext,
    pub approval_handler: Option<Arc<dyn nova_core::approval::ApprovalHandler>>,
    pub policy: Arc<dyn AgentPolicy>,
}

impl QueryLoop {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        backend: Arc<dyn LlmBackend>,
        tools: Arc<ToolRegistry>,
        hooks: HookManager,
        config: QueryLoopConfig,
        episodic: Option<EpisodicMemory>,
        side_query: Option<SideQuery>,
        consolidation: Option<ConsolidationMemory>,
        notify_tx: Option<mpsc::Sender<nova_core::executor::types::Notification>>,
        tool_context: ToolContext,
    ) -> Self {
        Self {
            config, backend, tools, hooks, episodic, side_query, consolidation,
            notify_tx,
            tool_context,
            approval_handler: None,
            policy: Arc::new(nova_core::policy::PassthroughPolicy),
        }
    }

    pub fn with_approval_handler(mut self, handler: Arc<dyn nova_core::approval::ApprovalHandler>) -> Self {
        self.approval_handler = Some(handler);
        self
    }

    pub fn with_policy(mut self, policy: Arc<dyn AgentPolicy>) -> Self {
        self.policy = policy;
        self
    }

    /// Run a full query loop turn for user input.
    pub async fn run_turn(
        &self,
        mut session: Session,
        system_prompt: &str,
        event_tx: mpsc::Sender<LoopEvent>,
    ) -> Result<(Session, Vec<Message>)> {
        session.reset_turns();
        let mut newly_added_messages = Vec::new();
        info!("--- Starting new query loop ---");

        let mut empty_retries: u32 = 0;
        let mut compaction_exhausted = false;
        const MAX_EMPTY_RETRIES: u32 = 3;

        // Build TurnPipeline with all 5 stages
        let all_tool_names: Vec<String> = self.tools.as_api_schemas().iter().map(|s| s.name.clone()).collect();

        let pipeline = TurnPipeline::new()
            .add(Box::new(PolicyStage::new(self.policy.clone(), all_tool_names)))
            .add(Box::new(InjectStage))
            .add(Box::new(BudgetStage {
                context_window: self.config.context_window,
                budget_trigger_pct: self.config.budget_trigger_pct,
                compact_target_pct: self.config.compact_target_pct,
            }))
            .add(Box::new(StreamStage {
                backend: self.backend.clone(),
                model: self.config.model.clone(),
                max_tokens: self.config.max_tokens,
                event_tx: event_tx.clone(),
            }))
            .add(Box::new(ExecuteStage {
                tools: self.tools.clone(),
                tool_context: self.tool_context.clone(),
                tool_timeout: self.config.tool_timeout,
                event_tx: event_tx.clone(),
                approval_handler: self.approval_handler.clone(),
            }));

        // Create TurnContext once, reset per iteration
        let mut ctx = TurnContext::new(
            String::new(),
            session.session_id.clone(),
            system_prompt.to_string(),
        );

        loop {
            if !session.increment_turn() {
                let _ = event_tx.send(LoopEvent::Error(
                    format!("Max turns ({}) reached", self.config.max_turns),
                )).await;
                break;
            }

            // Refresh per-iteration input
            ctx.user_input = session.messages.last()
                .and_then(|m| m.content.clone())
                .unwrap_or_default();
            ctx.system_prompt = system_prompt.to_string();

            // Run all 5 pipeline stages: Policy → Inject → Budget → Stream → Execute
            pipeline.run(&mut ctx, &mut session).await?;

            // ── Post-pipeline control flow ──────────────────────

            // Budget pre-check: compact if needed
            if ctx.needs_compact {
                let _ = event_tx.send(LoopEvent::CompactTriggered).await;
                self.run_consolidation(&mut session).await;
                self.write_compact_diary(&session.messages).await;
                let compactor = Compactor::new(self.config.compact_target_pct);
                let estimated = estimate_message_tokens(&session.messages);
                let budget_pct = estimated as f32 / self.config.context_window as f32;
                match compactor.compact_full(&session.messages, self.config.context_window, budget_pct).await {
                    Ok((compacted, _)) => {
                        debug!("Compact: {} -> {}", session.messages.len(), compacted.len());
                        session.messages = compacted;
                    }
                    Err(e) => warn!("Compact failed: {}", e),
                }
            }

            // Handle empty response
            if ctx.empty_response {
                warn!("Empty response, retry={}/{}", empty_retries, MAX_EMPTY_RETRIES);
                empty_retries += 1;
                if empty_retries >= MAX_EMPTY_RETRIES {
                    let _ = event_tx.send(LoopEvent::Error(
                        format!("Empty response {} times — giving up", MAX_EMPTY_RETRIES),
                    )).await;
                    break;
                }
                session.turn_count = session.turn_count.saturating_sub(1);
                tokio::time::sleep(Duration::from_secs(1)).await;
                ctx.reset_iteration();
                continue;
            }
            empty_retries = 0;

            // Handle context overflow
            if ctx.context_overflow {
                warn!("Context overflow, compacting...");
                let _ = event_tx.send(LoopEvent::CompactTriggered).await;
                self.write_compact_diary(&session.messages).await;
                let compactor = Compactor::new(self.config.compact_target_pct);
                match compactor.compact_full(&session.messages, self.config.context_window, 0.96).await {
                    Ok((compacted, _)) if compacted.len() < session.messages.len() => {
                        debug!("Overflow compact: {} -> {}", session.messages.len(), compacted.len());
                        session.messages = compacted;
                        session.turn_count = session.turn_count.saturating_sub(1);
                        ctx.reset_iteration();
                        continue;
                    }
                    Ok(_) => {
                        let _ = event_tx.send(LoopEvent::Error(
                            "Context size exceeded. Type /new for a fresh session.".into()
                        )).await;
                        break;
                    }
                    Err(e) => {
                        warn!("Overflow compact failed: {}", e);
                        break;
                    }
                }
            }

            // Update token stats
            session.token_stats.total_input_tokens += ctx.token_usage.input_tokens;
            session.token_stats.total_output_tokens += ctx.token_usage.output_tokens;

            // Budget post-check
            let input_tokens = ctx.token_usage.input_tokens as usize;
            let mut budget = TokenBudget::new(self.config.context_window, self.config.budget_trigger_pct);
            match budget.check(input_tokens) {
                BudgetCheck::NeedsCompact if !compaction_exhausted => {
                    let _ = event_tx.send(LoopEvent::CompactTriggered).await;
                    self.run_consolidation(&mut session).await;
                    self.write_compact_diary(&session.messages).await;
                    let compactor = Compactor::new(self.config.compact_target_pct);
                    let pct = input_tokens as f32 / self.config.context_window as f32;
                    match compactor.compact_full(&session.messages, self.config.context_window, pct).await {
                        Ok((compacted, _)) => {
                            if compacted.len() >= session.messages.len() {
                                compaction_exhausted = true;
                                let _ = event_tx.send(LoopEvent::Error(
                                    "[Warning] Auto-compaction cannot reduce further.".into()
                                )).await;
                            }
                            session.messages = compacted;
                        }
                        Err(e) => warn!("Post-compact failed: {}", e),
                    }
                }
                BudgetCheck::Diminishing => {
                    warn!("Budget diminishing, breaking");
                    let _ = event_tx.send(LoopEvent::Error(
                        "Token budget: diminishing returns".into(),
                    )).await;
                    break;
                }
                _ => {}
            }
            budget.record_turn(input_tokens);

            // No tool calls → turn done
            if ctx.tool_calls.is_empty() {
                let content = if ctx.text_content.is_empty() { None } else { Some(ctx.text_content.clone()) };
                let assistant_msg = Message::assistant(content, None);
                self.hooks.fire_post_sampling(&assistant_msg, &session).await;
                newly_added_messages.push(assistant_msg.clone());
                session.add_message(assistant_msg);

                debug!("Loop exit: no tool_calls");
                self.hooks.fire_stop(&mut session).await;
                let _ = event_tx.send(LoopEvent::TurnEnd).await;
                break;
            }

            // Track newly added messages (ExecuteStage already recorded them)
            let msg_count = session.messages.len();
            let start_idx = msg_count.saturating_sub(1 + ctx.tool_calls.len());
            for msg in &session.messages[start_idx..] {
                newly_added_messages.push(msg.clone());
            }

            // Fire hooks on assistant message
            if let Some(assistant_msg) = session.messages.get(start_idx) {
                self.hooks.fire_post_sampling(assistant_msg, &session).await;
            }

            // Terminate after tool if policy says so
            if ctx.terminate_after_tool {
                debug!("Policy: terminate_after_tool");
                break;
            }

            // Reset per-iteration state for next loop
            ctx.reset_iteration();
        }

        Ok((session, newly_added_messages))
    }

    async fn write_compact_diary(&self, messages: &[Message]) {
        let daily = match &self.episodic {
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
                if content.is_empty() { None } else { Some(format!("{}: {}", role, content)) }
            })
            .collect();

        if recent.len() < 3 { return; }

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
                    info!("Compact diary written: {} chars", summary.len());
                }
            }
            Ok(_) => {}
            Err(e) => warn!("Compact diary failed: {}", e),
        }
    }

    async fn run_consolidation(&self, session: &mut Session) {
        let consolidator = match &self.consolidation {
            Some(c) => c,
            None => return,
        };
        match consolidator.consolidate_session(
            &session.messages,
            session.last_memory_sweep_index,
            session.memory_updated_mutex,
        ).await {
            Ok(_) => {
                session.last_memory_sweep_index = session.messages.len();
                session.memory_updated_mutex = false;
            }
            Err(e) => warn!("Consolidation failed: {}", e),
        }
    }
}
