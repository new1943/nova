use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::hooks::{Hook, PostSamplingHook};
use nova_memory::memory::{DualWriteMemory, MemoryType};
use nova_core::message::Message;
use nova_memory::session::manager::Session;

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
        let content = match &response.content {
            Some(c) if !c.is_empty() => c,
            _ => return Ok(()),
        };

        let mut dw = self.dual_write.lock().await;

        if dw.has_writes_since(&session.session_id) {
            return Ok(());
        }

        if content.chars().count() > 200 {
            let summary: String = content.chars().take(100).collect();
            let summary = format!("{}...", summary);
            dw.write_if_not_written(
                &session.session_id,
                MemoryType::Project,
                &summary,
            ).await?;
        }

        Ok(())
    }
}
