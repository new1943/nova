use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use crate::tools::file_tracker::SharedFileReadTracker;
use crate::tools::registry::Tool;

/// Write file tool with read-first check, atomic write, and history backup
pub struct WriteFileTool {
    /// Shared file read tracker
    tracker: SharedFileReadTracker,
}

impl WriteFileTool {
    pub fn new(tracker: SharedFileReadTracker) -> Self {
        Self { tracker }
    }

    fn check_path(&self, path: &str) -> Result<()> {
        let nova_dir = dirs::home_dir()
            .map(|h| h.join(".nova").to_string_lossy().to_string())
            .unwrap_or_default();
        if !nova_dir.is_empty() && path.contains(&nova_dir) {
            anyhow::bail!("Cannot write to ~/.nova/ directory");
        }
        Ok(())
    }

    /// Perform atomic write using temp file + rename
    fn atomic_write(path: &Path, content: &str) -> Result<()> {
        let temp_path = path.with_extension("tmp");
        fs::write(&temp_path, content)?;
        fs::rename(&temp_path, path)?;  // Atomic on POSIX
        Ok(())
    }

    /// Backup file to history directory
    fn backup_to_history(path: &Path) -> Result<Option<String>> {
        let history_dir = dirs::home_dir()
            .map(|h| h.join(".nova/file_history"))
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp/.nova_file_history"));

        // Create history directory if needed
        fs::create_dir_all(&history_dir)?;

        let filename = path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        let timestamp = chrono_lite_timestamp();
        let backup_name = format!("{}_{}", filename, timestamp);
        let backup_path = history_dir.join(&backup_name);

        fs::copy(path, &backup_path)?;

        Ok(Some(backup_path.to_string_lossy().to_string()))
    }
}

/// Get a simple timestamp string (YYYYMMDD_HHMMSS)
fn chrono_lite_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Simple conversion without chrono dependency - just use seconds since epoch
    format!("{}", secs)
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str { "write_file" }

    fn description(&self) -> &str {
        "Write content to a file. Mode: 'overwrite' (default) or 'append'. Requires file to be read first. Uses atomic write and creates backups."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path to write" },
                "content": { "type": "string", "description": "Content to write" },
                "mode": { "type": "string", "enum": ["overwrite", "append"], "description": "Write mode (default: overwrite)" },
                "skip_read_check": { "type": "boolean", "description": "Skip read-first check (dangerous)" }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let path = args.get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'path' field"))?;
        let content = args.get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'content' field"))?;
        let mode = args.get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("overwrite");
        let skip_read_check = args.get("skip_read_check")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let file_path = Path::new(path);
        self.check_path(path)?;

        // Read-first check (unless skipped)
        if !skip_read_check {
            let tracker = self.tracker.lock().await;
            if !tracker.was_read(file_path) {
                anyhow::bail!("File '{}' must be read before writing. Use read_file tool first.", path);
            }
        }

        // Auto-create parent directories
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let backup_path = if mode == "overwrite" && file_path.exists() {
            // Backup existing file
            Self::backup_to_history(file_path)?
        } else {
            None
        };

        match mode {
            "append" => {
                let mut file = OpenOptions::new().create(true).append(true).open(path)?;
                write!(file, "{}", content)?;
            }
            _ => {
                // Use atomic write
                Self::atomic_write(file_path, content)?;
            }
        }

        let backup_msg = backup_path.map(|p| format!(" (backup: {})", p)).unwrap_or_default();
        Ok(format!("Written {} bytes to {}{}", content.len(), path, backup_msg))
    }
}
