use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::fs;

use crate::tools::registry::Tool;

/// FileEdit tool — precise string replacement in files.
/// Replaces exact `old_string` with `new_string`.
/// Fails if old_string is not found or matches multiple times (unless replace_all=true).
pub struct FileEditTool;

impl FileEditTool {
    fn check_path(&self, path: &str) -> Result<()> {
        let nova_dir = dirs::home_dir()
            .map(|h| h.join(".nova").to_string_lossy().to_string())
            .unwrap_or_default();
        if !nova_dir.is_empty() && path.contains(&nova_dir) {
            anyhow::bail!("Cannot edit files in ~/.nova/ directory");
        }
        Ok(())
    }
}

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str { "file_edit" }

    fn description(&self) -> &str {
        "Perform exact string replacement in a file. Replaces old_string with new_string. \
         old_string must match exactly (including whitespace/indentation). \
         Fails if old_string not found or matches multiple locations (use replace_all=true for all occurrences). \
         Prefer this over write_file for modifying existing files."
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
        let file_path = args.get("file_path")
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

        self.check_path(file_path)?;

        if old_string == new_string {
            anyhow::bail!("old_string and new_string are identical — nothing to change");
        }

        if old_string.is_empty() {
            anyhow::bail!("old_string cannot be empty");
        }

        // Read file
        let content = fs::read_to_string(file_path)
            .map_err(|e| anyhow::anyhow!("Cannot read '{}': {}", file_path, e))?;

        // Count occurrences
        let match_count = content.matches(old_string).count();

        if match_count == 0 {
            // Provide helpful context: show a snippet of the file
            let preview: String = content.chars().take(200).collect();
            anyhow::bail!(
                "old_string not found in '{}'. File starts with:\n{}{}",
                file_path,
                preview,
                if content.len() > 200 { "..." } else { "" }
            );
        }

        if match_count > 1 && !replace_all {
            anyhow::bail!(
                "old_string found {} times in '{}'. Use replace_all=true to replace all, \
                 or provide more context in old_string to make it unique.",
                match_count, file_path
            );
        }

        // Perform replacement
        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            // Replace only first occurrence
            content.replacen(old_string, new_string, 1)
        };

        // Write back
        fs::write(file_path, &new_content)?;

        Ok(json!({
            "file_path": file_path,
            "replacements_made": if replace_all { match_count } else { 1 },
            "old_string_preview": truncate_preview(old_string, 100),
            "new_string_preview": truncate_preview(new_string, 100),
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
