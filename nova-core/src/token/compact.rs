use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::message::{Message, Role};

/// Compact engine — Strategy 3: compress early messages into summary
///
/// - Triggered when token budget > 90%
/// - Preserves system prompt + recent messages
/// - Early messages → LLM summary (≤50 chars)
/// - Target: compress to 60% of context_window
/// - Re-entrancy guard: won't trigger while already running
/// - Protects tool_call + tool_result pairs from being split
pub struct Compactor {
    target_pct: f32,
    running: AtomicBool,
    api_key: String,
    api_base_url: String,
    model: String,
}

impl Compactor {
    pub fn new(target_pct: f32, api_key: String, api_base_url: String, model: String) -> Self {
        Self {
            target_pct,
            running: AtomicBool::new(false),
            api_key,
            api_base_url,
            model,
        }
    }

    /// Compact messages: summarize early messages, keep recent ones.
    /// Returns new message list with summary replacing early messages.
    pub async fn compact(
        &self,
        messages: &[Message],
        context_window: usize,
    ) -> Result<Vec<Message>> {
        // Re-entrancy guard
        if self.running.swap(true, Ordering::SeqCst) {
            anyhow::bail!("Compact already running");
        }
        let result = self.do_compact(messages, context_window).await;
        self.running.store(false, Ordering::SeqCst);
        result
    }

    async fn do_compact(
        &self,
        messages: &[Message],
        context_window: usize,
    ) -> Result<Vec<Message>> {
        if messages.is_empty() {
            return Ok(Vec::new());
        }

        // Target: keep messages that fit within target_pct of context_window
        // Heuristic: ~4 chars per token, estimate total tokens
        let total_chars: usize = messages.iter()
            .filter_map(|m| m.content.as_ref())
            .map(|c| c.len())
            .sum();
        let target_chars = (context_window as f32 * self.target_pct * 4.0) as usize;

        // If already under target, no need to compact
        if total_chars <= target_chars {
            return Ok(messages.to_vec());
        }

        // Calculate how many recent messages to keep (by char budget)
        let mut keep_chars = 0usize;
        let mut keep_from = messages.len();
        for (i, msg) in messages.iter().enumerate().rev() {
            let msg_chars = msg.content.as_ref().map(|c| c.len()).unwrap_or(0);
            if keep_chars + msg_chars > target_chars && keep_from < messages.len() {
                break;
            }
            keep_chars += msg_chars;
            keep_from = i;
        }

        // Ensure we keep at least 4 messages
        keep_from = keep_from.min(messages.len().saturating_sub(4));

        // Adjust split to not break tool_call + tool_result pairs
        let split = self.safe_split_point(messages, keep_from);

        if split == 0 {
            return Ok(messages.to_vec());
        }

        let early = &messages[..split];
        let recent = &messages[split..];

        // Generate summary of early messages via LLM
        let summary = self.summarize(early).await?;

        let mut result = Vec::with_capacity(recent.len() + 1);
        result.push(Message::user(format!("[对话摘要] {}", summary)));
        result.extend_from_slice(recent);

        Ok(result)
    }

    /// Find a safe split point that doesn't break tool_call/tool_result pairs
    fn safe_split_point(&self, messages: &[Message], mut split: usize) -> usize {
        // Walk backward from split to find a safe boundary
        while split > 0 {
            let msg = &messages[split];
            // Don't split right before a tool result
            if msg.role == Role::Tool {
                split -= 1;
                continue;
            }
            // Don't split right after an assistant message with tool_calls
            if split > 0 {
                let prev = &messages[split - 1];
                if prev.role == Role::Assistant && prev.tool_calls.is_some() {
                    split -= 1;
                    continue;
                }
            }
            break;
        }
        split
    }

    /// Call LLM to summarize early messages (≤50 chars target)
    async fn summarize(&self, messages: &[Message]) -> Result<String> {
        let mut conversation = String::new();
        for msg in messages {
            let role = match msg.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::Tool => "Tool",
                Role::System => "System",
            };
            if let Some(ref content) = msg.content {
                conversation.push_str(&format!("{}: {}\n", role, content));
            }
        }

        // Truncate if too long for the summary request (char-safe)
        let conversation = if conversation.chars().count() > 4000 {
            let truncated: String = conversation.chars().take(4000).collect();
            format!("{}...(truncated)", truncated)
        } else {
            conversation
        };

        let api = nova_api::client::ApiClient::new(
            self.api_key.clone(),
            self.api_base_url.clone(),
        );

        let req = nova_api::types::ApiRequest {
            model: self.model.clone(),
            max_tokens: 200,
            system: "Summarize the following conversation in ≤50 Chinese characters. Be concise.".into(),
            messages: vec![nova_api::types::ApiMessage::User {
                content: nova_api::types::Content::Text(conversation),
            }],
            tools: vec![],
            stream: false,
        };

        let resp = api.complete(&req).await?;

        // Extract text from response
        for block in &resp.content {
            if let nova_api::types::ContentBlock::Text { text } = block {
                return Ok(text.clone());
            }
        }

        Ok("对话摘要不可用".into())
    }
}
