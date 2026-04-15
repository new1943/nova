//! Daily Notes — Layer 2 episodic memory (diary) storage.
//!
//! Writes to `~/.nova/memories/YYYY-MM-DD.md` in markdown format.
//! System auto-writes here at Compact time and Session end.

use anyhow::Result;
use chrono::Local;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

/// Daily notes manager — writes to `~/.nova/memories/YYYY-MM-DD.md`
#[derive(Clone)]
pub struct DailyNotes {
    memories_dir: PathBuf,
}

impl DailyNotes {
    pub fn new(memories_dir: PathBuf) -> Self {
        // Ensure memories/ subdirectory exists (not memory/)
        let memories_dir = memories_dir.join("memories");
        Self { memories_dir }
    }

    /// Get today's note file path: ~/.nova/memories/YYYY-MM-DD.md
    fn today_path(&self) -> PathBuf {
        let date = Local::now().format("%Y-%m-%d").to_string();
        self.memories_dir.join(format!("{}.md", date))
    }

    /// Append a timestamped entry to today's daily note.
    /// Format: `## HH:MM:SS — Session Summary\n\n<content>\n`
    pub fn append(&self, content: &str, entry_type: &str) -> Result<()> {
        fs::create_dir_all(&self.memories_dir)?;
        let path = self.today_path();
        let time = Local::now().format("%H:%M:%S").to_string();
        let date = Local::now().format("%Y-%m-%d").to_string();

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        // If this is a new file (new day), write the date header first
        let file_size = file.metadata().map(|m| m.len()).unwrap_or(0);
        if file_size == 0 {
            writeln!(file, "# {}\n", date)?;
        }

        writeln!(file, "## {} — {}\n\n{}\n", time, entry_type, content)?;
        Ok(())
    }

    /// Append a Compact summary entry (called before message compression).
    pub fn append_compact(&self, summary: &str) -> Result<()> {
        self.append(summary, "Compact Summary")
    }

    /// Append a session summary entry (called at session end).
    pub fn append_session(&self, summary: &str) -> Result<()> {
        self.append(summary, "Session Summary")
    }

    /// Read today's notes (empty string if none).
    pub fn read_today(&self) -> String {
        fs::read_to_string(self.today_path()).unwrap_or_default()
    }

    /// Read yesterday's notes (empty string if none).
    pub fn read_yesterday(&self) -> String {
        let yesterday = (Local::now() - chrono::Duration::days(1))
            .format("%Y-%m-%d")
            .to_string();
        let path = self.memories_dir.join(format!("{}.md", yesterday));
        fs::read_to_string(path).unwrap_or_default()
    }

    /// List all diary files with their dates.
    pub fn list_diaries(&self) -> Vec<(String, PathBuf)> {
        let mut diaries = Vec::new();
        let entries = match std::fs::read_dir(&self.memories_dir) {
            Ok(e) => e,
            Err(_) => return diaries,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let filename = path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let date = filename.trim_end_matches(".md").to_string();
            if date.len() == 10 && date.chars().all(|c| c.is_ascii_digit() || c == '-') {
                diaries.push((date, path));
            }
        }
        diaries.sort_by(|a, b| b.0.cmp(&a.0)); // newest first
        diaries
    }

    /// Read a specific diary file by date.
    pub fn read_diary(&self, date: &str) -> String {
        let path = self.memories_dir.join(format!("{}.md", date));
        fs::read_to_string(path).unwrap_or_default()
    }

    /// Get the memories directory path.
    pub fn memories_dir(&self) -> &PathBuf {
        &self.memories_dir
    }
}
