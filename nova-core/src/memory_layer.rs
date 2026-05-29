use anyhow::Result;
use async_trait::async_trait;

/// MemoryLayer — unified interface for the 4-tier memory system
///
/// Each tier (Working/Episodic/Consolidation/Dream) implements this trait.
/// The Pipeline uses it for recall (inject into prompt) and store (persist after turn).
#[async_trait]
pub trait MemoryLayer: Send + Sync {
    /// Layer name for logging (e.g. "working", "episodic")
    fn name(&self) -> &str;

    /// Write a memory entry
    async fn store(&self, content: &str) -> Result<()>;

    /// Recall relevant memories for a query (injected into system prompt)
    async fn recall(&self, query: &str, limit: usize) -> Result<Vec<String>>;

    /// Idle-time consolidation (optional, default no-op)
    async fn consolidate(&self) -> Result<()> {
        Ok(())
    }
}
