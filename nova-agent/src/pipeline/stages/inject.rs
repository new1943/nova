use anyhow::Result;
use async_trait::async_trait;
use nova_memory::session::manager::Session;

use crate::pipeline::context::TurnContext;
use crate::pipeline::stage::PipelineStage;

/// InjectStage — assembles the final system prompt with all injections
///
/// Reads: system_prompt, prompt_injections
/// Writes: system_prompt (appends injections)
pub struct InjectStage;

#[async_trait]
impl PipelineStage for InjectStage {
    fn name(&self) -> &str { "inject" }

    async fn execute(&self, ctx: &mut TurnContext, _session: &mut Session) -> Result<()> {
        if !ctx.prompt_injections.is_empty() {
            let injections = ctx.prompt_injections.join("\n\n");
            ctx.system_prompt = format!("{}\n\n{}", ctx.system_prompt, injections);
        }
        Ok(())
    }
}
