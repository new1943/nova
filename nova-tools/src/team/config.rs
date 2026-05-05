use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub description: String,
    pub owner: Option<String>,
    pub status: TaskStatus,
    pub result: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Task {
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string()[..8].to_string(),
            description: description.into(),
            owner: None,
            status: TaskStatus::Pending,
            result: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub name: String,
    pub members: Vec<String>,
    pub tasks: Vec<Task>,
    pub created_at: DateTime<Utc>,
}

impl Team {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), members: Vec::new(), tasks: Vec::new(), created_at: Utc::now() }
    }

    pub fn add_member(&mut self, name: impl Into<String>) {
        let n = name.into();
        if !self.members.contains(&n) { self.members.push(n); }
    }

    pub fn add_task(&mut self, task: Task) { self.tasks.push(task); }

    pub fn find_task_mut(&mut self, task_id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == task_id)
    }
}

pub struct TeamManager {
    teams_dir: PathBuf,
}

impl TeamManager {
    pub fn new(teams_dir: PathBuf) -> Self { Self { teams_dir } }

    pub fn create_team(&self, name: &str) -> Result<Team> {
        let team = Team::new(name);
        self.save(&team)?;
        Ok(team)
    }

    pub fn load(&self, name: &str) -> Result<Option<Team>> {
        let path = self.teams_dir.join(name).join("config.json");
        if !path.exists() { return Ok(None); }
        let content = std::fs::read_to_string(path)?;
        Ok(Some(serde_json::from_str(&content)?))
    }

    pub fn save(&self, team: &Team) -> Result<()> {
        let dir = self.teams_dir.join(&team.name);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("config.json");
        std::fs::write(path, serde_json::to_string_pretty(team)?)?;
        Ok(())
    }

    pub fn list_teams(&self) -> Result<Vec<String>> {
        if !self.teams_dir.exists() { return Ok(Vec::new()); }
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&self.teams_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                names.push(entry.file_name().to_string_lossy().to_string());
            }
        }
        Ok(names)
    }
}
