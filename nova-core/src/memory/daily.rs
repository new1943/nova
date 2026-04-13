use anyhow::Result;
use chrono::Utc;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

/// Daily notes manager — writes to ~/.nova/memory/YYYY-MM-DD.md
pub struct DailyNotes {
    memory_dir: PathBuf,
}

impl DailyNotes {
    pub fn new(memory_dir: PathBuf) -> Self {
        Self { memory_dir }
    }

    /// Get today's note file path
    fn today_path(&self) -> PathBuf {
        let date = Utc::now().format("%Y-%m-%d").to_string();
        self.memory_dir.join(format!("{}.md", date))
    }

    /// Append an entry to today's daily note
    pub fn append(&self, entry: &str) -> Result<()> {
        fs::create_dir_all(&self.memory_dir)?;
        let path = self.today_path();
        let timestamp = Utc::now().format("%H:%M:%S").to_string();

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        writeln!(file, "\n- [{}] {}", timestamp, entry)?;
        Ok(())
    }

    /// Read today's notes (empty string if none)
    pub fn read_today(&self) -> String {
        fs::read_to_string(self.today_path()).unwrap_or_default()
    }

    /// Read yesterday's notes (empty string if none)
    pub fn read_yesterday(&self) -> String {
        let yesterday = (Utc::now() - chrono::Duration::days(1))
            .format("%Y-%m-%d")
            .to_string();
        let path = self.memory_dir.join(format!("{}.md", yesterday));
        fs::read_to_string(path).unwrap_or_default()
    }
}
