use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::hooks::{Hook, StopHook};
use crate::memory::dual_write::{DualWriteMemory, MemoryType};
use crate::message::Role;
use crate::session::manager::Session;

/// StopHook: extract memory at end of turn (serial, blocking).
/// Complements the PostSampling hook — this one has access to the full turn.
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

        // Skip if already written this turn (dual-write mutex)
        if dw.has_writes_since(&session.session_id) {
            return Ok(());
        }

        // Collect assistant messages from this turn that have tool calls
        // (indicates the agent did something worth remembering)
        let tool_actions: Vec<String> = session.messages.iter()
            .rev()
            .take(10) // look at recent messages
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

        // Reset the memory_written flag for next turn
        session.token_stats.memory_written = false;

        Ok(())
    }
}
