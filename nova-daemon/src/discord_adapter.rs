use async_trait::async_trait;
use nova_core::platform::{Platform, PlatformAdapter, PlatformMessage};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Discord 平台适配器
///
/// 提供 PlatformAdapter trait 接口，用于统一平台抽象。
/// 完整的 Gateway 集成由现有的 `discord.rs` 模块处理，
/// 此适配器提供 trait 接口以支持未来的统一平台处理。
#[allow(dead_code)]
pub struct DiscordAdapter {
    token: String,
    http: Option<Arc<serenity::http::Http>>,
}

impl DiscordAdapter {
    #[allow(dead_code)]
    pub fn new(token: String) -> Self {
        Self { token, http: None }
    }
}

#[async_trait]
impl PlatformAdapter for DiscordAdapter {
    fn platform(&self) -> Platform {
        Platform::Discord
    }

    async fn send(&self, channel_id: &str, content: &str) -> anyhow::Result<()> {
        let http = self
            .http
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Discord not started"))?;
        let channel =
            serenity::model::id::ChannelId::new(channel_id.parse::<u64>()?);
        // Discord 限制单条消息 2000 字符，使用 1950 留出安全余量
        let chars: Vec<char> = content.chars().collect();
        for chunk in chars.chunks(1950) {
            let chunk_str: String = chunk.iter().collect();
            let builder =
                serenity::builder::CreateMessage::new().content(chunk_str);
            channel.send_message(http, builder).await?;
        }
        Ok(())
    }

    async fn start(
        &mut self,
        _tx: mpsc::Sender<PlatformMessage>,
    ) -> anyhow::Result<()> {
        // Initialize HTTP client for sending messages
        let http = Arc::new(serenity::http::Http::new(&self.token));
        self.http = Some(http);
        // Note: Full Gateway integration is handled by the existing discord.rs module.
        // This adapter provides the trait interface for future unified platform handling.
        Ok(())
    }
}
