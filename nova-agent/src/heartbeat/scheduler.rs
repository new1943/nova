use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, error};

/// A heartbeat task parsed from HEARTBEAT.md
#[derive(Debug, Clone)]
pub struct HeartbeatTask {
    pub name: String,
    pub prompt: String,
}

/// Heartbeat scheduler — reads HEARTBEAT.md, fires tasks periodically
pub struct HeartbeatScheduler {
    interval: Duration,
    tasks: Vec<HeartbeatTask>,
}

impl HeartbeatScheduler {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            tasks: Vec::new(),
        }
    }

    /// Parse HEARTBEAT.md content into tasks.
    pub fn from_config(heartbeat_md: &str, interval: Duration) -> Self {
        let mut scheduler = Self::new(interval);
        let mut current_name: Option<String> = None;
        let mut current_body = String::new();

        for line in heartbeat_md.lines() {
            if let Some(name) = line.strip_prefix("## ") {
                if let Some(prev_name) = current_name.take() {
                    let prompt = current_body.trim().to_string();
                    if !prompt.is_empty() {
                        scheduler.tasks.push(HeartbeatTask {
                            name: prev_name,
                            prompt,
                        });
                    }
                }
                current_name = Some(name.trim().to_string());
                current_body.clear();
            } else if current_name.is_some() {
                current_body.push_str(line);
                current_body.push('\n');
            }
        }

        if let Some(name) = current_name {
            let prompt = current_body.trim().to_string();
            if !prompt.is_empty() {
                scheduler.tasks.push(HeartbeatTask { name, prompt });
            }
        }

        scheduler
    }

    pub fn tasks(&self) -> &[HeartbeatTask] {
        &self.tasks
    }

    /// Start the heartbeat loop in a background tokio task.
    pub fn start(self, notify_tx: mpsc::Sender<HeartbeatEvent>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!("Heartbeat started, interval: {:?}, {} tasks", self.interval, self.tasks.len());
            loop {
                tokio::time::sleep(self.interval).await;

                for task in &self.tasks {
                    info!("Heartbeat firing: {}", task.name);
                    if notify_tx.send(HeartbeatEvent {
                        task_name: task.name.clone(),
                        prompt: task.prompt.clone(),
                    }).await.is_err() {
                        error!("Heartbeat channel closed");
                        return;
                    }
                }
            }
        })
    }
}

/// Event emitted by the heartbeat scheduler
#[derive(Debug, Clone)]
pub struct HeartbeatEvent {
    pub task_name: String,
    pub prompt: String,
}
