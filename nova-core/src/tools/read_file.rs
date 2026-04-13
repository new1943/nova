use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::fs;

use crate::tools::registry::Tool;

/// Read file tool with optional line range
pub struct ReadFileTool;

const MAX_FILE_SIZE: u64 = 1_048_576; // 1MB

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str { "read_file" }

    fn description(&self) -> &str {
        "Read a file's contents. Supports optional start_line/end_line for partial reads."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path to read" },
                "start_line": { "type": "integer", "description": "Start line (1-based, optional)" },
                "end_line": { "type": "integer", "description": "End line (1-based, inclusive, optional)" }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let path = args.get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'path' field"))?;

        let metadata = fs::metadata(path)
            .map_err(|e| anyhow::anyhow!("Cannot read '{}': {}", path, e))?;

        if metadata.len() > MAX_FILE_SIZE {
            anyhow::bail!("File too large ({} bytes, max {}). Use line range.", metadata.len(), MAX_FILE_SIZE);
        }

        let content = fs::read_to_string(path)?;

        let start = args.get("start_line").and_then(|v| v.as_u64()).map(|v| v as usize);
        let end = args.get("end_line").and_then(|v| v.as_u64()).map(|v| v as usize);

        match (start, end) {
            (Some(s), Some(e)) => {
                let lines: Vec<&str> = content.lines().collect();
                let s = s.saturating_sub(1).min(lines.len());
                let e = e.min(lines.len());
                Ok(lines[s..e].join("\n"))
            }
            (Some(s), None) => {
                let lines: Vec<&str> = content.lines().collect();
                let s = s.saturating_sub(1).min(lines.len());
                Ok(lines[s..].join("\n"))
            }
            _ => Ok(content),
        }
    }
}
