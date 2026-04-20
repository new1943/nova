use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::message::{Message, Role};

/// Compact mode — dual-layer circuit breaker
#[derive(Debug, Clone, Copy)]
pub enum CompactMode {
    /// 85% ~ 95%: Call LLM for structured JSON summary
    Graceful,
    /// >95%: Directly drop oldest messages
    Forceful,
}

/// Compact result from structured JSON extraction
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CompactResult {
    /// 已归档的话题列表
    pub archived_topics: Vec<String>,
    /// 提取的用户偏好
    pub extracted_preferences: Vec<String>,
    /// 当前活跃话题摘要
    pub active_summary: String,
}

/// Compact engine — Strategy 3: compress early messages into summary
///
/// - Triggered when token budget > 90%
/// - Dual-layer circuit breaker: Graceful (85-95%) vs Forceful (>95%)
/// - Preserves system prompt + recent messages
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

    /// Determine compact mode based on budget percentage
    fn decide_mode(budget_pct: f32) -> CompactMode {
        if budget_pct > 0.95 {
            CompactMode::Forceful
        } else {
            CompactMode::Graceful
        }
    }

    /// Compact messages: summarize early messages, keep recent ones.
    /// Returns new message list with summary replacing early messages.
    pub async fn compact(
        &self,
        messages: &[Message],
        context_window: usize,
        budget_pct: f32,
    ) -> Result<Vec<Message>> {
        let (msgs, _) = self.compact_full(messages, context_window, budget_pct).await?;
        Ok(msgs)
    }

    /// Full compact: returns both the compacted messages and the structured result.
    /// This allows callers to update MemoryBoard and TopicTracker with the result.
    pub async fn compact_full(
        &self,
        messages: &[Message],
        context_window: usize,
        budget_pct: f32,
    ) -> Result<(Vec<Message>, Option<CompactResult>)> {
        // Re-entrancy guard
        if self.running.swap(true, Ordering::SeqCst) {
            anyhow::bail!("Compact already running");
        }
        let result = self.do_compact_full(messages, context_window, budget_pct).await;
        self.running.store(false, Ordering::SeqCst);
        result
    }

    async fn do_compact_full(
        &self,
        messages: &[Message],
        context_window: usize,
        budget_pct: f32,
    ) -> Result<(Vec<Message>, Option<CompactResult>)> {
        if messages.is_empty() {
            return Ok((Vec::new(), None));
        }

        // Calculate split point
        let split = self.calculate_split(messages, context_window)?;

        if split == 0 {
            return Ok((messages.to_vec(), None));
        }

        let mode = Self::decide_mode(budget_pct);

        match mode {
            CompactMode::Graceful => {
                let result = self.graceful_compact_full(messages, split).await?;
                Ok((result.0, Some(result.1)))
            }
            CompactMode::Forceful => {
                Ok((self.forceful_compact(messages, split), None))
            }
        }
    }

    /// Calculate the split point (how many messages to compact)
    fn calculate_split(&self, messages: &[Message], context_window: usize) -> Result<usize> {
        // Target: keep messages that fit within target_pct of context_window
        // Heuristic: ~4 chars per token, estimate total tokens
        let total_chars: usize = messages
            .iter()
            .filter_map(|m| m.content.as_ref())
            .map(|c| c.len())
            .sum();
        let target_chars = (context_window as f32 * self.target_pct * 4.0) as usize;

        // If already under target, no need to compact
        if total_chars <= target_chars {
            return Ok(0);
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

        Ok(split)
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

    /// Forceful compact: directly drop oldest messages without LLM summarization
    fn forceful_compact(&self, messages: &[Message], split: usize) -> Vec<Message> {
        let recent: Vec<Message> = messages[split..].to_vec();

        let mut new_messages = Vec::with_capacity(recent.len() + 1);
        new_messages.push(Message::system(
            "[Context window critically high, oldest messages forcefully dropped.]",
        ));
        new_messages.extend_from_slice(&recent);

        new_messages
    }

    /// Graceful compact: call LLM for structured JSON summary, then write to memory
    async fn graceful_compact_full(&self, messages: &[Message], split: usize) -> Result<(Vec<Message>, CompactResult)> {
        let early = &messages[..split];
        let recent = &messages[split..];

        // Call LLM for structured JSON summary
        let json_output = self.llm_structured_summary(early).await?;

        // Clean JSON: remove markdown fences
        let cleaned = Self::clean_json(&json_output);

        // Parse JSON (with fallback to forceful if parsing fails)
        let result: CompactResult = match serde_json::from_str(&cleaned) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Compact JSON parse failed: {}, falling back to forceful", e);
                return Ok((self.forceful_compact(messages, split), CompactResult {
                    archived_topics: Vec::new(),
                    extracted_preferences: Vec::new(),
                    active_summary: String::new(),
                }));
            }
        };

        // Build new message list
        let mut new_messages = Vec::with_capacity(recent.len() + 2);

        // Insert archived topics as system message
        if !result.archived_topics.is_empty() {
            let archived_text = format!(
                "[话题已归档：{}。]",
                result.archived_topics.join("、")
            );
            new_messages.push(Message::system(archived_text));
        }

        // Insert summary
        let summary_text = format!("[对话摘要] {}", result.active_summary);
        new_messages.push(Message::user(summary_text));
        new_messages.extend_from_slice(recent);

        Ok((new_messages, result))
    }

    /// Clean LLM output: remove markdown code block fences
    fn clean_json(raw: &str) -> String {
        let trimmed = raw.trim();

        // Remove ```json ... ``` wrapper
        if let Ok(re) = regex::Regex::new(r"^```json\s*\n?([\s\S]*?)\n?```$") {
            if let Some(caps) = re.captures(trimmed) {
                return caps.get(1).unwrap().as_str().trim().to_string();
            }
        }

        // Remove ``` ... ``` wrapper
        if let Ok(re) = regex::Regex::new(r"^```\s*\n?([\s\S]*?)\n?```$") {
            if let Some(caps) = re.captures(trimmed) {
                return caps.get(1).unwrap().as_str().trim().to_string();
            }
        }

        trimmed.to_string()
    }

    /// Call LLM to generate structured JSON summary
    async fn llm_structured_summary(&self, messages: &[Message]) -> Result<String> {
        let conversation = self.format_messages_for_summary(messages);

        let system = r#"你正在执行记忆整理。请分析历史对话，输出严谨JSON：

{
  "archived_topics": ["话题名1", "话题名2"],
  "extracted_preferences": ["偏好1", "偏好2"],
  "active_summary": "当前话题的一句话描述"
}

要求：
- archived_topics：已完结或明显不再讨论的话题
- extracted_preferences：极其确定的用户偏好和事实，切勿臆测
- active_summary：当前仍在继续的话题摘要
只输出JSON，不要其他文字。"#;

        let api = nova_api::client::ApiClient::new(
            self.api_key.clone(),
            self.api_base_url.clone(),
        );

        let req = nova_api::types::ApiRequest {
            model: self.model.clone(),
            max_tokens: 500,
            system: system.to_string(),
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

        anyhow::bail!("No text in LLM response")
    }

    /// Format messages for summary generation
    fn format_messages_for_summary(&self, messages: &[Message]) -> String {
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

        // Truncate if too long for the summary request
        if conversation.chars().count() > 4000 {
            let truncated: String = conversation.chars().take(4000).collect();
            format!("{}...(truncated)", truncated)
        } else {
            conversation
        }
    }

    /// Call LLM to summarize early messages (≤50 chars target) — legacy fallback
    #[allow(dead_code)]
    async fn summarize(&self, messages: &[Message]) -> Result<String> {
        let conversation = self.format_messages_for_summary(messages);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decide_mode_graceful() {
        assert!(matches!(Compactor::decide_mode(0.90), CompactMode::Graceful));
        assert!(matches!(Compactor::decide_mode(0.92), CompactMode::Graceful));
        assert!(matches!(Compactor::decide_mode(0.95), CompactMode::Graceful));
    }

    #[test]
    fn test_decide_mode_forceful() {
        assert!(matches!(Compactor::decide_mode(0.96), CompactMode::Forceful));
        assert!(matches!(Compactor::decide_mode(0.99), CompactMode::Forceful));
        assert!(matches!(Compactor::decide_mode(1.0), CompactMode::Forceful));
    }

    #[test]
    fn test_clean_json_with_json_fence() {
        let input = "```json\n{\"foo\": \"bar\"}\n```";
        let result = Compactor::clean_json(input);
        assert_eq!(result, "{\"foo\": \"bar\"}");
    }

    #[test]
    fn test_clean_json_with_plain_fence() {
        let input = "```\n{\"foo\": \"bar\"}\n```";
        let result = Compactor::clean_json(input);
        assert_eq!(result, "{\"foo\": \"bar\"}");
    }

    #[test]
    fn test_clean_json_without_fence() {
        let input = "{\"foo\": \"bar\"}";
        let result = Compactor::clean_json(input);
        assert_eq!(result, "{\"foo\": \"bar\"}");
    }

    #[test]
    fn test_clean_json_with_extra_whitespace() {
        let input = "  ```json\n  {\"foo\": \"bar\"}\n  ```  ";
        let result = Compactor::clean_json(input);
        assert_eq!(result, "{\"foo\": \"bar\"}");
    }
}
