use anyhow::Result;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use crate::message::Message;

/// JSONL session history (append-only)
pub struct SessionHistory {
    path: PathBuf,
}

impl SessionHistory {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Append a single message as one JSON line
    pub fn append(&self, msg: &Message) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let line = serde_json::to_string(msg)?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    /// Clear the JSONL file
    pub fn clear(&self) -> Result<()> {
        if self.path.exists() {
            fs::File::create(&self.path)?; // Truncates the file
        }
        Ok(())
    }

    /// Load all messages from JSONL file
    pub fn load_all(&self) -> Result<Vec<Message>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut messages = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            messages.push(serde_json::from_str(&line)?);
        }
        Ok(messages)
    }
}
