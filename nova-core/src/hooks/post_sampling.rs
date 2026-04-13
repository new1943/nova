use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::hooks::{Hook, PostSamplingHook};
use crate::memory::dual_write::DualWriteMemory;
use crate::message::Message;
use crate::session::manager::Session;

/// PostSampling hook: extract key information from LLM response into memory.
/// Runs in forked agent context (non-blocking).
pub struct MemoryExtractHook {
    dual_write: Arc<Mutex<DualWriteMemory>>,
}

impl MemoryExtractHook {
    pub fn new(dual_write: Arc<Mutex<DualWriteMemory>>) -> Self {
        Self { dual_write }
    }
}

impl Hook for MemoryExtractHook {
    fn name(&self) -> &str { "memory_extract_post_sampling" }
}

#[async_trait]
impl PostSamplingHook for MemoryExtractHook {
    async fn run(&self, response: &Message, session: &Session) -> Result<()> {
        // Only process assistant messages with content
        let content = match &response.content {
            Some(c) if !c.is_empty() => c,
            _ => return Ok(()),
        };

        // Simple heuristic: if response mentions decisions, preferences, or key facts,
        // extract and store. In production, this would call LLM for extraction.
        let mut dw = self.dual_write.lock().await;

        // Check dual-write: skip if main agent already wrote this turn
        if dw.has_writes_since(&session.session_id) {
            return Ok(());
        }

        // For now, only store if content is substantial (>200 chars)
        if content.chars().count() > 200 {
            // Safe truncation: take first 100 characters (not bytes)
            let summary: String = content.chars().take(100).collect();
            let summary = format!("{}...", summary);
            dw.write_if_not_written(
                &session.session_id,
                crate::memory::dual_write::MemoryType::Project,
                &summary,
            ).await?;
        }

        Ok(())
    }
}
