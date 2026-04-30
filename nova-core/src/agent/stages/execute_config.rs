use crate::agent::pipeline::{PipelineStage, TurnContext};
use crate::agent::preflight::Complexity;
use tracing::info;

/// ExecuteConfigStage — 决定委派后是否终止循环。
///
/// 读取 GateStage 的 allowed_tools，如果是委派模式（High/Medium），
/// 设置 should_terminate_after_tool = true，让 QueryLoop 在执行
/// delegate_task/delegate_complex_project 后立即终止，不做多余的 LLM 调用。
pub struct ExecuteConfigStage;

impl ExecuteConfigStage {
    pub fn new() -> Self { Self }
}

impl Default for ExecuteConfigStage {
    fn default() -> Self { Self::new() }
}

#[async_trait::async_trait]
impl PipelineStage for ExecuteConfigStage {
    fn name(&self) -> &str { "execute_config" }

    async fn execute(&self, ctx: &mut TurnContext) -> anyhow::Result<()> {
        let should_terminate = matches!(ctx.complexity, Complexity::High | Complexity::Medium);

        ctx.should_terminate_after_tool = should_terminate;

        if should_terminate {
            info!("ExecuteConfig: will terminate after delegation tool call");
            ctx.log_decision(
                "execute_config",
                "terminate_after_tool=true",
                &format!("Complexity={:?}, delegation mode", ctx.complexity),
            );
        } else {
            ctx.log_decision(
                "execute_config",
                "terminate_after_tool=false",
                "Low complexity, normal interactive mode",
            );
        }

        Ok(())
    }
}
