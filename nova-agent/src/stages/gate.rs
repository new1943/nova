use nova_core::pipeline::{PipelineStage, TurnContext};
use nova_core::preflight_types::Complexity;
use tracing::info;

/// GateStage — 根据 complexity 物理隔离工具，LLM 无法绕过。
pub struct GateStage;

impl GateStage {
    pub fn new() -> Self { Self }
}

impl Default for GateStage {
    fn default() -> Self { Self::new() }
}

#[async_trait::async_trait]
impl PipelineStage for GateStage {
    fn name(&self) -> &str { "gate" }

    async fn execute(&self, ctx: &mut TurnContext) -> anyhow::Result<()> {
        let (tools, reason) = match ctx.complexity {
            Complexity::High => (
                Some(vec![
                    "delegate_complex_project".to_string(),
                    "cancel_delegated_project".to_string(),
                ]),
                "High complexity → only delegate_complex_project + cancel",
            ),
            Complexity::Medium => (
                Some(vec![
                    "delegate_task".to_string(),
                    "cancel_delegated_project".to_string(),
                ]),
                "Medium complexity → only delegate_task + cancel",
            ),
            Complexity::Low => (
                None,
                "Low complexity → all tools visible",
            ),
        };

        info!("Gate: {}", reason);
        ctx.allowed_tools = tools;
        ctx.log_decision("gate", &format!("tools={}", 
            ctx.allowed_tools.as_ref()
                .map(|t| t.join(","))
                .unwrap_or("all".into())
        ), reason);

        Ok(())
    }
}
