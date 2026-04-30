use crate::agent::pipeline::{PipelineStage, PromptInjection, TurnContext};
use tracing::info;

/// InjectStage — 将分类结果、任务上下文等注入到 system prompt。
///
/// 读取前置 Stage 的输出，生成 XML 标签片段追加到 prompt_injections。
/// 这是 **提示词注入的唯一出口**，所有运行时上下文都通过这里注入。
///
/// 原则：代码做执行（GateStage），提示词做解释（InjectStage）。
/// 如果 GateStage 已经物理隔离了工具，InjectStage 只告诉 LLM "为什么"，
/// 不重复说 "你只能用这些工具"。
pub struct InjectStage {
    /// 可选的任务上下文目录
    memories_dir: Option<std::path::PathBuf>,
}

impl InjectStage {
    pub fn new(memories_dir: Option<std::path::PathBuf>) -> Self {
        Self { memories_dir }
    }
}

#[async_trait::async_trait]
impl PipelineStage for InjectStage {
    fn name(&self) -> &str { "inject" }

    async fn execute(&self, ctx: &mut TurnContext) -> anyhow::Result<()> {
        // 1. 注入 Preflight 分类结果（告诉 LLM 分类原因，而非约束它）
        if let Some(ref result) = ctx.preflight_result {
            ctx.prompt_injections.push(PromptInjection {
                tag: "preflight".into(),
                content: format!(
                    "complexity: {:?}\ntopic_shift: {}\nreason: {}",
                    result.complexity, result.topic_shift, result.reason,
                ),
                priority: 1,
            });
        }

        // 2. 注入任务上下文
        if let Some(ref memories_dir) = self.memories_dir {
            match crate::task::TaskLogger::read_context(memories_dir).await {
                Ok(tasks_ctx) if !tasks_ctx.is_empty() => {
                    ctx.prompt_injections.push(PromptInjection {
                        tag: "tasks".into(),
                        content: tasks_ctx,
                        priority: 10,
                    });
                }
                _ => {}
            }
        }

        info!("Inject: {} injections prepared", ctx.prompt_injections.len());
        ctx.log_decision(
            "inject",
            &format!("{} injections", ctx.prompt_injections.len()),
            "Preflight + tasks context injected",
        );
        Ok(())
    }
}
