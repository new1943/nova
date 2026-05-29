use async_trait::async_trait;

use crate::message::Message;

/// Policy context — input for AgentPolicy::decide()
pub struct PolicyContext {
    pub user_input: String,
    pub recent_messages: Vec<Message>,
    pub all_tool_names: Vec<String>,
}

/// Policy decision — output of AgentPolicy::decide()
pub struct PolicyDecision {
    /// Tool names visible to the LLM this turn
    pub allowed_tools: Vec<String>,
    /// Whether to break the loop after a tool call
    pub terminate_after_tool: bool,
    /// Extra content to inject into system prompt
    pub prompt_injection: String,
}

/// AgentPolicy — decides tool visibility and behavior each turn
///
/// Implementations can be simple (passthrough all tools) or complex
/// (Preflight classification → tool filtering). The Pipeline calls
/// `decide()` once per turn before the LLM request.
#[async_trait]
pub trait AgentPolicy: Send + Sync {
    async fn decide(&self, ctx: &PolicyContext) -> PolicyDecision;
}

/// Passthrough policy — all tools visible, no termination override
pub struct PassthroughPolicy;

#[async_trait]
impl AgentPolicy for PassthroughPolicy {
    async fn decide(&self, ctx: &PolicyContext) -> PolicyDecision {
        PolicyDecision {
            allowed_tools: ctx.all_tool_names.clone(),
            terminate_after_tool: false,
            prompt_injection: String::new(),
        }
    }
}
