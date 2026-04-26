use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::debug;

use crate::message::{Message, Role};

/// Compact mode — dual-layer circuit breaker
#[derive(Debug, Clone, Copy)]
pub enum CompactMode {
    /// 85% ~ 95%: Truncate with [压缩] marker, preserve recent messages
    Graceful,
    /// >95%: Directly drop oldest messages with [强制截断] marker
    Forceful,
}

/// Compact result — [V4 DEPRECATED]
/// LLM summarization has been moved to MemoryKeeper.
/// This struct is kept for API compatibility but all fields will be empty.
#[derive(Debug, Clone, Default)]
pub struct CompactResult {
    /// [DEPRECATED] 已归档的话题列表 — now handled by MemoryKeeper
    pub archived_topics: Vec<String>,
    /// [DEPRECATED] 提取的用户偏好 — now handled by MemoryKeeper
    pub extracted_preferences: Vec<String>,
    /// [DEPRECATED] 当前活跃话题摘要 — now handled by MemoryKeeper
    pub active_summary: String,
}

/// Compact engine — [V4 DEPRECATED LLM summarization]
///
/// - Triggered when token budget > 90%
/// - Dual-layer circuit breaker: Graceful (85-95%) vs Forceful (>95%)
/// - Preserves system prompt + recent messages
/// - Re-entrancy guard: won't trigger while already running
/// - Protects tool_call + tool_result pairs from being split
///
/// [V4 CHANGE] No longer calls LLM for structured summary.
/// Memory extraction is delegated to MemoryKeeper via ShadowEvent bus.
pub struct Compactor {
    target_pct: f32,
    running: AtomicBool,
}

impl Compactor {
    #[allow(dead_code)]
    pub fn new(target_pct: f32) -> Self {
        Self {
            target_pct,
            running: AtomicBool::new(false),
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
            debug!("Compact skipped: total_chars under target");
            return Ok((messages.to_vec(), None));
        }

        let mode = Self::decide_mode(budget_pct);
        debug!("Compact: budget_pct={:.1}%, mode={:?}, total_msgs={}, split={}",
            budget_pct * 100.0, mode, messages.len(), split);

        match mode {
            CompactMode::Graceful => {
                // [V4 DEPRECATED] CompactResult fields are now empty - memory extraction moved to MemoryKeeper
                Ok((self.graceful_compact(messages, split), Some(CompactResult::default())))
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
        debug!("Forceful compact: drop {} oldest messages, keep {} recent", split, messages.len() - split);
        let recent: Vec<Message> = messages[split..].to_vec();

        let mut new_messages = Vec::with_capacity(recent.len() + 1);
        new_messages.push(Message::system(
            "[Context window critically high, oldest messages forcefully dropped.]",
        ));
        new_messages.extend_from_slice(&recent);

        new_messages
    }

    /// Graceful compact: truncate oldest messages with [已压缩] marker.
    /// [V4 DEPRECATED] LLM summarization removed - memory extraction now handled by MemoryKeeper.
    fn graceful_compact(&self, messages: &[Message], split: usize) -> Vec<Message> {
        debug!("Graceful compact: drop {} oldest messages, keep {} recent", split, messages.len() - split);
        let recent: Vec<Message> = messages[split..].to_vec();

        let mut new_messages = Vec::with_capacity(recent.len() + 1);
        new_messages.push(Message::system(
            "[上下文已压缩，早期消息已丢弃。详情见 MEMORY.md]",
        ));
        new_messages.extend_from_slice(&recent);

        new_messages
    }

    /// [DEPRECATED] Only kept for test compatibility
    #[allow(dead_code)]
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
