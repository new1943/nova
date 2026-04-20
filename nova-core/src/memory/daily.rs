//! Daily Notes — Layer 2 episodic memory (diary) storage.
//!
//! Writes to `~/.nova/memories/YYYY-MM-DD.md` in markdown format.
//! System auto-writes here at Compact time and Session end.
//!
//! Supports two formats:
//! - Legacy: `## HH:MM:SS — EntryType\n\n<content>\n`
//! - Topic Timeline: `## HH:MM — TopicName [Status Icon]\n\n...details...\n`

use anyhow::Result;
use chrono::Local;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use super::topic_state::TopicStatus;

/// Status icon mapping
fn status_icon(status: TopicStatus) -> &'static str {
    match status {
        TopicStatus::Started => "🟢 开始",
        TopicStatus::Active => "🔵 进行中",
        TopicStatus::Suspended => "🟡 挂起",
        TopicStatus::Archived => "⚫ 已归档",
    }
}

/// Topic timeline entry
#[derive(Debug, Clone)]
pub struct DiaryEntry {
    /// Time (HH:MM:SS)
    pub time: String,
    /// Topic name
    pub topic: String,
    /// Topic status
    pub status: TopicStatus,
    /// Detailed description
    pub description: Option<String>,
    /// Key conclusions
    pub conclusions: Vec<String>,
}

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

    /// Append a topic timeline entry to today's daily note.
    /// Format: `## HH:MM — TopicName [Status Icon]\n\n...details...\n`
    pub fn append_topic_entry(&self, entry: &DiaryEntry) -> Result<()> {
        fs::create_dir_all(&self.memories_dir)?;
        let path = self.today_path();
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

        // Format the entry
        let icon = status_icon(entry.status.clone());
        let mut entry_text = format!("## {} — {} [{}]", entry.time, entry.topic, icon);

        if !entry.conclusions.is_empty() {
            entry_text.push_str("\n\n");
            for conclusion in &entry.conclusions {
                entry_text.push_str(&format!("- {}\n", conclusion));
            }
        }

        if let Some(ref desc) = entry.description {
            if !entry.conclusions.is_empty() {
                entry_text.push_str(&format!("\n{}", desc));
            } else {
                entry_text.push_str(&format!("\n\n{}", desc));
            }
        }

        writeln!(file, "{}\n", entry_text)?;
        Ok(())
    }

    /// Append a Compact result as topic timeline entries.
    /// Creates entries for all archived topics.
    pub fn append_compact_topics(&self, archived_topics: &[String], active_summary: &str) -> Result<()> {
        let time = Local::now().format("%H:%M:%S").to_string();

        for topic in archived_topics {
            let entry = DiaryEntry {
                time: time.clone(),
                topic: topic.clone(),
                status: TopicStatus::Archived,
                description: Some(format!("活跃摘要：{}", active_summary)),
                conclusions: Vec::new(),
            };
            self.append_topic_entry(&entry)?;
        }

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
