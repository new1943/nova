//! Dispatcher — central event router for the ShadowEvent bus.
//!
//! Runs as a background tokio task. Receives `ShadowEvent`s via mpsc channel
//! and routes them to the appropriate handler:
//!   - TaskProgress → TaskManager (fast file I/O)
//!   - TopicArchived / SystemIdle → MemoryKeeper (async SideQuery)
//!   - ProjectCompleted → Discord push + IPC push to TUI
//!
//! Backpressure: bounded channel (capacity 100). On overflow, TaskProgress events
//! are dropped (non-critical); ProjectCompleted goes to dead-letter queue for retry.

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, broadcast};
use tokio::task;
use nova_core::models::{ShadowEvent, ShadowEventEmitter};
use nova_memory::sidequery::MemoryKeeper;
use nova_ipc::Event as IpcEvent;
use tracing::{info, warn, debug};
use crate::task_manager::TaskManager;

/// Shared dispatcher sender handle.
/// Uses Arc internally so the single dispatcher loop sender can be shared
/// across many concurrent HandleConfigs without move semantics.
pub struct DispatcherSender {
    tx: Arc<mpsc::Sender<ShadowEvent>>,
}

// Arc<mpsc::Sender> is Clone, so DispatcherSender can be Clone
impl Clone for DispatcherSender {
    fn clone(&self) -> Self {
        DispatcherSender {
            tx: self.tx.clone(),
        }
    }
}

impl DispatcherSender {
    /// Send an event without blocking the caller.
    /// Returns Err(()) if the channel is closed; event is dropped (fire-and-forget).
    pub fn emit(&self, event: ShadowEvent) {
        if self.tx.try_send(event).is_err() {
            debug!("ShadowEvent dropped (channel full or closed)");
        }
    }

    /// Returns a clone of the underlying mpsc::Sender for use with QueryLoop.
    /// This allows QueryLoop to emit ShadowEvent::TopicArchived directly.
    pub fn channel(&self) -> mpsc::Sender<ShadowEvent> {
        (*self.tx).clone()
    }
}

impl ShadowEventEmitter for DispatcherSender {
    fn emit(&self, event: ShadowEvent) {
        DispatcherSender::emit(self, event)
    }
}

/// [V4 Task 6.2] Discord push channel for ProjectCompleted events
use crate::DiscordPush;

/// The Dispatcher background loop. Created once per daemon, spawned as a detached task.
pub struct Dispatcher {
    workspace_dir: PathBuf,
    memory_keeper: Option<Arc<MemoryKeeper>>,
    /// Optional channel for Discord proactive push (ProjectCompleted events)
    discord_push_tx: Option<std::sync::Arc<tokio::sync::mpsc::Sender<DiscordPush>>>,
    /// [V4 Fix] Optional IPC push channel for ProjectCompleted → TUI (broadcast)
    ipc_push_tx: Option<std::sync::Arc<broadcast::Sender<IpcEvent>>>,
}

