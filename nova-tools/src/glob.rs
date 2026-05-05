use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

use crate::registry::{ToolHandler, ToolContext};

/// Glob file search tool with enhanced features
pub struct GlobTool;

const MAX_RESULTS: usize = 100;

#[async_trait]
impl ToolHandler for GlobTool {
    fn name(&self) -> &str { "glob" }

    fn description(&self) -> &str {
        "Search for files matching a glob pattern. Returns matching file paths. \
         Results are limited to 100 files, sorted by mtime (most recent first)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern (e.g. '**/*.rs', 'src/*.txt')" },
                "path": { "type": "string", "description": "Base directory to search in (optional)" },
                "limit": { "type": "integer", "description": "Maximum number of results (default: 100)" }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        let pattern = args.get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'pattern' field"))?;

        let base_path = args.get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        let limit = args.get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(MAX_RESULTS as u64) as usize;

        let base = Path::new(base_path);
        if !base.exists() {
            anyhow::bail!("Path does not exist: {}", base_path);
        }
        if !base.is_dir() {
            anyhow::bail!("Path is not a directory: {}", base_path);
        }

        let full_pattern = if base_path == "." {
            pattern.to_string()
        } else {
            format!("{}/{}", base_path.trim_end_matches('/'), pattern)
        };

        let mut entries: Vec<(std::path::PathBuf, std::time::SystemTime)> = Vec::new();

        for entry in glob::glob(&full_pattern)? {
            match entry {
                Ok(path) => {
                    if path.is_file() {
                        if let Ok(metadata) = std::fs::metadata(&path) {
                            let mtime = metadata.modified()
                                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                            entries.push((path, mtime));
                        }
                    }
                }
                Err(e) => tracing::warn!("glob error: {}", e),
            }
        }

        entries.sort_by(|a, b| b.1.cmp(&a.1));

        let paths: Vec<String> = entries.into_iter()
            .take(limit)
            .map(|(path, _)| path.to_string_lossy().to_string())
            .collect();

        if paths.is_empty() {
            Ok("No matches found.".into())
        } else {
            Ok(paths.join("\n"))
        }
    }
}
