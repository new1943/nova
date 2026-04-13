use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;

use crate::tools::registry::Tool;

/// Write file tool (overwrite or append)
pub struct WriteFileTool;

impl WriteFileTool {
    fn check_path(&self, path: &str) -> Result<()> {
        let nova_dir = dirs::home_dir()
            .map(|h| h.join(".nova").to_string_lossy().to_string())
            .unwrap_or_default();
        if !nova_dir.is_empty() && path.contains(&nova_dir) {
            anyhow::bail!("Cannot write to ~/.nova/ directory");
        }
        Ok(())
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str { "write_file" }

    fn description(&self) -> &str {
        "Write content to a file. Mode: 'overwrite' (default) or 'append'. Auto-creates parent directories."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path to write" },
                "content": { "type": "string", "description": "Content to write" },
                "mode": { "type": "string", "enum": ["overwrite", "append"], "description": "Write mode (default: overwrite)" }
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

        self.check_path(path)?;

        // Auto-create parent directories
        if let Some(parent) = std::path::Path::new(path).parent() {
            fs::create_dir_all(parent)?;
        }

        match mode {
            "append" => {
                let mut file = OpenOptions::new().create(true).append(true).open(path)?;
                write!(file, "{}", content)?;
            }
            _ => {
                fs::write(path, content)?;
            }
        }

        Ok(format!("Written {} bytes to {}", content.len(), path))
    }
}
