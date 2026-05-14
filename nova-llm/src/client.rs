use anyhow::Result;
use async_trait::async_trait;
use futures::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use nova_core::llm_backend::{
    CompletionContent, CompletionMessage, CompletionRequest, CompletionResponse,
    ContentBlock as CoreContentBlock, LlmBackend, StreamDelta,
    TokenUsage, ToolSchema as CoreToolSchema,
};

use crate::stream::StreamEvent;
use crate::types::{
    ApiMessage, ApiRequest, ApiResponse, Content, ContentBlock, ContentBlockStartData, DeltaData,
    SseEvent, ToolSchema,
};

/// LLM API client (Anthropic-compatible)
#[derive(Clone)]
pub struct ApiClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl ApiClient {
    pub fn new(api_key: String, base_url: String) -> Self {
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(120))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("failed to build http client");
        Self {
            http,
            api_key,
            base_url,
        }
    }

    fn url(&self) -> String {
        format!("{}/v1/messages", self.base_url)
    }

    fn request_builder(&self, req: &ApiRequest) -> reqwest::RequestBuilder {
        self.http
            .post(self.url())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(req)
    }

    /// Non-streaming completion (original method, kept for backward compatibility)
    pub async fn complete(&self, req: &ApiRequest) -> Result<ApiResponse> {
        let resp = self.request_builder(req).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("API error {}: {}", status, body);
        }
        Ok(resp.json().await?)
    }

    /// Streaming completion (original method, kept for backward compatibility)
    /// Sends StreamEvents to the channel. Returns when the stream ends.
    pub async fn stream(&self, req: &ApiRequest, tx: mpsc::Sender<StreamEvent>) -> Result<()> {
        let resp = self.request_builder(req).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let _ = tx
                .send(StreamEvent::Error(format!("API error {}: {}", status, body)))
                .await;
            anyhow::bail!("API error {}: {}", status, body);
        }

        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();
        let mut got_any_event = false;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            debug!("SSE chunk: {}", chunk_str);
            buffer.push_str(&chunk_str);

            // Process complete SSE lines
            while let Some(pos) = buffer.find("\n\n") {
                let block = buffer[..pos].to_string();
                buffer = buffer[pos + 2..].to_string();

                for line in block.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        if data == "[DONE]" {
                            return Ok(());
                        }
                        match serde_json::from_str::<SseEvent>(data) {
                            Ok(event) => {
                                got_any_event = true;
                                self.handle_sse_event(event, &tx).await;
                            }
                            Err(e) => {
                                warn!(
                                    "SSE parse failed: {} — data: {}",
                                    e,
                                    &data[..data.len().min(200)]
                                );
                            }
                        }
                    }
                }
            }
        }

        // If we got no events at all, log the remaining buffer
        if !got_any_event && !buffer.trim().is_empty() {
            warn!(
                "Stream ended with no parsed events. Remaining buffer: {}",
                &buffer[..buffer.len().min(500)]
            );
        }

        Ok(())
    }

    async fn handle_sse_event(&self, event: SseEvent, tx: &mpsc::Sender<StreamEvent>) {
        match event {
            SseEvent::MessageStart { message } => {
                if let Some(usage) = message.usage {
                    let _ = tx.send(StreamEvent::Usage(usage)).await;
                }
            }
            SseEvent::ContentBlockStart { content_block, .. } => match content_block {
                ContentBlockStartData::ToolUse { id, name } => {
                    let _ = tx.send(StreamEvent::ToolUseStart { id, name }).await;
                }
                ContentBlockStartData::Text { text } => {
                    if !text.is_empty() {
                        let _ = tx.send(StreamEvent::TextDelta(text)).await;
                    }
                }
                ContentBlockStartData::Thinking { .. } => {
                    // Thinking blocks are streamed via ThinkingDelta
                }
            },
            SseEvent::ContentBlockDelta { delta, .. } => match delta {
                DeltaData::TextDelta { text } => {
                    let _ = tx.send(StreamEvent::TextDelta(text)).await;
                }
                DeltaData::ThinkingDelta { .. } => {
                    // Skip thinking deltas for now
                }
                DeltaData::SignatureDelta { .. } => {
                    // Skip signature deltas (thinking block signatures)
                }
                DeltaData::InputJsonDelta { partial_json } => {
                    let _ = tx.send(StreamEvent::ToolInputDelta(partial_json)).await;
                }
            },
            SseEvent::ContentBlockStop { index } => {
                let _ = tx.send(StreamEvent::ToolUseEnd { index }).await;
            }
            SseEvent::MessageDelta { delta, usage } => {
                if let Some(usage) = usage {
                    let _ = tx.send(StreamEvent::Usage(usage)).await;
                }
                let _ = tx
                    .send(StreamEvent::MessageStop {
                        stop_reason: delta.stop_reason,
                    })
                    .await;
            }
            SseEvent::MessageStop => {
                let _ = tx
                    .send(StreamEvent::MessageStop { stop_reason: None })
                    .await;
            }
            SseEvent::Error { error } => {
                let _ = tx.send(StreamEvent::Error(error.message)).await;
            }
            SseEvent::Ping => {}
        }
    }

    // --- Type conversion helpers ---

    /// Convert a generic CompletionRequest to an Anthropic-specific ApiRequest
    pub fn to_api_request(&self, req: &CompletionRequest) -> ApiRequest {
        ApiRequest {
            model: req.model.clone(),
            max_tokens: req.max_tokens,
            system: req.system.clone(),
            messages: req.messages.iter().map(Self::convert_message).collect(),
            tools: req.tools.iter().map(Self::convert_tool_schema).collect(),
            stream: req.stream,
        }
    }

    /// Convert a generic CompletionRequest to an Anthropic-specific ApiRequest with stream=true
    fn to_api_request_stream(&self, req: &CompletionRequest) -> ApiRequest {
        let mut api_req = self.to_api_request(req);
        api_req.stream = true;
        api_req
    }

    /// Convert an Anthropic ApiResponse to a generic CompletionResponse
    pub fn to_completion_response(resp: ApiResponse) -> CompletionResponse {
        CompletionResponse {
            id: resp.id,
            content: resp.content.into_iter().map(Self::convert_content_block_to_core).collect(),
            stop_reason: resp.stop_reason,
            usage: TokenUsage {
                input_tokens: resp.usage.input_tokens,
                output_tokens: resp.usage.output_tokens,
            },
        }
    }

    /// Convert a StreamEvent to a StreamDelta
    pub fn to_stream_delta(event: StreamEvent) -> StreamDelta {
        match event {
            StreamEvent::TextDelta(text) => StreamDelta::TextDelta(text),
            StreamEvent::ToolUseStart { id, name } => StreamDelta::ToolUseStart { id, name },
            StreamEvent::ToolInputDelta(json) => StreamDelta::ToolInputDelta(json),
            StreamEvent::ToolUseEnd { index } => StreamDelta::ToolUseEnd { index },
            StreamEvent::MessageStop { stop_reason } => {
                StreamDelta::MessageStop { stop_reason }
            }
            StreamEvent::Usage(usage) => StreamDelta::Usage(TokenUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
            }),
            StreamEvent::Error(msg) => StreamDelta::Error(msg),
        }
    }

    // --- Internal conversion helpers ---

    fn convert_message(msg: &CompletionMessage) -> ApiMessage {
        match msg {
            CompletionMessage::User { content } => ApiMessage::User {
                content: Self::convert_content(content),
            },
            CompletionMessage::Assistant { content } => ApiMessage::Assistant {
                content: Self::convert_content(content),
            },
        }
    }

    fn convert_content(content: &CompletionContent) -> Content {
        match content {
            CompletionContent::Text(text) => Content::Text(text.clone()),
            CompletionContent::Blocks(blocks) => {
                Content::Blocks(blocks.iter().map(Self::convert_content_block_to_api).collect())
            }
        }
    }

    fn convert_content_block_to_api(block: &CoreContentBlock) -> ContentBlock {
        match block {
            CoreContentBlock::Text { text } => ContentBlock::Text { text: text.clone() },
            CoreContentBlock::Image { source } => ContentBlock::Image {
                source: crate::types::ImageSource {
                    source_type: source.source_type.clone(),
                    media_type: source.media_type.clone(),
                    data: source.data.clone(),
                },
            },
            CoreContentBlock::Thinking {
                thinking,
                signature,
            } => ContentBlock::Thinking {
                thinking: thinking.clone(),
                signature: signature.clone(),
            },
            CoreContentBlock::ToolUse { id, name, input } => ContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            },
            CoreContentBlock::ToolResult {
                tool_use_id,
                content,
            } => ContentBlock::ToolResult {
                tool_use_id: tool_use_id.clone(),
                content: content.clone(),
            },
        }
    }

    fn convert_content_block_to_core(block: ContentBlock) -> CoreContentBlock {
        match block {
            ContentBlock::Text { text } => CoreContentBlock::Text { text },
            ContentBlock::Image { source } => CoreContentBlock::Image {
                source: nova_core::llm_backend::ImageSource {
                    source_type: source.source_type,
                    media_type: source.media_type,
                    data: source.data,
                },
            },
            ContentBlock::Thinking {
                thinking,
                signature,
            } => CoreContentBlock::Thinking {
                thinking,
                signature,
            },
            ContentBlock::ToolUse { id, name, input } => {
                CoreContentBlock::ToolUse { id, name, input }
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
            } => CoreContentBlock::ToolResult {
                tool_use_id,
                content,
            },
        }
    }

    fn convert_tool_schema(schema: &CoreToolSchema) -> ToolSchema {
        ToolSchema {
            name: schema.name.clone(),
            description: schema.description.clone(),
            input_schema: schema.input_schema.clone(),
        }
    }
}

// --- LlmBackend trait implementation ---

#[async_trait]
impl LlmBackend for ApiClient {
    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse> {
        let api_req = self.to_api_request(req);
        let api_resp = ApiClient::complete(self, &api_req).await?;
        Ok(Self::to_completion_response(api_resp))
    }

    async fn stream(
        &self,
        req: &CompletionRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> Result<()> {
        let api_req = self.to_api_request_stream(req);
        let (inner_tx, mut inner_rx) = mpsc::channel::<StreamEvent>(64);

        let client = self.clone();
        let stream_handle =
            tokio::spawn(async move { ApiClient::stream(&client, &api_req, inner_tx).await });

        while let Some(event) = inner_rx.recv().await {
            let delta = Self::to_stream_delta(event);
            if tx.send(delta).await.is_err() {
                break;
            }
        }

        stream_handle.await??;
        Ok(())
    }
}
