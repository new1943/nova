use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// LLM 补全请求（provider 无关）
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub model: String,
    pub max_tokens: u32,
    pub system: String,
    pub messages: Vec<CompletionMessage>,
    pub tools: Vec<ToolSchema>,
    pub stream: bool,
}

/// 补全请求中的消息（provider 无关）
#[derive(Debug, Clone)]
pub enum CompletionMessage {
    User { content: CompletionContent },
    Assistant { content: CompletionContent },
}

/// 消息内容
#[derive(Debug, Clone)]
pub enum CompletionContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

/// Image source for base64-encoded images (Anthropic API format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub source_type: String,  // always "base64"
    pub media_type: String,   // e.g. "image/png", "image/jpeg"
    pub data: String,         // base64-encoded image data
}

/// 内容块类型
#[derive(Debug, Clone)]
pub enum ContentBlock {
    Text { text: String },
    Image { source: ImageSource },
    Thinking { thinking: String, signature: Option<String> },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String },
}

/// 工具 schema
#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// LLM 补全响应
#[derive(Debug, Clone)]
pub struct CompletionResponse {
    pub id: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
    pub usage: TokenUsage,
}

/// Token 用量
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// 流式增量事件
#[derive(Debug)]
pub enum StreamDelta {
    TextDelta(String),
    ToolUseStart { id: String, name: String },
    ToolInputDelta(String),
    ToolUseEnd { index: usize },
    MessageStop { stop_reason: Option<String> },
    Usage(TokenUsage),
    Error(String),
}

/// LLM 后端抽象 trait
#[async_trait]
pub trait LlmBackend: Send + Sync {
    /// 非流式补全
    async fn complete(&self, req: &CompletionRequest) -> anyhow::Result<CompletionResponse>;

    /// 流式补全 — 通过 channel 发送增量事件
    async fn stream(
        &self,
        req: &CompletionRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> anyhow::Result<()>;
}
