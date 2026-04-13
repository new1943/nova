use anyhow::Result;
use futures::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::stream::StreamEvent;
use crate::types::{ApiRequest, ApiResponse, SseEvent, ContentBlockStartData, DeltaData};

/// LLM API client (Anthropic-compatible)
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

    /// Non-streaming completion
    pub async fn complete(&self, req: &ApiRequest) -> Result<ApiResponse> {
        let resp = self.request_builder(req).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("API error {}: {}", status, body);
        }
        Ok(resp.json().await?)
    }

    /// Streaming completion — sends StreamEvents to the channel.
    /// Returns when the stream ends.
    pub async fn stream(&self, req: &ApiRequest, tx: mpsc::Sender<StreamEvent>) -> Result<()> {
        let resp = self.request_builder(req).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let _ = tx.send(StreamEvent::Error(format!("API error {}: {}", status, body))).await;
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
                                warn!("SSE parse failed: {} — data: {}", e, &data[..data.len().min(200)]);
                            }
                        }
                    }
                }
            }
        }

        // If we got no events at all, log the remaining buffer
        if !got_any_event && !buffer.trim().is_empty() {
            warn!("Stream ended with no parsed events. Remaining buffer: {}", &buffer[..buffer.len().min(500)]);
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
            SseEvent::ContentBlockStart { content_block, .. } => {
                match content_block {
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
                }
            }
            SseEvent::ContentBlockDelta { delta, .. } => {
                match delta {
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
                }
            }
            SseEvent::ContentBlockStop { index } => {
                let _ = tx.send(StreamEvent::ToolUseEnd { index }).await;
            }
            SseEvent::MessageDelta { delta, usage } => {
                if let Some(usage) = usage {
                    let _ = tx.send(StreamEvent::Usage(usage)).await;
                }
                let _ = tx.send(StreamEvent::MessageStop { stop_reason: delta.stop_reason }).await;
            }
            SseEvent::MessageStop => {
                let _ = tx.send(StreamEvent::MessageStop { stop_reason: None }).await;
            }
            SseEvent::Error { error } => {
                let _ = tx.send(StreamEvent::Error(error.message)).await;
            }
            SseEvent::Ping => {}
        }
    }
}
