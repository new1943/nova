use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::tools::registry::Tool;

/// Glob file search tool
pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str { "glob" }

    fn description(&self) -> &str {
        "Search for files matching a glob pattern. Returns matching file paths."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern (e.g. '**/*.rs', 'src/*.txt')" }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let pattern = args.get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'pattern' field"))?;

        let mut paths = Vec::new();
        for entry in glob::glob(pattern)? {
            match entry {
                Ok(path) => paths.push(path.to_string_lossy().to_string()),
                Err(e) => tracing::warn!("glob error: {}", e),
            }
        }

        Ok(paths.join("\n"))
    }
}
