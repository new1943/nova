use std::path::PathBuf;
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::team::config::{TeamManager, Task, TaskStatus};
use crate::team::mailbox::{Mailbox, MailMessage};
use crate::tools::Tool;

/// Tool for managing teams, tasks, and inter-agent messaging.
pub struct TeamTool {
    manager: TeamManager,
    mailbox: RwLock<Mailbox>,
}

impl TeamTool {
    pub fn new(teams_dir: PathBuf) -> Self {
        Self {
            manager: TeamManager::new(teams_dir),
            mailbox: RwLock::new(Mailbox::new()),
        }
    }
}

#[async_trait]
impl Tool for TeamTool {
    fn name(&self) -> &str { "team" }

    fn description(&self) -> &str {
        "Manage teams of agents. Create teams, add and assign tasks, \
         and send messages between agent members. \
         Use for coordinating multiple specialized agents on complex workflows."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create_team", "list_teams", "add_task", "assign_task", "list_tasks", "send_message"],
                    "description": "Action to perform"
                },
                "team": {
                    "type": "string",
                    "description": "Team name (required for most actions)"
                },
                "description": {
                    "type": "string",
                    "description": "Task description (for add_task)"
                },
                "task_id": {
                    "type": "string",
                    "description": "Task ID (for assign_task)"
                },
                "member": {
                    "type": "string",
                    "description": "Agent member name (for assign_task, send_message)"
                },
                "content": {
                    "type": "string",
                    "description": "Message content (for send_message)"
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
            "create_team" => {
                let team = args.get("team")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'team' field"))?;
                let t = self.manager.create_team(team)?;
                Ok(format!("Created team '{}' with {} initial members", t.name, t.members.len()))
            }

            "list_teams" => {
                let names = self.manager.list_teams()?;
                if names.is_empty() {
                    Ok("No teams exist.".to_string())
                } else {
                    Ok(format!("Teams:\n{}", names.join("\n")))
                }
            }

            "add_task" => {
                let team = args.get("team")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'team' field"))?;
                let description = args.get("description")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'description' field"))?;

                let mut t = self.manager.load(team)?
                    .ok_or_else(|| anyhow::anyhow!("Team '{}' not found", team))?;
                let task = Task::new(description);
                let task_id = task.id.clone();
                t.add_task(task);
                self.manager.save(&t)?;
                Ok(format!("Added task '{}' to team '{}'", task_id, team))
            }

            "assign_task" => {
                let team = args.get("team")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'team' field"))?;
                let task_id = args.get("task_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'task_id' field"))?;
                let member = args.get("member")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'member' field"))?;

                let mut t = self.manager.load(team)?
                    .ok_or_else(|| anyhow::anyhow!("Team '{}' not found", team))?;

                let task = t.find_task_mut(task_id)
                    .ok_or_else(|| anyhow::anyhow!("Task '{}' not found", task_id))?;
                task.owner = Some(member.to_string());
                task.status = TaskStatus::InProgress;
                task.updated_at = Utc::now();

                self.manager.save(&t)?;
                Ok(format!("Assigned task '{}' to '{}'", task_id, member))
            }

            "list_tasks" => {
                let team = args.get("team")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'team' field"))?;

                let t = self.manager.load(team)?
                    .ok_or_else(|| anyhow::anyhow!("Team '{}' not found", team))?;

                if t.tasks.is_empty() {
                    return Ok("No tasks in this team.".to_string());
                }

                let lines: Vec<String> = t.tasks.iter().map(|task| {
                    let owner = task.owner.as_deref().unwrap_or("(unassigned)");
                    let status_str = format!("{:?}", task.status);
                    format!("[{}] {} — {} (owner: {})",
                        task.id, status_str, task.description, owner)
                }).collect();
                Ok(lines.join("\n"))
            }

            "send_message" => {
                let member = args.get("member")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'member' field"))?;
                let content = args.get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'content' field"))?;

                let msg = MailMessage {
                    from: "coordinator".to_string(),
                    to: member.to_string(),
                    content: content.to_string(),
                    timestamp: Utc::now(),
                };

                self.mailbox.write().await.send(msg);
                Ok(format!("Message sent to '{}': {}", member, content))
            }

            _ => Err(anyhow::anyhow!(
                "Unknown action: {}. Use: create_team, list_teams, add_task, assign_task, list_tasks, send_message.",
                action
            )),
        }
    }
}
