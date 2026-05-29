use std::time::{Duration, Instant};

use nova_core::llm_backend::TokenUsage;

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

/// Decision log entry — records what each stage decided
#[derive(Debug, Clone)]
pub struct DecisionEntry {
    pub stage: String,
    pub elapsed: Duration,
    pub timestamp: Instant,
}

/// TurnContext — shared state for all Pipeline stages within a single turn
///
/// Each stage writes to its own fields and reads others'. This replaces
/// the implicit state sharing in the monolithic agent_loop.
pub struct TurnContext {
    // ── Input (immutable) ──────────────────────
    pub user_input: String,
    pub session_id: String,

    // ── Stage: Inject ──────────────────────────
    pub system_prompt: String,
    pub prompt_injections: Vec<String>,

    // ── Stage: Policy ──────────────────────────
    pub allowed_tools: Vec<String>,
    pub terminate_after_tool: bool,

    // ── Stage: Budget ──────────────────────────
    pub needs_compact: bool,
    pub budget_pct: f32,

    // ── Stage: Stream (output) ─────────────────
    pub tool_calls: Vec<AccumulatedToolCall>,
    pub text_content: String,
    pub token_usage: TokenUsage,
    pub context_overflow: bool,
    pub stream_error: Option<String>,

    // ── Stage: Execute (output) ────────────────
    pub tool_results: Vec<(String, String)>,  // (tool_call_id, result)

    // ── Loop control ───────────────────────────
    pub turn_count: usize,
    pub should_break: bool,
    pub break_reason: Option<String>,
    pub empty_response: bool,

    // ── Observability ──────────────────────────
    pub decision_log: Vec<DecisionEntry>,
}

impl TurnContext {
    pub fn new(user_input: String, session_id: String, system_prompt: String) -> Self {
        Self {
            user_input,
            session_id,
            system_prompt,
            prompt_injections: Vec::new(),
            allowed_tools: Vec::new(),
            terminate_after_tool: false,
            needs_compact: false,
            budget_pct: 0.0,
            tool_calls: Vec::new(),
            text_content: String::new(),
            token_usage: TokenUsage::default(),
            context_overflow: false,
            stream_error: None,
            tool_results: Vec::new(),
            turn_count: 0,
            should_break: false,
            break_reason: None,
            empty_response: false,
            decision_log: Vec::new(),
        }
    }

    /// Reset per-iteration state (between loop iterations)
    pub fn reset_iteration(&mut self) {
        self.tool_calls.clear();
        self.text_content.clear();
        self.token_usage = TokenUsage::default();
        self.context_overflow = false;
        self.stream_error = None;
        self.tool_results.clear();
        self.empty_response = false;
        self.needs_compact = false;
    }
}
