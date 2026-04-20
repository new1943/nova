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

/// VCS directories to exclude by default
const VCS_DIRS: &[&str] = &["--glob", "!.git/", "--glob", "!.svn/", "--glob", "!.hg/", "--glob", "!.sl/"];

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum OutputMode {
    #[default]
    Content,
    Files,
    Count,
}

/// Configuration for grep execution
#[derive(Debug, Clone)]
struct GrepConfig<'a> {
    pattern: &'a str,
    path: &'a str,
    glob: Option<&'a str>,
    context: u32,
    case_insensitive: bool,
    output_mode: OutputMode,
    head_limit: Option<usize>,
    offset: Option<usize>,
    file_type: Option<&'a str>,
    before_context: u32,
    after_context: u32,
    cwd: Option<&'a str>,
}

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

    async fn run_rg(&self, config: GrepConfig<'_>) -> Result<String> {
        let mut cmd = Command::new("rg");
        cmd.arg("--no-heading");

        match config.output_mode {
            OutputMode::Content => {
                cmd.arg("--line-number").arg("--color=never");
            }
            OutputMode::Files => {
                cmd.arg("-l");
            }
            OutputMode::Count => {
                cmd.arg("-c");
            }
        }

        // VCS exclusions
        for vcs in VCS_DIRS {
            cmd.arg(vcs);
        }

        if config.case_insensitive {
            cmd.arg("-i");
        }

        // Context lines
        if config.context > 0 {
            cmd.arg("-C").arg(config.context.to_string());
        }

        // Before/After context
        if config.before_context > 0 {
            cmd.arg("-B").arg(config.before_context.to_string());
        }
        if config.after_context > 0 {
            cmd.arg("-A").arg(config.after_context.to_string());
        }

        // Glob filter
        if let Some(g) = config.glob {
            cmd.arg("--glob").arg(g);
        }

        // File type filter
        if let Some(t) = config.file_type {
            cmd.arg("--type").arg(t);
        }

        // Head limit
        if let Some(limit) = config.head_limit {
            cmd.arg("--max-count").arg(limit.to_string());
        }

        if let Some(c) = config.cwd {
            cmd.current_dir(c);
        }

        cmd.arg("--").arg(config.pattern).arg(config.path);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let output = cmd.output().await?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        if stdout.is_empty() {
            return Ok("No matches found.".into());
        }

        // Apply offset if specified (skip first N lines)
        let final_output = if let Some(off) = config.offset {
            let lines: Vec<&str> = stdout.lines().collect();
            if off < lines.len() {
                lines[off..].join("\n")
            } else {
                String::new()
            }
        } else {
            stdout.to_string()
        };

        if final_output.is_empty() {
            return Ok("No matches found.".into());
        }

        // For content mode, truncate to MAX_RESULTS
        if config.output_mode == OutputMode::Content {
            let lines: Vec<&str> = final_output.lines().collect();
            if lines.len() > MAX_RESULTS {
                let truncated: String = lines[..MAX_RESULTS].join("\n");
                return Ok(format!("{}\n\n... ({} more lines, showing first {})", truncated, lines.len() - MAX_RESULTS, MAX_RESULTS));
            }
        }

        Ok(final_output)
    }

    async fn run_grep_fallback(
        pattern: &str,
        path: &str,
        case_insensitive: bool,
        head_limit: Option<usize>,
    ) -> Result<String> {
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

        // Apply head limit
        let final_lines = if let Some(limit) = head_limit {
            if limit < lines.len() {
                &lines[..limit]
            } else {
                &lines
            }
        } else if lines.len() > MAX_RESULTS {
            &lines[..MAX_RESULTS]
        } else {
            &lines
        };

        if final_lines.is_empty() {
            return Ok("No matches found.".into());
        }

        let result = final_lines.join("\n");

        if lines.len() > MAX_RESULTS || (head_limit.is_some() && head_limit.unwrap() < lines.len()) {
            let shown = head_limit.unwrap_or(MAX_RESULTS).min(MAX_RESULTS);
            return Ok(format!("{}\n\n... ({} more lines)", result, lines.len() - shown));
        }

        Ok(result)
    }

    fn parse_output_mode(mode: Option<&str>) -> OutputMode {
        match mode {
            Some("files") | Some("files_with_matches") => OutputMode::Files,
            Some("count") => OutputMode::Count,
            _ => OutputMode::Content,
        }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str { "grep" }

    fn description(&self) -> &str {
        "Search file contents using regex patterns. Uses ripgrep (rg) for fast search. \
         Supports glob filtering, context lines, case-insensitive search, VCS directory exclusion, \
         head/offset pagination, multiple output modes (content/files/count), and file type filtering."
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
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files", "count"],
                    "description": "Output mode: 'content' (default), 'files' (file names only), 'count' (line counts per file)"
                },
                "head_limit": {
                    "type": "integer",
                    "description": "Maximum number of matches to return (default: 200)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Skip first N matches (pagination offset, default: 0)"
                },
                "file_type": {
                    "type": "string",
                    "description": "Filter by file type: 'js', 'ts', 'py', 'rs', 'go', 'java', etc. (rg --type)"
                },
                "before_context": {
                    "type": "integer",
                    "description": "Number of lines BEFORE each match (like grep -B)"
                },
                "after_context": {
                    "type": "integer",
                    "description": "Number of lines AFTER each match (like grep -A)"
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory for the search. Results will be relative to this directory (saves tokens)."
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

        let cwd = args.get("cwd")
            .and_then(|v| v.as_str());

        let config = GrepConfig {
            pattern,
            path,
            glob: args.get("glob").and_then(|v| v.as_str()),
            context: args.get("context").and_then(|v| v.as_u64()).unwrap_or(2) as u32,
            case_insensitive: args.get("case_insensitive").and_then(|v| v.as_bool()).unwrap_or(false),
            output_mode: Self::parse_output_mode(args.get("output_mode").and_then(|v| v.as_str())),
            head_limit: args.get("head_limit").and_then(|v| v.as_u64()).map(|v| v as usize),
            offset: args.get("offset").and_then(|v| v.as_u64()).map(|v| v as usize),
            file_type: args.get("file_type").and_then(|v| v.as_str()),
            before_context: args.get("before_context").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            after_context: args.get("after_context").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            cwd,
        };

        if Self::has_rg().await {
            self.run_rg(config).await
        } else {
            Self::run_grep_fallback(pattern, path, config.case_insensitive, config.head_limit).await
        }
    }
}
