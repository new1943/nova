use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::process::Command;

use crate::tools::registry::Tool;

/// Run mode for the bash tool
#[derive(Debug, Clone, PartialEq)]
pub enum BashMode {
    /// No command whitelist — only blocks truly dangerous patterns
    Open,
    /// Whitelist-only — commands must be in ALLOWED_COMMANDS
    Sandbox,
}

/// Bash tool with configurable restriction level
pub struct BashTool {
    nova_dir: String,
    mode: BashMode,
}

impl BashTool {
    pub fn new(mode: BashMode) -> Self {
        let nova_dir = dirs::home_dir()
            .map(|h| h.join(".nova").to_string_lossy().to_string())
            .unwrap_or_else(|| "/.nova".into());
        Self { nova_dir, mode }
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new(BashMode::Open)
    }
}

// Always blocked — catastrophic / destructive patterns
const BLOCKED_PATTERNS: &[&str] = &[
    "rm -rf /", "rm -rf /*", "rm -rf ~",
    "mkfs", "dd if=",
    ":(){ :|:& };:",
    "shutdown", "reboot", "halt", "poweroff",
    "mv / ", "cp /dev/null",
];

// Sandbox-only: allowed command whitelist
const SANDBOX_ALLOWED: &[&str] = &[
    "ls", "cat", "find", "head", "tail", "wc", "diff", "stat",
    "echo", "pwd", "which", "env", "date", "whoami", "uname",
    "grep", "rg", "ag", "sed", "awk", "sort", "uniq", "tr",
    "file", "du", "df", "tree", "less", "more",
    "git", "cargo", "rustc", "python", "python3", "node",
    "mkdir", "touch", "cp", "mv", "rm",
];

// Blocked shell operators (both modes)
const BLOCKED_OPERATORS: &[&str] = &[
    "$(", "`",
    "<(", ">(",
];

impl BashTool {
    fn validate(&self, command: &str) -> Result<()> {
        let cmd_lower = command.to_lowercase();
        let trimmed = cmd_lower.trim();

        if trimmed.is_empty() {
            anyhow::bail!("Empty command");
        }

        // Always block catastrophic patterns
        for pat in BLOCKED_PATTERNS {
            if trimmed.contains(pat) {
                anyhow::bail!("Blocked dangerous command pattern: {}", pat);
            }
        }

        // Always block command/process substitution
        for op in BLOCKED_OPERATORS {
            if command.contains(op) {
                anyhow::bail!("Blocked operator: '{}' — not allowed", op);
            }
        }

        // Always block writes to ~/.nova/
        if command.contains(&self.nova_dir) || command.contains("~/.nova") || command.contains("$HOME/.nova") {
            anyhow::bail!("Cannot operate on ~/.nova/ directory");
        }

        // Always block privilege escalation
        let first_word = trimmed.split_whitespace().next().unwrap_or("");
        if first_word == "sudo" || first_word == "su" || first_word == "doas" {
            anyhow::bail!("Blocked: privilege escalation not allowed");
        }

        // Sandbox mode: enforce command whitelist
        if self.mode == BashMode::Sandbox {
            let base_cmd = first_word.rsplit('/').next().unwrap_or(first_word);
            if !SANDBOX_ALLOWED.contains(&base_cmd) {
                anyhow::bail!(
                    "Sandbox mode: '{}' not in allowed list. Use mode = \"open\" in config to remove restrictions.",
                    base_cmd
                );
            }
        }

        Ok(())
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str { "bash" }

    fn description(&self) -> &str {
        match self.mode {
            BashMode::Open => "Execute a bash command. Open mode: most commands allowed, only catastrophic patterns blocked.",
            BashMode::Sandbox => "Execute a bash command. Sandbox mode: only whitelisted commands allowed.",
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to execute"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let command = args.get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' field"))?;

        self.validate(command)?;

        let output = Command::new("bash")
            .arg("-c")
            .arg(command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if output.status.success() {
            Ok(stdout.to_string())
        } else {
            Ok(format!("Exit code: {}\nstdout: {}\nstderr: {}",
                output.status.code().unwrap_or(-1), stdout, stderr))
        }
    }
}
