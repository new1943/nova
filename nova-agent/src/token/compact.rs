use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::debug;

use nova_core::message::{Message, Role};

/// Compact mode — dual-layer circuit breaker
#[derive(Debug, Clone, Copy)]
pub enum CompactMode {
    /// 85% ~ 95%: Truncate with [压缩] marker, preserve recent messages
    Graceful,
    /// >95%: Directly drop oldest messages with [强制截断] marker
    Forceful,
}

/// Compact result — returned from compact operations.
#[derive(Debug, Clone, Default)]
pub struct CompactResult;

/// Compact engine
pub struct Compactor {
    target_pct: f32,
    running: AtomicBool,
}

impl Compactor {
    pub fn new(target_pct: f32) -> Self {
        Self {
            target_pct,
            running: AtomicBool::new(false),
        }
    }

    fn decide_mode(budget_pct: f32) -> CompactMode {
        if budget_pct > 0.95 {
            CompactMode::Forceful
        } else {
            CompactMode::Graceful
        }
    }

    pub async fn compact(
        &self,
        messages: &[Message],
        context_window: usize,
        budget_pct: f32,
    ) -> Result<Vec<Message>> {
        let (msgs, _) = self.compact_full(messages, context_window, budget_pct).await?;
        Ok(msgs)
    }

    pub async fn compact_full(
        &self,
        messages: &[Message],
        context_window: usize,
        budget_pct: f32,
    ) -> Result<(Vec<Message>, Option<CompactResult>)> {
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
                Ok((self.graceful_compact(messages, split), Some(CompactResult)))
            }
            CompactMode::Forceful => {
                Ok((self.forceful_compact(messages, split), None))
            }
        }
    }

    fn calculate_split(&self, messages: &[Message], context_window: usize) -> Result<usize> {
        let total_chars: usize = messages
            .iter()
            .map(|m| {
                let content_chars = m.content.as_ref().map(|c| c.len()).unwrap_or(0);
                let tc_chars = m.tool_calls.as_ref().map(|tcs| {
                    tcs.iter().map(|tc| tc.name.len() + tc.arguments.to_string().len()).sum::<usize>()
                }).unwrap_or(0);
                content_chars + tc_chars
            })
            .sum();
        let target_chars = (context_window as f32 * self.target_pct * 4.0) as usize;

        if total_chars <= target_chars {
            return Ok(0);
        }

        let mut keep_chars = 0usize;
        let mut keep_from = messages.len();
        for (i, msg) in messages.iter().enumerate().rev() {
            let content_chars = msg.content.as_ref().map(|c| c.len()).unwrap_or(0);
            let tc_chars = msg.tool_calls.as_ref().map(|tcs| {
                tcs.iter().map(|tc| tc.name.len() + tc.arguments.to_string().len()).sum::<usize>()
            }).unwrap_or(0);
            let msg_chars = content_chars + tc_chars;
            if keep_chars + msg_chars > target_chars && keep_from < messages.len() {
                break;
            }
            keep_chars += msg_chars;
            keep_from = i;
        }

        keep_from = keep_from.min(messages.len().saturating_sub(4));

        let split = self.safe_split_point(messages, keep_from);

        Ok(split)
    }

    fn safe_split_point(&self, messages: &[Message], mut split: usize) -> usize {
        while split > 0 {
            let msg = &messages[split];
            if msg.role == Role::Tool {
                split -= 1;
                continue;
            }
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
}
