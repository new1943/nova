use std::collections::HashMap;
use std::path::PathBuf;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::tools::Tool;
use crate::worktree::isolate::{Worktree, WorktreeManager};

pub struct WorktreeTool {
    manager: WorktreeManager,
    /// Currently active worktrees (session_id → Worktree)
    /// Worktrees are dropped (cleanup) when removed from this map.
    /// Uses RwLock for interior mutability (Sync + async-safe).
    active: RwLock<HashMap<String, Worktree>>,
}

impl WorktreeTool {
    pub fn new(repo_root: PathBuf) -> Self {
        Self {
            manager: WorktreeManager::new(repo_root),
            active: RwLock::new(HashMap::new()),
        }
    }

    /// Returns true if the repo root is a git repository
    pub fn is_git_repo(&self) -> bool {
        self.manager.is_git_repo()
    }
}

#[async_trait]
impl Tool for WorktreeTool {
    fn name(&self) -> &str { "worktree" }

    fn description(&self) -> &str {
        "Manage git worktrees for isolated branch work. Create a new worktree for a session, \
         list active worktrees, or clean up a specific worktree. \
         Use when you need to work on multiple branches in parallel without git checkout conflicts."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "list", "cleanup"],
                    "description": "Action to perform"
                },
                "session_id": {
                    "type": "string",
                    "description": "Session ID to create/cleanup a worktree for (required for create/cleanup)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let action = args.get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' field"))?;

        match action {
            "create" => {
                let session_id = args.get("session_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'session_id' for create action"))?;

                if !self.manager.is_git_repo() {
                    anyhow::bail!("Not a git repository. Set workspace to a git repo root.");
                }

                {
                    let active = self.active.read().await;
                    if active.contains_key(session_id) {
                        return Ok(format!("Worktree for session '{}' already exists at: {}",
                            session_id, active.get(session_id).unwrap().path.display()));
                    }
                }

                let wt = self.manager.create(session_id)?;
                let path = wt.path.clone();
                self.active.write().await.insert(session_id.to_string(), wt);
                Ok(format!("Created worktree at: {}\nUse this path for git operations in the new branch.", path.display()))
            }

            "list" => {
                let active = self.active.read().await;
                if active.is_empty() {
                    return Ok("No active worktrees.".to_string());
                }
                let mut lines = Vec::new();
                for (sid, wt) in active.iter() {
                    lines.push(format!("session '{}' → {} (branch: {})", sid, wt.path.display(), wt.branch));
                }
                Ok(lines.join("\n"))
            }

            "cleanup" => {
                let session_id = args.get("session_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'session_id' for cleanup action"))?;

                let mut wt = match self.active.write().await.remove(session_id) {
                    Some(w) => w,
                    None => return Ok(format!("No active worktree found for session '{}'.", session_id)),
                };
                wt.cleanup()?;
                Ok(format!("Cleaned up worktree for session '{}'.", session_id))
            }

            _ => Err(anyhow::anyhow!("Unknown action: {}. Use: create, list, cleanup.", action)),
        }
    }
}
