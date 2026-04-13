use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::process::Command;

use crate::tools::registry::Tool;

/// Grep tool — wraps ripgrep (rg) for fast code search.
/// Falls back to grep -rn if rg is not installed.
pub struct GrepTool;

const MAX_RESULTS: usize = 200;

impl GrepTool {
    /// Check if ripgrep is available
    async fn has_rg() -> bool {
        Command::new("rg")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    async fn run_rg(pattern: &str, path: &str, glob: Option<&str>, context: u32, case_insensitive: bool) -> Result<String> {
        let mut cmd = Command::new("rg");
        cmd.arg("--no-heading")
           .arg("--line-number")
           .arg("--color=never")
           .arg("--max-count=50");  // max matches per file

        if case_insensitive {
            cmd.arg("-i");
        }
        if context > 0 {
            cmd.arg("-C").arg(context.to_string());
        }
        if let Some(g) = glob {
            cmd.arg("--glob").arg(g);
        }

        cmd.arg("--").arg(pattern).arg(path);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let output = cmd.output().await?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        if stdout.is_empty() {
            return Ok("No matches found.".into());
        }

        // Truncate to MAX_RESULTS lines
        let lines: Vec<&str> = stdout.lines().collect();
        if lines.len() > MAX_RESULTS {
            let truncated: String = lines[..MAX_RESULTS].join("\n");
            Ok(format!("{}\n\n... ({} more lines, showing first {})", truncated, lines.len() - MAX_RESULTS, MAX_RESULTS))
        } else {
            Ok(stdout.to_string())
        }
    }

    async fn run_grep_fallback(pattern: &str, path: &str, case_insensitive: bool) -> Result<String> {
        let mut cmd = Command::new("grep");
        cmd.arg("-rn").arg("--color=never");
        if case_insensitive {
            cmd.arg("-i");
        }
        cmd.arg("--").arg(pattern).arg(path);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let output = cmd.output().await?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        if stdout.is_empty() {
            return Ok("No matches found.".into());
        }

        let lines: Vec<&str> = stdout.lines().collect();
        if lines.len() > MAX_RESULTS {
            let truncated: String = lines[..MAX_RESULTS].join("\n");
            Ok(format!("{}\n\n... ({} more lines)", truncated, lines.len() - MAX_RESULTS))
        } else {
            Ok(stdout.to_string())
        }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str { "grep" }

    fn description(&self) -> &str {
        "Search file contents using regex patterns. Uses ripgrep (rg) for fast search. Supports glob filtering, context lines, and case-insensitive search."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search in. Defaults to current directory."
                },
                "glob": {
                    "type": "string",
                    "description": "Glob pattern to filter files (e.g. '*.rs', '*.{ts,tsx}')"
                },
                "context": {
                    "type": "integer",
                    "description": "Number of context lines before and after each match (default: 2)"
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Case insensitive search (default: false)"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let pattern = args.get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'pattern' field"))?;

        let path = args.get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        let glob = args.get("glob").and_then(|v| v.as_str());
        let context = args.get("context").and_then(|v| v.as_u64()).unwrap_or(2) as u32;
        let case_insensitive = args.get("case_insensitive").and_then(|v| v.as_bool()).unwrap_or(false);

        if Self::has_rg().await {
            Self::run_rg(pattern, path, glob, context, case_insensitive).await
        } else {
            Self::run_grep_fallback(pattern, path, case_insensitive).await
        }
    }
}