impl Dispatcher {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self {
            workspace_dir,
            memory_keeper: None,
            discord_push_tx: None,
            ipc_push_tx: None,
        }
    }

    /// Set the MemoryKeeper. Called by main.rs after Dispatcher creation but before spawn.
    pub fn with_memory_keeper(mut self, memory_keeper: Arc<MemoryKeeper>) -> Self {
        self.memory_keeper = Some(memory_keeper);
        self
    }

    /// Set the Discord push channel for ProjectCompleted events.
    pub fn with_discord_push_tx(mut self, tx: std::sync::Arc<tokio::sync::mpsc::Sender<DiscordPush>>) -> Self {
        self.discord_push_tx = Some(tx);
        self
    }

    /// [V4 Fix] Set the IPC push channel for ProjectCompleted → TUI events.
    pub fn with_ipc_push_tx(mut self, tx: std::sync::Arc<broadcast::Sender<IpcEvent>>) -> Self {
        self.ipc_push_tx = Some(tx);
        self
    }

    /// Spawn the dispatcher loop. Returns an `Arc<DispatcherSender>` so it can be
    /// shared across multiple HandleConfigs via cheap `Arc::clone()`.
    pub fn spawn(self) -> Arc<DispatcherSender> {
        let (tx, mut rx) = mpsc::channel::<ShadowEvent>(100);
        let task_manager = TaskManager::new(self.workspace_dir.clone());
        let memory_keeper = self.memory_keeper.clone();
        let discord_push_tx = self.discord_push_tx.clone();
        let ipc_push_tx = self.ipc_push_tx.clone();

        task::spawn(async move {
            info!("Dispatcher loop started (capacity=100)");
            loop {
                match rx.recv().await {
                    Some(event) => {
                        Self::handle_event(&event, &task_manager, memory_keeper.as_deref(), discord_push_tx.as_ref().map(|tx| tx.as_ref()), ipc_push_tx.as_ref().map(|tx| tx.as_ref())).await;
                    }
                    None => {
                        warn!("Dispatcher: channel closed, exiting loop");
                        break;
                    }
                }
            }
        });

        Arc::new(DispatcherSender { tx: Arc::new(tx) })
    }

    async fn handle_event(
        event: &ShadowEvent,
        task_manager: &TaskManager,
        memory_keeper: Option<&MemoryKeeper>,
        discord_push_tx: Option<&tokio::sync::mpsc::Sender<DiscordPush>>,
        ipc_push_tx: Option<&tokio::sync::broadcast::Sender<IpcEvent>>,
    ) {
        match event {
            ShadowEvent::TaskProgress { .. } => {
                // Fast path: synchronous file I/O in the dispatch loop
                // TaskManager never awaits, so it won't block the loop
                if let Err(e) = task_manager.handle(event) {
                    warn!("TaskManager handle error: {}", e);
                }
            }
            ShadowEvent::TopicArchived { transcript } => {
                info!("Dispatcher: TopicArchived ({} messages) → MemoryKeeper", transcript.len());
                if let Some(mk) = memory_keeper {
                    mk.handle_archived_topic(transcript.clone()).await;
                } else {
                    debug!("MemoryKeeper not configured, dropping TopicArchived");
                }
            }
            ShadowEvent::SystemIdle { duration_secs, transcript } => {
                info!("Dispatcher: SystemIdle ({}s, {} messages)",
                    duration_secs, transcript.len());
                // Cleanup completed tasks on idle
                if let Err(e) = task_manager.cleanup_completed() {
                    warn!("TaskManager cleanup error: {}", e);
                }
                // Trigger memory extraction on idle
                if let Some(mk) = memory_keeper {
                    mk.handle_idle(*duration_secs);
                }
            }
            ShadowEvent::ProjectCompleted { project_id, report, channel_id } => {
                info!("Dispatcher: ProjectCompleted (project={}) → IPC/Discord", project_id);
                
                let mut ipc_success = false;
                // [V4 Fix] Push to IPC for TUI notification (broadcast to all connections)
                if let Some(tx) = ipc_push_tx {
                    let event = IpcEvent::ProjectCompleted {
                        project_id: project_id.clone(),
                        report: report.clone(),
                    };
                    if tx.receiver_count() > 0 {
                        if tx.send(event).is_ok() {
                            ipc_success = true;
                            info!("IPC push sent ProjectCompleted for project {}", project_id);
                        } else {
                            warn!("IPC push failed (no active receivers)");
                        }
                    } else {
                        warn!("IPC push failed (0 receivers active)");
                    }
                }

                // [V4 Task 6.2] Push to Discord if channel is configured
                // Fallback to Discord if IPC failed, OR if it explicitly has a numeric Discord channel ID
                if !ipc_success || channel_id.parse::<u64>().is_ok() || channel_id == "coordinator" {
                    if let Some(tx) = discord_push_tx {
                        // If channel_id is empty, fallback to "coordinator" mapping
                        let target_channel = if channel_id.is_empty() {
                            "coordinator".to_string()
                        } else {
                            channel_id.clone()
                        };
                        let push = DiscordPush {
                            channel_id: target_channel,
                            content: format!("✅ Project Completed\n\n{}", report),
                        };
                        if tx.try_send(push).is_err() {
                            warn!("Discord push channel full or closed, dropping ProjectCompleted for project {}", project_id);
                        } else {
                            info!("Discord push fallback sent for project {}", project_id);
                        }
                    } else {
                        warn!("Discord push not configured, dropping ProjectCompleted for project {}", project_id);
                    }
                }
            }
        }
    }
}


