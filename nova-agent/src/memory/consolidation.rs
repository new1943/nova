use anyhow::Result;
use async_trait::async_trait;
use nova_core::memory_layer::MemoryLayer;
use nova_memory::memory::consolidate::MemoryConsolidator;

/// ConsolidationMemory — wraps MemoryConsolidator as a MemoryLayer
///
/// Layer 3: idle-time dual-write consolidation. Extracts memories from
/// conversation transcripts during idle periods (>15min since last activity).
#[derive(Clone)]
pub struct ConsolidationMemory {
    inner: MemoryConsolidator,
}

impl ConsolidationMemory {
    pub fn new(consolidator: MemoryConsolidator) -> Self {
        Self { inner: consolidator }
    }

    /// Run consolidation on session messages (delegates to MemoryConsolidator)
    pub async fn consolidate_session(
        &self,
        messages: &[nova_core::message::Message],
        sweep_index: usize,
        mutex: bool,
    ) -> Result<bool> {
        self.inner.consolidate(messages, sweep_index, mutex).await
    }
}

#[async_trait]
impl MemoryLayer for ConsolidationMemory {
    fn name(&self) -> &str { "consolidation" }

    async fn store(&self, _content: &str) -> Result<()> {
        // Consolidation doesn't store individual entries — it processes transcripts
        Ok(())
    }

    async fn recall(&self, _query: &str, _limit: usize) -> Result<Vec<String>> {
        // Consolidation doesn't recall — it writes to MEMORY.md
        Ok(vec![])
    }

    async fn consolidate(&self) -> Result<()> {
        // No-op without session context; use consolidate_session() directly
        Ok(())
    }
}
