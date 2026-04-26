//! TaskManager — manages the physical Tasks.md kanban board.
//!
//! Design: Single writer, fire-and-forget.
//! - Dispatcher sends `TaskProgress` events → TaskManager handles them
//! - All file I/O is synchronous and very fast (< 10ms)
//! - On write failure: retry 3×, then log and drop (non-blocking)

use std::path::PathBuf;
use std::io::Write;
use nova_core::models::{ShadowEvent, TaskAction};
use anyhow::Result;
use regex::Regex;
use tracing::{info, warn, debug};

const MAX_RETRIES: u8 = 3;
const TASKS_FILE: &str = "Tasks.md";

/// TaskManager: manages Tasks.md as a fire-and-forget WAL-like kanban.
/// Instantiated once per daemon, owned by the Dispatcher.
pub struct TaskManager {
    tasks_path: PathBuf,
}

impl TaskManager {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self {
            tasks_path: workspace_dir.join(TASKS_FILE),
        }
    }

    /// Handle a TaskProgress event — update Tasks.md accordingly.
    pub fn handle(&self, event: &ShadowEvent) -> Result<()> {
        match event {
            ShadowEvent::TaskProgress { task_id, action, description } => {
                match action {
                    TaskAction::Add => self.add_task(task_id, description),
                    TaskAction::Update => self.update_task(task_id, description),
                    TaskAction::Complete => self.complete_task(task_id),
                    TaskAction::Remove => self.remove_task(task_id),
                }
            }
            _ => {
                // TaskManager only handles TaskProgress; other events are logged and dropped
                debug!("TaskManager received non-TaskProgress event, ignoring");
                Ok(())
            }
        }
    }

    fn add_task(&self, task_id: &str, description: &str) -> Result<()> {
        let line = format!("- [ ] [{}] {}\n", task_id, description);
        let mut retries = 0;
        loop {
            match self.append_to_file(&line) {
                Ok(_) => {
                    info!("TaskManager: added task [{}]", task_id);
                    return Ok(());
                }
                Err(e) if retries < MAX_RETRIES => {
                    retries += 1;
                    warn!("TaskManager: add failed (retry {}/{}): {}", retries, MAX_RETRIES, e);
                }
                Err(e) => {
                    warn!("TaskManager: add task [{}] failed after {} retries: {}", task_id, MAX_RETRIES, e);
                    return Ok(()); // Non-blocking — don't propagate
                }
            }
        }
    }

    fn update_task(&self, task_id: &str, description: &str) -> Result<()> {
        self.update_line_matching(task_id, |_old_line| {
            Some(format!("- [ ] [{}] {}", task_id, description))
        })
    }

    fn complete_task(&self, task_id: &str) -> Result<()> {
        self.update_line_matching(task_id, |old_line| {
            if old_line.starts_with("- [ ]") {
                Some(old_line.replace("- [ ]", "- [x]"))
            } else {
                None
            }
        })
    }

    fn remove_task(&self, task_id: &str) -> Result<()> {
        let escaped_id = regex::escape(task_id);
        // Pattern: "- [ ] [task_id] description" or "- [x] [task_id] description"
        let re = Regex::new(&format!(r"(?m)^(- \[.?\] \[{}\] .+)$", escaped_id))?;
        let content = self.read_file()?;
        let updated = re.replace_all(&content, "");
        self.write_file(updated.trim())?;
        info!("TaskManager: removed task [{}]", task_id);
        Ok(())
    }

    /// Update a task line matching task_id. If the closure returns Some(new_line), replace it.
    fn update_line_matching<F>(&self, task_id: &str, f: F) -> Result<()>
    where
        F: Fn(&str) -> Option<String>,
    {
        let escaped_id = regex::escape(task_id);
        let re = Regex::new(&format!(r"(?m)^(- \[.?\] \[{}\] .+)$", escaped_id))?;
        let content = self.read_file()?;
        let mut made_replacement = false;
        let mut updated = content.clone();

        for cap in re.captures_iter(&content) {
            if let Some(m) = cap.get(0) {
                let old_line = m.as_str();
                if let Some(new_line) = f(old_line) {
                    updated = updated.replacen(old_line, &new_line, 1);
                    made_replacement = true;
                    break;
                }
            }
        }

        if made_replacement {
            self.write_file(updated.trim())?;
            info!("TaskManager: updated task [{}]", task_id);
        } else {
            debug!("TaskManager: task [{}] not found for update", task_id);
        }
        Ok(())
    }

    /// Physically erase all completed (`- [x]`) tasks from Tasks.md.
    /// Called by Dispatcher when receiving SystemIdle event.
    /// Returns the number of tasks cleaned.
    pub fn cleanup_completed(&self) -> Result<usize> {
        let re = Regex::new(r"(?m)^(- \[x\] \[.+?\] .+)$")?;
        let content = self.read_file()?;
        let mut cleaned_lines = Vec::new();
        let mut count = 0;

        for line in content.lines() {
            if re.is_match(line) {
                count += 1;
            } else {
                cleaned_lines.push(line);
            }
        }

        if count > 0 {
            self.write_file(cleaned_lines.join("\n"))?;
            info!("TaskManager: cleaned {} completed tasks", count);
        }
        Ok(count)
    }

    fn read_file(&self) -> Result<String> {
        if !self.tasks_path.exists() {
            return Ok(String::new());
        }
        Ok(std::fs::read_to_string(&self.tasks_path)?)
    }

    fn write_file(&self, content: impl AsRef<str>) -> Result<()> {
        std::fs::write(&self.tasks_path, content.as_ref())?;
        Ok(())
    }

    fn append_to_file(&self, line: &str) -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.tasks_path)?;
        file.write_all(line.as_bytes())?;
        Ok(())
    }
}

impl Clone for TaskManager {
    fn clone(&self) -> Self {
        Self {
            tasks_path: self.tasks_path.clone(),
        }
    }
}
