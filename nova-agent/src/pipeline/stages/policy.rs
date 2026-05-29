use anyhow::Result;
use async_trait::async_trait;
use nova_core::policy::{AgentPolicy, PolicyContext};
use nova_memory::session::manager::Session;
use std::sync::Arc;

use crate::pipeline::context::TurnContext;
use crate::pipeline::stage::PipelineStage;

/// PolicyStage — decides tool visibility and termination behavior
///
/// Reads: user_input, recent messages, all tool names
/// Writes: allowed_tools, terminate_after_tool
pub struct PolicyStage {
    policy: Arc<dyn AgentPolicy>,
    all_tool_names: Vec<String>,
}

impl PolicyStage {
    pub fn new(policy: Arc<dyn AgentPolicy>, all_tool_names: Vec<String>) -> Self {
        Self { policy, all_tool_names }
    }
}

#[async_trait]
impl PipelineStage for PolicyStage {
    fn name(&self) -> &str { "policy" }

    async fn execute(&self, ctx: &mut TurnContext, session: &mut Session) -> Result<()> {
        let policy_ctx = PolicyContext {
            user_input: ctx.user_input.clone(),
            recent_messages: session.messages.clone(),
            all_tool_names: self.all_tool_names.clone(),
        };

        let decision = self.policy.decide(&policy_ctx).await;
        ctx.allowed_tools = decision.allowed_tools;
        ctx.terminate_after_tool = decision.terminate_after_tool;
        if !decision.prompt_injection.is_empty() {
            ctx.prompt_injections.push(decision.prompt_injection);
        }

        Ok(())
    }
}
