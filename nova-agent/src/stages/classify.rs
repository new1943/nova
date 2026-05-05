use nova_core::pipeline::{PipelineStage, TurnContext};
use nova_core::preflight_types::PreFlightCheckResult;
use crate::preflight::PreFlightChecker;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

/// ClassifyStage — 调用 Preflight 分类器，写入 complexity 和 preflight_result。
pub struct ClassifyStage {
    checker: Arc<PreFlightChecker>,
    timeout_secs: u64,
}

impl ClassifyStage {
    pub fn new(checker: Arc<PreFlightChecker>, timeout_secs: u64) -> Self {
        Self { checker, timeout_secs }
    }
}

#[async_trait::async_trait]
impl PipelineStage for ClassifyStage {
    fn name(&self) -> &str { "classify" }

    async fn execute(&self, ctx: &mut TurnContext) -> anyhow::Result<()> {
        let result = tokio::time::timeout(
            Duration::from_secs(self.timeout_secs),
            self.checker.check(&ctx.user_input, &ctx.recent_messages),
        ).await.unwrap_or_else(|_| {
            ctx.log_decision("classify", "timeout", "Preflight timed out, using default Low");
            PreFlightCheckResult::default()
        });

        info!("Classify: complexity={:?}, topic_shift={}, reason={}",
            result.complexity, result.topic_shift, result.reason);

        ctx.complexity = result.complexity;
        ctx.topic_shift = result.topic_shift;
        ctx.log_decision(
            "classify",
            &format!("complexity={:?}", result.complexity),
            &result.reason,
        );
        ctx.preflight_result = Some(result);
        Ok(())
    }
}
