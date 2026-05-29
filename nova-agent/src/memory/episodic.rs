use anyhow::Result;
use async_trait::async_trait;
use nova_core::memory_layer::MemoryLayer;
use nova_memory::memory::daily::DailyNotes;

/// EpisodicMemory — wraps DailyNotes as a MemoryLayer
///
/// Layer 2: daily journal entries, compact summaries, session diaries.
/// Stores are written to today's diary. Recall reads today + yesterday.
#[derive(Clone)]
pub struct EpisodicMemory {
    inner: DailyNotes,
}

impl EpisodicMemory {
    pub fn new(daily: DailyNotes) -> Self {
        Self { inner: daily }
    }

    /// Delegate to DailyNotes::append_compact
    pub fn append_compact(&self, summary: &str) -> anyhow::Result<()> {
        self.inner.append_compact(summary)
    }

    /// Delegate to DailyNotes::append_session
    pub fn append_session(&self, summary: &str) -> anyhow::Result<()> {
        self.inner.append_session(summary)
    }
}

#[async_trait]
impl MemoryLayer for EpisodicMemory {
    fn name(&self) -> &str { "episodic" }

    async fn store(&self, content: &str) -> Result<()> {
        self.inner.append(content, "note")
    }

    async fn recall(&self, _query: &str, _limit: usize) -> Result<Vec<String>> {
        let today = self.inner.read_today();
        let yesterday = self.inner.read_yesterday();
        let mut results = Vec::new();
        if !today.is_empty() {
            results.push(format!("[Today]\n{}", today));
        }
        if !yesterday.is_empty() {
            results.push(format!("[Yesterday]\n{}", yesterday));
        }
        Ok(results)
    }
}
