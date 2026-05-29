use anyhow::Result;
use async_trait::async_trait;
use nova_core::llm_backend::{
    CompletionMessage, CompletionRequest, LlmBackend, StreamDelta,
    ContentBlock as CoreContentBlock, ToolSchema as CoreToolSchema,
};
use nova_core::message::{Message, Role};
use nova_memory::session::manager::Session;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::agent_loop::LoopEvent;
use crate::pipeline::context::{AccumulatedToolCall, TurnContext};
use crate::pipeline::stage::PipelineStage;

/// StreamStage — builds the API request and streams the LLM response
///
/// Reads: system_prompt, messages, allowed_tools, config
/// Writes: tool_calls, text_content, token_usage, context_overflow, stream_error, empty_response
pub struct StreamStage {
    pub backend: Arc<dyn LlmBackend>,
    pub model: String,
    pub max_tokens: u32,
    pub event_tx: mpsc::Sender<LoopEvent>,
}

#[async_trait]
impl PipelineStage for StreamStage {
    fn name(&self) -> &str { "stream" }

    async fn execute(&self, ctx: &mut TurnContext, session: &mut Session) -> Result<()> {
        let completion_messages = build_completion_messages(&session.messages);

        let core_tool_schemas: Vec<CoreToolSchema> = ctx.allowed_tools.iter().map(|name| {
            CoreToolSchema {
                name: name.clone(),
                description: String::new(),
                input_schema: serde_json::json!({}),
            }
        }).collect();

        let req = CompletionRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            system: ctx.system_prompt.clone(),
            messages: completion_messages,
            tools: core_tool_schemas,
            stream: true,
        };

        info!("API request: model={}, {} messages", self.model, session.messages.len());

        let (stream_tx, mut stream_rx) = mpsc::channel::<StreamDelta>(64);
        let backend = self.backend.clone();
        let req_clone = req.clone();
        let stream_handle = tokio::spawn(async move {
            backend.stream(&req_clone, stream_tx).await
        });

        let mut current_tool: Option<AccumulatedToolCall> = None;

        while let Some(delta) = stream_rx.recv().await {
            match delta {
                StreamDelta::TextDelta(text) => {
                    ctx.text_content.push_str(&text);
                    let _ = self.event_tx.send(LoopEvent::TextDelta(text)).await;
                }
                StreamDelta::ToolUseStart { id, name } => {
                    let _ = self.event_tx.send(LoopEvent::ToolCallStart {
                        id: id.clone(), name: name.clone(),
                    }).await;
                    current_tool = Some(AccumulatedToolCall {
                        id, name, input_json: String::new(),
                    });
                }
                StreamDelta::ToolInputDelta(json) => {
                    if let Some(ref mut tool) = current_tool {
                        tool.input_json.push_str(&json);
                    }
                }
                StreamDelta::ToolUseEnd { .. } => {
                    if let Some(tool) = current_tool.take() {
                        ctx.tool_calls.push(tool);
                    }
                }
                StreamDelta::Usage(u) => {
                    let _ = self.event_tx.send(LoopEvent::TokenUsage {
                        input: u.input_tokens, output: u.output_tokens,
                    }).await;
                    ctx.token_usage = u;
                }
                StreamDelta::MessageStop { .. } => {}
                StreamDelta::Error(e) => {
                    error!("API stream error: {}", e);
                    let lower = e.to_lowercase();
                    if lower.contains("context_length") || lower.contains("too many tokens")
                        || lower.contains("max_tokens") || lower.contains("context window")
                    {
                        ctx.context_overflow = true;
                    }
                    ctx.stream_error = Some(e.clone());
                    let _ = self.event_tx.send(LoopEvent::Error(e)).await;
                }
            }
        }

        if let Ok(Err(e)) = stream_handle.await {
            error!("API stream fatal: {}", e);
            if ctx.text_content.is_empty() && ctx.tool_calls.is_empty() {
                ctx.stream_error = Some(format!("API stream error: {}", e));
                let _ = self.event_tx.send(LoopEvent::Error(ctx.stream_error.clone().unwrap())).await;
            }
        }

        ctx.empty_response = ctx.text_content.is_empty()
            && ctx.tool_calls.is_empty()
            && !ctx.context_overflow
            && ctx.token_usage.input_tokens == 0;

        debug!("Stream done: text_len={}, tools={}, usage={}in/{}out",
            ctx.text_content.len(), ctx.tool_calls.len(),
            ctx.token_usage.input_tokens, ctx.token_usage.output_tokens);

        Ok(())
    }
}

/// Convert session Messages to CompletionMessage format
fn build_completion_messages(messages: &[Message]) -> Vec<CompletionMessage> {
    let mut msgs = Vec::new();
    for msg in messages {
        match msg.role {
            Role::System => {}
            Role::User => {
                if msg.attachments.is_empty() {
                    if let Some(ref content) = msg.content {
                        msgs.push(CompletionMessage::User {
                            content: nova_core::llm_backend::CompletionContent::Text(content.clone()),
                        });
                    }
                } else {
                    let mut blocks = Vec::new();
                    if let Some(ref content) = msg.content {
                        if !content.is_empty() {
                            blocks.push(CoreContentBlock::Text { text: content.clone() });
                        }
                    }
                    for att in &msg.attachments {
                        if att.media_type.starts_with("image/") {
                            use base64::Engine as _;
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&att.data);
                            blocks.push(CoreContentBlock::Image {
                                source: nova_core::llm_backend::ImageSource {
                                    source_type: "base64".into(),
                                    media_type: att.media_type.clone(),
                                    data: b64,
                                },
                            });
                        } else {
                            blocks.push(CoreContentBlock::Text {
                                text: format!("[Attached file: {} ({} bytes, {})]", att.filename, att.data.len(), att.media_type),
                            });
                        }
                    }
                    if !blocks.is_empty() {
                        msgs.push(CompletionMessage::User {
                            content: nova_core::llm_backend::CompletionContent::Blocks(blocks),
                        });
                    }
                }
            }
            Role::Assistant => {
                let mut blocks = Vec::new();
                if let Some(ref text) = msg.content {
                    if !text.is_empty() {
                        blocks.push(CoreContentBlock::Text { text: text.clone() });
                    }
                }
                if let Some(ref tcs) = msg.tool_calls {
                    for tc in tcs {
                        let input = if tc.arguments.is_null() {
                            serde_json::json!({})
                        } else {
                            tc.arguments.clone()
                        };
                        blocks.push(CoreContentBlock::ToolUse {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            input,
                        });
                    }
                }
                if !blocks.is_empty() {
                    msgs.push(CompletionMessage::Assistant {
                        content: nova_core::llm_backend::CompletionContent::Blocks(blocks),
                    });
                }
            }
            Role::Tool => {
                if let (Some(ref id), Some(ref content)) = (&msg.tool_call_id, &msg.content) {
                    msgs.push(CompletionMessage::User {
                        content: nova_core::llm_backend::CompletionContent::Blocks(vec![CoreContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: content.clone(),
                        }]),
                    });
                }
            }
        }
    }
    msgs
}
