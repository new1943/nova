use anyhow::Result;
use async_trait::async_trait;
use nova_core::message::{Message, ToolCall};
use nova_memory::session::manager::Session;
use nova_tools::registry::{ToolRegistry, ToolContext};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

use crate::agent_loop::LoopEvent;
use crate::pipeline::context::TurnContext;
use crate::pipeline::stage::PipelineStage;

/// ExecuteStage — runs tool calls and appends results to session
///
/// Reads: tool_calls, allowed_tools
/// Writes: tool_results, session messages, newly_added
pub struct ExecuteStage {
    pub tools: Arc<ToolRegistry>,
    pub tool_context: ToolContext,
    pub tool_timeout: Duration,
    pub event_tx: mpsc::Sender<LoopEvent>,
    pub approval_handler: Option<Arc<dyn nova_core::approval::ApprovalHandler>>,
}

const MAX_TOOL_CHARS: usize = 30_000;

#[async_trait]
impl PipelineStage for ExecuteStage {
    fn name(&self) -> &str { "execute" }

    async fn execute(&self, ctx: &mut TurnContext, session: &mut Session) -> Result<()> {
        if ctx.tool_calls.is_empty() {
            return Ok(());
        }

        // Record assistant message first
        let msg_tool_calls = Some(ctx.tool_calls.iter().map(|tc| ToolCall {
            id: tc.id.clone(),
            name: tc.name.clone(),
            arguments: tc.parse_input().unwrap_or_else(|_| serde_json::json!({})),
        }).collect());

        let content = if ctx.text_content.is_empty() { None } else { Some(ctx.text_content.clone()) };
        let assistant_msg = Message::assistant(content, msg_tool_calls);
        session.add_message(assistant_msg);

        // Execute each tool
        for tc in &ctx.tool_calls {
            let input = tc.parse_input().unwrap_or_else(|_| serde_json::json!({}));
            let input_str = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
            let input_preview = if input_str.len() > 200 {
                format!("{}...[{} chars]", &input_str[..200], input_str.len())
            } else {
                input_str
            };
            debug!("Tool: name={}, args={}", tc.name, input_preview);

            // Check tool availability
            let is_allowed = ctx.allowed_tools.iter().any(|n| n == &tc.name);

            // Approval check
            let approval_denied = if is_allowed {
                if let Some(ref handler) = self.approval_handler {
                    if nova_core::approval::requires_approval(&tc.name) {
                        match handler.request_approval(&tc.name, &input).await {
                            nova_core::approval::ApprovalDecision::Allow => false,
                            nova_core::approval::ApprovalDecision::AllowSession => false,
                            nova_core::approval::ApprovalDecision::Deny => true,
                        }
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            };

            let mut result = if !is_allowed {
                let msg = format!("Tool `{}` is not available. Available: {:?}", tc.name, ctx.allowed_tools);
                warn!("{}", msg);
                format!("{{\"error\": \"{}\"}}", msg)
            } else if approval_denied {
                warn!("Tool `{}` denied", tc.name);
                format!("{{\"error\": \"Tool '{}' was denied\"}}", tc.name)
            } else {
                match self.tools.execute(&tc.name, input.clone(), &self.tool_context, self.tool_timeout).await {
                    Ok(r) => {
                        debug!("Tool result: name={}, len={}", tc.name, r.len());
                        r
                    }
                    Err(e) => {
                        error!("Tool `{}` failed: {}", tc.name, e);
                        format!("{{\"error\": \"{}\"}}", e)
                    }
                }
            };

            // Truncate
            if result.len() > MAX_TOOL_CHARS {
                let omitted = result.len() - MAX_TOOL_CHARS;
                let byte_idx = result.char_indices().nth(MAX_TOOL_CHARS).map(|(i, _)| i).unwrap_or(result.len());
                result.truncate(byte_idx);
                result.push_str(&format!("\n\n... [OUTPUT TRUNCATED - {omitted} chars omitted!]"));
            }

            let _ = self.event_tx.send(LoopEvent::ToolCallResult {
                id: tc.id.clone(), content: result.clone(),
            }).await;

            ctx.tool_results.push((tc.id.clone(), result.clone()));
            let tool_msg = Message::tool_result(&tc.id, &result);
            session.add_message(tool_msg);
        }

        Ok(())
    }
}
