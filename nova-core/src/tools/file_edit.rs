use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

use crate::tools::file_tracker::SharedFileReadTracker;
use crate::tools::registry::Tool;

/// FileEdit tool — precise string replacement in files.
/// Replaces exact `old_string` with `new_string`.
/// Fails if old_string is not found or matches multiple times (unless replace_all=true).
pub struct FileEditTool {
    /// Shared file read tracker
    tracker: SharedFileReadTracker,
}

impl FileEditTool {
    pub fn new(tracker: SharedFileReadTracker) -> Self {
        Self { tracker }
    }

    fn check_path(&self, _path: &str) -> Result<()> {
        Ok(())
    }

    /// Perform atomic write using temp file + rename
    fn atomic_write(path: &Path, content: &str) -> Result<()> {
        let temp_path = path.with_extension("tmp");
        fs::write(&temp_path, content)?;
        fs::rename(&temp_path, path)?;  // Atomic on POSIX
        Ok(())
    }

    /// Generate unified diff format (simplified)
    fn generate_diff(old_content: &str, new_content: &str, file_path: &str) -> String {
        // Simplified diff - shows that a change occurred
        let old_lines = old_content.lines().count();
        let new_lines = new_content.lines().count();

        format!(
            "--- a/{}\n+++ b/{}\n@@ -{},{} +{},{} @@\n [content changed]",
            file_path, file_path,
            1, old_lines,
            1, new_lines
        )
    }
}

// ─── T31 Placeholder Items ───────────────────────────────────────────────────
// The following items were marked as deferred in tasks.md T31:
//
// 1. LSP 通知占位 (LSP Notification Placeholder):
//    When a file is edited, we should notify language servers about the change.
//    In Rust, the dominant LSP is rust-analyzer, which watches files via fs_watcher
//    and automatically detects changes. For a more explicit notification approach,
//    one would call rust-analyzer's `textDocument/didChange` endpoint via LSP JSON-RPC.
//    This is marked deferred because the Rust LSP ecosystem is less mature than
//    TypeScript's, and rust-analyzer's auto-watch is usually sufficient.
//
// 2. Quote 规范化占位 (Quote Normalization Placeholder):
//    Handle mixed curly quotes (") and straight quotes (") in old_string/new_string.
//    The LLM sometimes generates curly quotes while the file contains straight quotes
//    (or vice versa), causing edit failures. A normalization step would convert
//    all quote characters to a consistent form before matching.
//    Implementation would use unicode-aware character replacement in old_string/new_string.

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str { "file_edit" }

    fn description(&self) -> &str {
        "Perform exact string replacement in a file. Replaces old_string with new_string. \
         old_string must match exactly (including whitespace/indentation). \
         Fails if old_string not found or matches multiple locations (use replace_all=true for all occurrences). \
         Requires file to be read first. Uses atomic write. Returns structured patch."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact text to find and replace (must match precisely including whitespace)"
                },
                "new_string": {
                    "type": "string",
                    "description": "The replacement text"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences (default: false, fails if >1 match)"
                }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let file_path_str = args.get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'file_path'"))?;
        let old_string = args.get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'old_string'"))?;
        let new_string = args.get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'new_string'"))?;
        let replace_all = args.get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Expand ~ to home directory
        let file_path_str = if file_path_str.starts_with("~/") {
            if let Some(home) = dirs::home_dir() {
                file_path_str.replacen("~", &home.to_string_lossy(), 1)
            } else {
                file_path_str.to_string()
            }
        } else {
            file_path_str.to_string()
        };

        let file_path = Path::new(&file_path_str);
        self.check_path(&file_path_str)?;

        // Read-first check
        {
            let tracker = self.tracker.lock().await;
            if !tracker.was_read(file_path) {
                anyhow::bail!("File '{}' must be read before editing. Use read_file tool first.", file_path_str);
            }

            // Check for concurrent modification
            if let Ok(metadata) = fs::metadata(&file_path_str) {
                if let Ok(current_mtime) = metadata.modified() {
                    if tracker.was_modified_since_read(file_path, current_mtime) {
                        anyhow::bail!(
                            "File '{}' was modified since it was read. \
                             The file may have been changed by an external process (linter, user, etc). \
                             Please read the file again before making edits.",
                            file_path_str
                        );
                    }
                }
            }
        }

        if old_string == new_string {
            anyhow::bail!("old_string and new_string are identical — nothing to change");
        }

        if old_string.is_empty() {
            anyhow::bail!("old_string cannot be empty");
        }

        // Read file
        let content = fs::read_to_string(&file_path_str)
            .map_err(|e| anyhow::anyhow!("Cannot read '{}': {}", file_path_str, e))?;

        let original_content = content.clone();

        // Count occurrences
        let match_count = content.matches(old_string).count();

        if match_count == 0 {
            // Provide helpful context: show a snippet of the file
            let preview: String = content.chars().take(200).collect();
            anyhow::bail!(
                "old_string not found in '{}'. File starts with:\n{}{}",
                file_path_str,
                preview,
                if content.len() > 200 { "..." } else { "" }
            );
        }

        if match_count > 1 && !replace_all {
            anyhow::bail!(
                "old_string found {} times in '{}'. Use replace_all=true to replace all, \
                 or provide more context in old_string to make it unique.",
                match_count, file_path_str
            );
        }

        // Perform replacement
        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            // Replace only first occurrence
            content.replacen(old_string, new_string, 1)
        };

        // Use atomic write
        Self::atomic_write(file_path, &new_content)?;

        // Generate structured patch
        let patch = Self::generate_diff(&original_content, &new_content, &file_path_str);

        let replacements = if replace_all { match_count } else { 1 };

        Ok(json!({
            "file_path": file_path_str,
            "replacements_made": replacements,
            "old_string_preview": truncate_preview(old_string, 100),
            "new_string_preview": truncate_preview(new_string, 100),
            "patch": patch,
        }).to_string())
    }
}

fn truncate_preview(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{}...", truncated)
    }
}
