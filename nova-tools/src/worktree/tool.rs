use std::collections::HashMap;
use std::path::PathBuf;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::registry::{ToolHandler, ToolContext};
use super::isolate::{Worktree, WorktreeManager};

pub struct WorktreeTool {
    manager: WorktreeManager,
    active: RwLock<HashMap<String, Worktree>>,
}

impl WorktreeTool {
    pub fn new(repo_root: PathBuf) -> Self {
        Self {
            manager: WorktreeManager::new(repo_root),
            active: RwLock::new(HashMap::new()),
        }
    }

    pub fn is_git_repo(&self) -> bool {
        self.manager.is_git_repo()
    }
}

#[async_trait]
impl ToolHandler for WorktreeTool {
    fn name(&self) -> &str { "worktree" }

    fn description(&self) -> &str {
        "Manage git worktrees for isolated branch work."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["create", "list", "cleanup"] },
                "session_id": { "type": "string" }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        let action = args.get("action").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' field"))?;

        match action {
            "create" => {
                let session_id = args.get("session_id").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'session_id'"))?;
                if !self.manager.is_git_repo() {
                    anyhow::bail!("Not a git repository.");
                }
                {
                    let active = self.active.read().await;
                    if active.contains_key(session_id) {
                        return Ok(format!("Worktree for session '{}' already exists", session_id));
                    }
                }
                let wt = self.manager.create(session_id)?;
                let path = wt.path.clone();
                self.active.write().await.insert(session_id.to_string(), wt);
                Ok(format!("Created worktree at: {}", path.display()))
            }
            "list" => {
                let active = self.active.read().await;
                if active.is_empty() { return Ok("No active worktrees.".to_string()); }
                let mut lines = Vec::new();
                for (sid, wt) in active.iter() {
                    lines.push(format!("session '{}' → {} (branch: {})", sid, wt.path.display(), wt.branch));
                }
                Ok(lines.join("\n"))
            }
            "cleanup" => {
                let session_id = args.get("session_id").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'session_id'"))?;
                let mut wt = match self.active.write().await.remove(session_id) {
                    Some(w) => w,
                    None => return Ok(format!("No active worktree for session '{}'.", session_id)),
                };
                wt.cleanup()?;
                Ok(format!("Cleaned up worktree for session '{}'.", session_id))
            }
            _ => Err(anyhow::anyhow!("Unknown action: {}", action)),
        }
    }
}
