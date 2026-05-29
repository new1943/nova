pub mod stages;
pub mod context;
pub mod stage;

pub use context::TurnContext;
pub use stage::PipelineStage;

use anyhow::Result;
use nova_memory::session::manager::Session;
use std::time::Instant;
use tracing::debug;

/// TurnPipeline — orchestrates per-turn processing through ordered stages
///
/// Replaces the monolithic agent_loop with composable, testable stages.
/// Each stage writes to its own fields in TurnContext and reads others'.
pub struct TurnPipeline {
    stages: Vec<Box<dyn PipelineStage>>,
}

impl TurnPipeline {
    pub fn new() -> Self {
        Self { stages: Vec::new() }
    }

    pub fn add(mut self, stage: Box<dyn PipelineStage>) -> Self {
        self.stages.push(stage);
        self
    }

    /// Execute all stages in order, recording decision log entries
    pub async fn run(&self, ctx: &mut TurnContext, session: &mut Session) -> Result<()> {
        for stage in &self.stages {
            let start = Instant::now();
            stage.execute(ctx, session).await?;
            let elapsed = start.elapsed();
            debug!("[Pipeline] {} completed in {:.1}ms", stage.name(), elapsed.as_secs_f64() * 1000.0);
            ctx.decision_log.push(crate::pipeline::context::DecisionEntry {
                stage: stage.name().into(),
                elapsed,
                timestamp: Instant::now(),
            });
        }
        Ok(())
    }
}
