use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::process::Command;

use crate::tools::registry::Tool;
use crate::tools::truncate::truncate_bash;

pub mod security;

use security::validate_bash_command;

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
    mode: BashMode,
}

impl BashTool {
    pub fn new(mode: BashMode) -> Self {
        Self { mode }
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new(BashMode::Open)
    }
}

impl BashTool {
    fn validate(&self, command: &str) -> Result<()> {
        let lowered = command.to_lowercase();
        let trimmed = lowered.trim();

        if trimmed.is_empty() {
            anyhow::bail!("Empty command");
        }

        // Run security validations
        if let Err(e) = validate_bash_command(command) {
            anyhow::bail!("{}", e.message);
        }

        // Always block catastrophic patterns (override)
        let blocked: &[&str] = &[
            "rm -rf /",
            "rm -rf /*",
            "rm -rf ~",
            "mkfs",
            "dd if=",
            ":(){ :|:& };:",
            "shutdown",
            "reboot",
            "halt",
            "poweroff",
            "mv / ",
            "cp /dev/null",
        ];

        for pat in blocked {
            if trimmed.contains(pat) {
                anyhow::bail!("Blocked dangerous command pattern: {}", pat);
            }
        }

        // Always block command substitution (from security module)
        let blocked_ops: &[&str] = &["$(", "`", "<(", ">("];
        for op in blocked_ops {
            if command.contains(op) {
                // Check if it's properly quoted
                let quoted_check = security::quote::QuoteState::extract_unquoted(command);
                if quoted_check.1.contains(op) {
                    // Unquoted - block
                    anyhow::bail!("Blocked operator: '{}' — not allowed", op);
                }
            }
        }

        // Always block privilege escalation
        let first_word = trimmed.split_whitespace().next().unwrap_or("");
        if first_word == "sudo"
            || first_word == "su"
            || first_word == "doas"
        {
            anyhow::bail!("Blocked: privilege escalation not allowed");
        }

        // Sandbox mode: enforce command whitelist
        if self.mode == BashMode::Sandbox {
            let base_cmd = first_word.rsplit('/').next().unwrap_or(first_word);
            if !security::constants::is_sandbox_allowed(base_cmd) {
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
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        match self.mode {
            BashMode::Open => {
                "Execute a bash command. Open mode: most commands allowed, only catastrophic patterns blocked."
            }
            BashMode::Sandbox => {
                "Execute a bash command. Sandbox mode: only whitelisted commands allowed."
            }
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
        let command = args
            .get("command")
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

        let final_output = if output.status.success() {
            stdout.to_string()
        } else {
            format!(
                "Exit code: {}\nstdout: {}\nstderr: {}",
                output.status.code().unwrap_or(-1),
                stdout,
                stderr
            )
        };

        // I/O Shield: truncate超长输出
        let truncated = truncate_bash(&final_output);
        if truncated.len() < final_output.len() {
            tracing::warn!(
                "bash output truncated: {} -> {} chars",
                final_output.len(),
                truncated.len()
            );
        }

        Ok(truncated)
    }
}
