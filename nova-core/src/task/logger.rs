//! TaskLogger — 轻量级任务状态持久化模块
//!
//! 使用 Markdown 文件 (tasks.md) 作为单一真实数据源 (SSOT)，
//! 实现任务状态的持久化，供 Agent 在上下文中感知当前任务。

use std::path::PathBuf;
use anyhow::Result;
use regex::Regex;
use tokio::fs::{OpenOptions, File};
use tokio::io::{AsyncWriteExt, AsyncReadExt};
use tokio::sync::Mutex;

/// 任务状态枚举
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

    pub fn from_str(s: &str) -> Option<Self> {
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

/// TaskLogger — 任务状态管理器
///
/// 使用 tokio::sync::Mutex 保证并发安全，所有操作串行化。
pub struct TaskLogger;

impl TaskLogger {
    /// 获取 tasks.md 文件路径
    fn tasks_path(workspace_dir: &PathBuf) -> PathBuf {
        workspace_dir.join("tasks.md")
    }

    /// 确保 tasks.md 文件存在并有基本头部
    async fn ensure_header(workspace_dir: &PathBuf) -> Result<()> {
        let path = Self::tasks_path(workspace_dir);
        if !path.exists() {
            let header = "# Nova Tasks\n> 本文件由 Nova 系统自动维护，您也可以直接修改它。\n\n";
            let mut file = File::create(&path).await?;
            file.write_all(header.as_bytes()).await?;
        }
        Ok(())
    }

    /// 追加新任务到 tasks.md
    ///
    /// 格式：`* `task-id`: [Running] description`
    pub async fn append_task(workspace_dir: &PathBuf, id: &str, desc: &str) -> Result<()> {
        static MUTEX: Mutex<()> = Mutex::const_new(());
        let _guard = MUTEX.lock().await;

        Self::ensure_header(workspace_dir).await?;

        let path = Self::tasks_path(workspace_dir);
        let line = format!("* `{}`: [Running] {}\n", id, desc);

        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;

        Ok(())
    }

    /// 更新任务状态
    pub async fn update_status(
        workspace_dir: &PathBuf,
        id: &str,
        new_status: &str,
        error_msg: Option<&str>,
    ) -> Result<()> {
        static MUTEX: Mutex<()> = Mutex::const_new(());
        let _guard = MUTEX.lock().await;

        let path = Self::tasks_path(workspace_dir);
        if !path.exists() {
            return Ok(());
        }

        let mut content = String::new();
        let mut file = OpenOptions::new()
            .read(true)
            .open(&path)
            .await?;
        file.read_to_string(&mut content).await?;
        drop(file);

        // 匹配任务行的正则：* `task-id`: [status] description
        let escaped_id = regex::escape(id);
        let re_pattern = format!(r"(\* `{}`: \[)\w+(\])", escaped_id);
        let re = Regex::new(&re_pattern)?;

        let mut new_content = if let Some(captures) = re.captures(&content) {
            let prefix = captures.get(1).map(|m| m.as_str()).unwrap_or("");
            let replacement = format!("{}{}{}", prefix, new_status, "]");
            content.replacen(&captures[0], &replacement, 1)
        } else {
            content.clone()
        };

        // 如果有 error_msg，追加到描述后
        if let Some(err) = error_msg {
            let re2 = Regex::new(&format!(r"(\* `{}`: \[{}\] )", escaped_id, new_status))?;
            new_content = re2.replace(&new_content, format!("${{1}}${{2}}(错误: {})", err)).to_string();
        }

        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .await?;
        file.write_all(new_content.as_bytes()).await?;
        file.flush().await?;

        Ok(())
    }

    /// 更新任务状态（简洁版）
    pub async fn update_status_v2(
        workspace_dir: &PathBuf,
        id: &str,
        new_status: &str,
        _error_msg: Option<&str>,
    ) -> Result<()> {
        static MUTEX: Mutex<()> = Mutex::const_new(());
        let _guard = MUTEX.lock().await;

        let path = Self::tasks_path(workspace_dir);
        if !path.exists() {
            return Ok(());
        }

        let mut content = String::new();
        let mut file = OpenOptions::new()
            .read(true)
            .open(&path)
            .await?;
        file.read_to_string(&mut content).await?;
        drop(file);

        // 使用更精确的正则：匹配 `id`: [任意状态] 并替换
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

        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .await?;
        file.write_all(new_content.as_bytes()).await?;
        file.flush().await?;

        Ok(())
    }

    /// 读取任务列表，格式化为 XML 标签供 Prompt 使用
    ///
    /// 只返回活跃任务（Pending 和 Running）
    pub async fn read_context(workspace_dir: &PathBuf) -> Result<String> {
        let path = Self::tasks_path(workspace_dir);
        if !path.exists() {
            return Ok(String::new());
        }

        let mut content = String::new();
        let mut file = OpenOptions::new()
            .read(true)
            .open(&path)
            .await?;
        file.read_to_string(&mut content).await?;
        drop(file);

        let lines: Vec<&str> = content.lines().collect();
        let mut active_tasks = Vec::new();

        for line in lines {
            // 匹配任务行：* `task-id`: [status] description
            if let Some(captures) = Regex::new(r"^\* `([^`]+)`: \[(\w+)\] (.+)$")?.captures(line) {
                let status = captures.get(2).map(|m| m.as_str()).unwrap_or("");
                if status == "Pending" || status == "Running" {
                    let id = captures.get(1).map(|m| m.as_str()).unwrap_or("");
                    let desc = captures.get(3).map(|m| m.as_str()).unwrap_or("");
                    active_tasks.push(format!("* `{}`: [{}] {}", id, status, desc));
                }
            }
        }

        if active_tasks.is_empty() {
            return Ok(String::new());
        }

        Ok(format!(
            "\n\n<current_tasks>\n{}\n</current_tasks>",
            active_tasks.join("\n")
        ))
    }

    /// 清理孤儿任务
    ///
    /// 在 Daemon 重启时调用，将所有 [Running] 状态修正为 [Failed]
    pub async fn sweep_orphans(workspace_dir: &PathBuf) -> Result<()> {
        static MUTEX: Mutex<()> = Mutex::const_new(());
        let _guard = MUTEX.lock().await;

        let path = Self::tasks_path(workspace_dir);
        if !path.exists() {
            return Ok(());
        }

        let mut content = String::new();
        let mut file = OpenOptions::new()
            .read(true)
            .open(&path)
            .await?;
        file.read_to_string(&mut content).await?;
        drop(file);

        // 将所有 [Running] 替换为 [Failed]
        let re = Regex::new(r"(\[)Running(\])")?;
        let new_content = re.replace_all(&content, "${1}Failed${2}");

        if new_content != content {
            let mut file = OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)
                .await?;
            file.write_all(new_content.as_bytes()).await?;
            file.flush().await?;
        }

        Ok(())
    }
}
