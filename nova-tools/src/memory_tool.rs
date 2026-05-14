use anyhow::Result;
use async_trait::async_trait;
use chrono::Local;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

use crate::registry::{ToolContext, ToolHandler};

/// Memory tool — manages conversation memory and persistent knowledge.
///
/// Three actions:
/// - review: Summarize current conversation into daily memory file
/// - save: Save persistent memory (user preferences, tips, decisions) to MEMORY.md
/// - load: Read memory file and return contents
pub struct MemoryTool;

impl MemoryTool {
    pub fn new() -> Self {
        Self
    }

    fn memories_dir(ctx: &ToolContext) -> PathBuf {
        ctx.workspace_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("~/.nova"))
            .join("memories")
    }

    fn memory_file(ctx: &ToolContext) -> PathBuf {
        ctx.workspace_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("~/.nova"))
            .join("MEMORY.md")
    }

    fn today_file(ctx: &ToolContext) -> PathBuf {
        let today = Local::now().format("%Y-%m-%d").to_string();
        Self::memories_dir(ctx).join(format!("{}.md", today))
    }

    fn handle_review(&self, args: &Value, ctx: &ToolContext) -> Result<String> {
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'content' field for review"))?;

        let dir = Self::memories_dir(ctx);
        fs::create_dir_all(&dir)?;

        let file = Self::today_file(ctx);
        let today = Local::now().format("%Y-%m-%d").to_string();
        let time = Local::now().format("%H:%M").to_string();

        // Append to daily memory file
        let entry = format!("\n## {} — {}\n\n{}\n", today, time, content);

        if file.exists() {
            let mut existing = fs::read_to_string(&file)?;
            existing.push_str(&entry);
            fs::write(&file, existing)?;
        } else {
            let header = format!("# Daily Memory — {}\n", today);
            fs::write(&file, format!("{}{}", header, entry))?;
        }

        Ok(format!(
            "Memory reviewed and saved to {}",
            file.display()
        ))
    }

    fn handle_save(&self, args: &Value, ctx: &ToolContext) -> Result<String> {
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'content' field for save"))?;

        let file = Self::memory_file(ctx);
        let date = Local::now().format("%Y-%m-%d").to_string();

        let entry = format!("\n## {}\n\n{}\n", date, content);

        if file.exists() {
            let mut existing = fs::read_to_string(&file)?;
            existing.push_str(&entry);
            fs::write(&file, existing)?;
        } else {
            let header = "# MEMORY.md\n\nPersistent memory: user preferences, tips, decisions.\n";
            fs::write(&file, format!("{}{}", header, entry))?;
        }

        Ok(format!("Persistent memory saved to {}", file.display()))
    }

    fn handle_load(&self, args: &Value, ctx: &ToolContext) -> Result<String> {
        let date = args
            .get("date")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| Local::now().format("%Y-%m-%d").to_string());

        let file = if date == "MEMORY" {
            Self::memory_file(ctx)
        } else {
            Self::memories_dir(ctx).join(format!("{}.md", date))
        };

        if !file.exists() {
            return Ok(format!("No memory file found for {}", date));
        }

        let content = fs::read_to_string(&file)?;
        Ok(content)
    }
}

#[async_trait]
impl ToolHandler for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }

    fn description(&self) -> &str {
        r#"Memory tool for managing conversation memory and persistent knowledge.

Actions:
- review: Summarize current conversation into daily memory file (memories/YYYY-MM-DD.md). Use when topic shifts.
- save: Save persistent memory (user preferences, tips, decisions) to MEMORY.md. Use when user says "remember this".
- load: Read memory file. Default: today. Specify date or "MEMORY" for persistent memory.

When to use review:
- Topic clearly shifts (completely different subject)
- Conversation naturally ends, want to record key points

When to use save:
- User says "remember this", "note this for later"
- Discovered important user preference, project decision, or pitfall

When to use load:
- New session starts, need today's context
- Need to recall what was discussed before"#
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["review", "save", "load"],
                    "description": "Memory action to perform"
                },
                "content": {
                    "type": "string",
                    "description": "Content for review/save actions"
                },
                "date": {
                    "type": "string",
                    "description": "Date for load action (YYYY-MM-DD or 'MEMORY'). Default: today"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' field"))?;

        match action {
            "review" => self.handle_review(&args, ctx),
            "save" => self.handle_save(&args, ctx),
            "load" => self.handle_load(&args, ctx),
            _ => Err(anyhow::anyhow!(
                "Unknown memory action: '{}'. Use review, save, or load.",
                action
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_ctx() -> (ToolContext, TempDir) {
        let tmp = TempDir::new().unwrap();
        let ctx = ToolContext::new("test".to_string(), Some(tmp.path().to_path_buf()));
        (ctx, tmp)
    }

    #[test]
    fn review_creates_daily_file() {
        let tool = MemoryTool::new();
        let (ctx, _tmp) = test_ctx();

        let args = json!({
            "action": "review",
            "content": "讨论了 Rust 的生命周期机制"
        });

        let result = tool.handle_review(&args, &ctx).unwrap();
        assert!(result.contains("saved to"));

        // Check file exists
        let file = MemoryTool::today_file(&ctx);
        assert!(file.exists());
        let content = fs::read_to_string(&file).unwrap();
        assert!(content.contains("Rust 的生命周期机制"));
    }

    #[test]
    fn save_appends_to_memory() {
        let tool = MemoryTool::new();
        let (ctx, _tmp) = test_ctx();

        let args = json!({
            "action": "save",
            "content": "用户偏好：使用 Rust 而不是 Python"
        });

        tool.handle_save(&args, &ctx).unwrap();

        let file = MemoryTool::memory_file(&ctx);
        assert!(file.exists());
        let content = fs::read_to_string(&file).unwrap();
        assert!(content.contains("用户偏好"));
    }

    #[test]
    fn load_returns_content() {
        let tool = MemoryTool::new();
        let (ctx, _tmp) = test_ctx();

        // First create a file via review
        let review_args = json!({
            "action": "review",
            "content": "测试内容"
        });
        tool.handle_review(&review_args, &ctx).unwrap();

        // Then load it
        let load_args = json!({
            "action": "load"
        });
        let result = tool.handle_load(&load_args, &ctx).unwrap();
        assert!(result.contains("测试内容"));
    }

    #[test]
    fn load_missing_file_returns_message() {
        let tool = MemoryTool::new();
        let (ctx, _tmp) = test_ctx();

        let args = json!({
            "action": "load",
            "date": "2099-01-01"
        });
        let result = tool.handle_load(&args, &ctx).unwrap();
        assert!(result.contains("No memory file found"));
    }

    #[tokio::test]
    async fn unknown_action_errors() {
        let tool = MemoryTool::new();
        let (ctx, _tmp) = test_ctx();

        let args = json!({"action": "invalid"});
        let result = tool.execute(args, &ctx).await;
        assert!(result.is_err());
    }
}
