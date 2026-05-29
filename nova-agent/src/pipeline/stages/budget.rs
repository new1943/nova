use anyhow::Result;
use async_trait::async_trait;
use nova_memory::session::manager::Session;
use tracing::debug;

use crate::pipeline::context::TurnContext;
use crate::pipeline::stage::PipelineStage;
use crate::token::budget::TokenBudget;
use crate::token::counter::count_tokens;
use nova_core::message::Message;

/// BudgetStage — pre-flight and post-flight token budget management
///
/// Reads: session messages, context_window
/// Writes: needs_compact, budget_pct, should_break
pub struct BudgetStage {
    pub context_window: usize,
    pub budget_trigger_pct: f32,
    pub compact_target_pct: f32,
}

#[async_trait]
impl PipelineStage for BudgetStage {
    fn name(&self) -> &str { "budget" }

    async fn execute(&self, ctx: &mut TurnContext, session: &mut Session) -> Result<()> {
        let estimated = estimate_message_tokens(&session.messages);
        let budget_pct = estimated as f32 / self.context_window as f32;
        ctx.budget_pct = budget_pct;

        debug!("Budget: estimated_tokens={}, pct={:.1}%, threshold={:.1}%",
            estimated, budget_pct * 100.0, self.budget_trigger_pct * 100.0);

        let budget = TokenBudget::new(self.context_window, self.budget_trigger_pct);
        if estimated > 0 && budget.needs_compact(estimated) {
            ctx.needs_compact = true;
        }

        Ok(())
    }
}

/// Estimate token count for messages (shared with compactor)
pub fn estimate_message_tokens(messages: &[Message]) -> usize {
    let mut total = 0;
    for m in messages {
        if let Some(ref content) = m.content {
            total += count_tokens(content);
        }
        if let Some(ref tool_calls) = m.tool_calls {
            for tc in tool_calls {
                total += count_tokens(&tc.name);
                total += count_tokens(&tc.arguments.to_string());
            }
        }
    }
    total
}
