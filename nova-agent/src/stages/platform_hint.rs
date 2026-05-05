use nova_core::pipeline::{PipelineStage, PromptInjection, TurnContext};
use nova_core::platform::Platform;

/// PlatformHintStage — 根据当前平台注入行为提示到 system prompt。
///
/// - Discord: 消息长度限制、Markdown 格式、简洁对话风格
/// - TUI: 终端全宽可用、代码块、详细格式
/// - None: 静默跳过
#[derive(Default)]
pub struct PlatformHintStage;

impl PlatformHintStage {
    pub fn new() -> Self {
        Self
    }

    fn discord_hint() -> &'static str {
        "You are responding on Discord. Keep messages under 2000 characters. \
         Use Markdown formatting. Avoid very long code blocks. \
         Be concise and conversational."
    }

    fn tui_hint() -> &'static str {
        "You are responding in a terminal (TUI). Full terminal width is available. \
         You can use code blocks and detailed formatting. \
         Longer responses are acceptable."
    }
}

#[async_trait::async_trait]
impl PipelineStage for PlatformHintStage {
    fn name(&self) -> &str {
        "platform_hint"
    }

    async fn execute(&self, ctx: &mut TurnContext) -> anyhow::Result<()> {
        let hint = match ctx.platform {
            Some(Platform::Discord) => Some(Self::discord_hint()),
            Some(Platform::Tui) => Some(Self::tui_hint()),
            None => None,
        };

        if let Some(hint_text) = hint {
            ctx.prompt_injections.push(PromptInjection {
                tag: "platform-hint".into(),
                content: hint_text.to_string(),
                priority: 2,
            });
            ctx.log_decision(
                "platform_hint",
                &format!("injected {:?} hint", ctx.platform),
                "Platform-specific behavior guidance",
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_core::pipeline::TurnContext;

    #[tokio::test]
    async fn test_discord_platform_injects_hint() {
        let stage = PlatformHintStage::new();
        let mut ctx = TurnContext::new("hello".into(), vec![])
            .with_platform(Platform::Discord);

        stage.execute(&mut ctx).await.unwrap();

        assert_eq!(ctx.prompt_injections.len(), 1);
        let injection = &ctx.prompt_injections[0];
        assert_eq!(injection.tag, "platform-hint");
        assert_eq!(injection.priority, 2);
        assert!(injection.content.contains("Discord"));
        assert!(injection.content.contains("2000 characters"));
        assert!(injection.content.contains("Markdown"));
        assert!(injection.content.contains("concise"));
    }

    #[tokio::test]
    async fn test_tui_platform_injects_hint() {
        let stage = PlatformHintStage::new();
        let mut ctx = TurnContext::new("hello".into(), vec![])
            .with_platform(Platform::Tui);

        stage.execute(&mut ctx).await.unwrap();

        assert_eq!(ctx.prompt_injections.len(), 1);
        let injection = &ctx.prompt_injections[0];
        assert_eq!(injection.tag, "platform-hint");
        assert_eq!(injection.priority, 2);
        assert!(injection.content.contains("terminal"));
        assert!(injection.content.contains("TUI"));
        assert!(injection.content.contains("code blocks"));
        assert!(injection.content.contains("Longer responses"));
    }

    #[tokio::test]
    async fn test_none_platform_does_not_inject() {
        let stage = PlatformHintStage::new();
        let mut ctx = TurnContext::new("hello".into(), vec![]);
        // platform is None by default

        stage.execute(&mut ctx).await.unwrap();

        assert!(ctx.prompt_injections.is_empty());
        assert!(ctx.decision_log.is_empty());
    }
}
