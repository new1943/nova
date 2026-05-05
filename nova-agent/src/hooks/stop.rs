use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::hooks::{Hook, StopHook};
use nova_memory::memory::{DualWriteMemory, MemoryType};
use nova_core::message::Role;
use nova_memory::session::manager::Session;

/// StopHook: extract memory at end of turn (serial, blocking).
pub struct MemoryExtractStopHook {
    dual_write: Arc<Mutex<DualWriteMemory>>,
}

impl MemoryExtractStopHook {
    pub fn new(dual_write: Arc<Mutex<DualWriteMemory>>) -> Self {
        Self { dual_write }
    }
}

impl Hook for MemoryExtractStopHook {
    fn name(&self) -> &str { "memory_extract_stop" }
}

#[async_trait]
impl StopHook for MemoryExtractStopHook {
    async fn run(&self, session: &mut Session) -> Result<()> {
        let mut dw = self.dual_write.lock().await;

        if dw.has_writes_since(&session.session_id) {
            return Ok(());
        }

        let tool_actions: Vec<String> = session.messages.iter()
            .rev()
            .take(10)
            .filter(|m| m.role == Role::Assistant && m.tool_calls.is_some())
            .filter_map(|m| {
                m.tool_calls.as_ref().map(|tcs| {
                    tcs.iter()
                        .map(|tc| format!("{}({})", tc.name, tc.id))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
            })
            .collect();

        if !tool_actions.is_empty() {
            let summary = format!("Turn {}: tools used — {}", 
                session.turn_count, tool_actions.join("; "));
            dw.write_and_mark(&session.session_id, MemoryType::Project, &summary).await?;
        }

        Ok(())
    }
}
