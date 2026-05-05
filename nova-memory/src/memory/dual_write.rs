use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::time::Instant;

use crate::memory::store::MemoryStore;

/// Memory type categories
#[derive(Debug, Clone, Copy)]
pub enum MemoryType {
    User,
    Feedback,
    Project,
    Reference,
}

impl MemoryType {
    pub fn filename(&self) -> &str {
        match self {
            Self::User => "user.jsonl",
            Self::Feedback => "feedback.jsonl",
            Self::Project => "project.jsonl",
            Self::Reference => "reference.jsonl",
        }
    }
}

/// Strategy 7: Dual-write mutex memory system
///
/// - Main agent writes → mark written → forked agent skips
/// - Main agent didn't write → forked agent writes (fallback)
/// - Never duplicate writes
pub struct DualWriteMemory {
    store: MemoryStore,
    written_since: HashMap<String, Instant>,
}

impl DualWriteMemory {
    pub fn new(base_path: PathBuf) -> Self {
        Self {
            store: MemoryStore::new(base_path),
            written_since: HashMap::new(),
        }
    }

    /// Check if memory was written since this session started
    pub fn has_writes_since(&self, session_id: &str) -> bool {
        self.written_since.contains_key(session_id)
    }

    /// Write memory entry and mark as written
    pub async fn write_and_mark(
        &mut self,
        session_id: &str,
        mem_type: MemoryType,
        content: &str,
    ) -> Result<()> {
        self.store.append(mem_type, content)?;
        self.mark_written(session_id);
        Ok(())
    }

    /// Write memory only if not already written this turn (forked agent fallback)
    pub async fn write_if_not_written(
        &mut self,
        session_id: &str,
        mem_type: MemoryType,
        content: &str,
    ) -> Result<bool> {
        if self.has_writes_since(session_id) {
            return Ok(false);
        }
        self.store.append(mem_type, content)?;
        self.mark_written(session_id);
        Ok(true)
    }

    /// Simple write (used by hooks)
    pub async fn write(&mut self, mem_type: MemoryType, content: &str) -> Result<()> {
        self.store.append(mem_type, content)
    }

    /// Mark that memory was written for this session
    pub fn mark_written(&mut self, session_id: &str) {
        self.written_since.insert(session_id.to_string(), Instant::now());
    }

    /// Clear write marker (call at start of new turn)
    pub fn clear_marker(&mut self, session_id: &str) {
        self.written_since.remove(session_id);
    }
}
