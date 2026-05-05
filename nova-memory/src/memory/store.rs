use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use crate::memory::dual_write::MemoryType;

/// JSONL memory store — append-only per memory type
pub struct MemoryStore {
    base_path: PathBuf,
}

impl MemoryStore {
    pub fn new(base_path: PathBuf) -> Self {
        Self { base_path }
    }

    /// Append a memory entry to the appropriate JSONL file
    pub fn append(&self, mem_type: MemoryType, content: &str) -> Result<()> {
        fs::create_dir_all(&self.base_path)?;
        let path = self.base_path.join(mem_type.filename());
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let entry = json!({
            "content": content,
            "timestamp": Utc::now().to_rfc3339(),
        });
        writeln!(file, "{}", serde_json::to_string(&entry)?)?;
        Ok(())
    }
}
