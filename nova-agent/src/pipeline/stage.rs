use anyhow::Result;
use async_trait::async_trait;
use nova_memory::session::manager::Session;

use super::context::TurnContext;

/// PipelineStage — a single step in the TurnPipeline
///
/// Each stage reads from and writes to TurnContext. Stages do not
/// communicate directly — only through the shared context.
#[async_trait]
pub trait PipelineStage: Send + Sync {
    /// Stage name for logging and decision_log
    fn name(&self) -> &str;

    /// Execute the stage, modifying TurnContext
    async fn execute(&self, ctx: &mut TurnContext, session: &mut Session) -> Result<()>;
}
