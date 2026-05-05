//! TaskLogger — lightweight task status persistence module

use std::path::{Path, PathBuf};
use anyhow::Result;
use regex::Regex;
use tokio::fs::{OpenOptions, File};
use tokio::io::{AsyncWriteExt, AsyncReadExt};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Pending => "Pending",
            TaskStatus::Running => "Running",
            TaskStatus::Completed => "Completed",
            TaskStatus::Failed => "Failed",
            TaskStatus::Cancelled => "Cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "Pending" => Some(TaskStatus::Pending),
            "Running" => Some(TaskStatus::Running),
            "Completed" => Some(TaskStatus::Completed),
            "Failed" => Some(TaskStatus::Failed),
            "Cancelled" => Some(TaskStatus::Cancelled),
            _ => None,
        }
    }
}

pub struct TaskLogger;

impl TaskLogger {
    fn tasks_path(workspace_dir: &Path) -> PathBuf {
        workspace_dir.join("tasks.md")
    }

    async fn ensure_header(workspace_dir: &Path) -> Result<()> {
        let path = Self::tasks_path(workspace_dir);
        if !path.exists() {
            let header = "# Nova Tasks\n> 本文件由 Nova 系统自动维护，您也可以直接修改它。\n\n";
            let mut file = File::create(&path).await?;
            file.write_all(header.as_bytes()).await?;
        }
        Ok(())
    }

    pub async fn append_task(workspace_dir: &Path, id: &str, desc: &str) -> Result<()> {
        static MUTEX: Mutex<()> = Mutex::const_new(());
        let _guard = MUTEX.lock().await;
        Self::ensure_header(workspace_dir).await?;
        let path = Self::tasks_path(workspace_dir);
        let line = format!("* `{}`: [Running] {}\n", id, desc);
        let mut file = OpenOptions::new().append(true).open(&path).await?;
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;
        Ok(())
    }

    pub async fn update_status(workspace_dir: &Path, id: &str, new_status: &str, _error_msg: Option<&str>) -> Result<()> {
        static MUTEX: Mutex<()> = Mutex::const_new(());
        let _guard = MUTEX.lock().await;
        let path = Self::tasks_path(workspace_dir);
        if !path.exists() { return Ok(()); }
        let mut content = String::new();
        let mut file = OpenOptions::new().read(true).open(&path).await?;
        file.read_to_string(&mut content).await?;
        drop(file);
        let escaped_id = regex::escape(id);
        let re_pattern = format!(r"(\* `{}`: \[)\w+(\])", escaped_id);
        let re = Regex::new(&re_pattern)?;
        let new_content = if let Some(captures) = re.captures(&content) {
            let prefix = captures.get(1).map(|m| m.as_str()).unwrap_or("");
            let replacement = format!("{}{}{}", prefix, new_status, "]");
            content.replacen(&captures[0], &replacement, 1)
        } else {
            content
        };
        let mut file = OpenOptions::new().write(true).truncate(true).open(&path).await?;
        file.write_all(new_content.as_bytes()).await?;
        file.flush().await?;
        Ok(())
    }

    pub async fn update_status_v2(workspace_dir: &Path, id: &str, new_status: &str, _error_msg: Option<&str>) -> Result<()> {
        Self::update_status(workspace_dir, id, new_status, _error_msg).await
    }

    pub async fn read_context(workspace_dir: &Path) -> Result<String> {
        let path = Self::tasks_path(workspace_dir);
        if !path.exists() { return Ok(String::new()); }
        let mut content = String::new();
        let mut file = OpenOptions::new().read(true).open(&path).await?;
        file.read_to_string(&mut content).await?;
        drop(file);
        let mut active_tasks = Vec::new();
        let re = Regex::new(r"^\* `([^`]+)`: \[(\w+)\] (.+)$")?;
        for line in content.lines() {
            if let Some(captures) = re.captures(line) {
                let status = captures.get(2).map(|m| m.as_str()).unwrap_or("");
                if status == "Pending" || status == "Running" {
                    let id = captures.get(1).map(|m| m.as_str()).unwrap_or("");
                    let desc = captures.get(3).map(|m| m.as_str()).unwrap_or("");
                    active_tasks.push(format!("* `{}`: [{}] {}", id, status, desc));
                }
            }
        }
        if active_tasks.is_empty() { return Ok(String::new()); }
        Ok(format!("\n\n<current_tasks>\n{}\n</current_tasks>", active_tasks.join("\n")))
    }

    pub async fn sweep_orphans(workspace_dir: &Path) -> Result<()> {
        static MUTEX: Mutex<()> = Mutex::const_new(());
        let _guard = MUTEX.lock().await;
        let path = Self::tasks_path(workspace_dir);
        if !path.exists() { return Ok(()); }
        let mut content = String::new();
        let mut file = OpenOptions::new().read(true).open(&path).await?;
        file.read_to_string(&mut content).await?;
        drop(file);
        let re = Regex::new(r"(\[)Running(\])")?;
        let new_content = re.replace_all(&content, "${1}Failed${2}");
        if new_content != content {
            let mut file = OpenOptions::new().write(true).truncate(true).open(&path).await?;
            file.write_all(new_content.as_bytes()).await?;
            file.flush().await?;
        }
        Ok(())
    }
}
