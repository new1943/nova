use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// 平台枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Platform {
    Discord,
    Tui,
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Platform::Discord => write!(f, "Discord"),
            Platform::Tui => write!(f, "TUI"),
        }
    }
}

/// 平台消息 — 从平台适配器上报的统一消息结构
#[derive(Debug, Clone)]
pub struct PlatformMessage {
    pub channel_id: String,
    pub user_id: String,
    pub content: String,
}

/// 平台适配器 trait
#[async_trait]
pub trait PlatformAdapter: Send + Sync {
    /// 返回当前平台类型
    fn platform(&self) -> Platform;

    /// 向指定频道发送消息
    async fn send(&self, channel_id: &str, content: &str) -> anyhow::Result<()>;

    /// 启动平台消息监听，将收到的消息通过 channel 上报
    async fn start(&mut self, tx: mpsc::Sender<PlatformMessage>) -> anyhow::Result<()>;
}
