use crate::agent::pipeline::{PipelineStage, TurnContext};
use crate::agent::preflight::PreFlightChecker;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

/// ClassifyStage — 调用 Preflight 分类器，写入 complexity 和 preflight_result。
///
/// 这是 Pipeline 的第一个 Stage，为后续所有 Stage 提供分类元数据。
/// 下游 Stage（GateStage、InjectStage）只读取 ClassifyStage 的结果，
/// 不重复做分类。
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
            crate::agent::preflight::PreFlightCheckResult::default()
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
