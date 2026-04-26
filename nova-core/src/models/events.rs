//! ShadowEvent — the unified event bus protocol for Nova V4.
//!
//! This module defines the single event type that flows through the system's
//! central mpsc channel, connecting the main Agent/QueryLoop to background
//! services like TaskManager and MemoryKeeper.
//!
//! Design principles:
//! - Fire-and-forget: emitters drop events into the channel and continue immediately
//! - All heavy work (file I/O, LLM calls) happens in the Dispatcher/handlers
//! - ShadowEvents are purely data — no business logic

use crate::message::Message;

/// Task action for TaskProgress events
#[derive(Debug, Clone)]
pub enum TaskAction {
    /// A new task was added to the board
    Add,
    /// A task's status or description was updated
    Update,
    /// A task was marked complete (shown as `- [x]` briefly before cleanup)
    Complete,
    /// A task was physically erased from Tasks.md ("阅后即焚")
    Remove,
}

/// Shadow event — unified event bus for all background notifications.
///
/// Emitters: QueryLoop, Coordinator, SubAgent workers, Heartbeat
/// Receivers: Dispatcher → TaskManager / MemoryKeeper
///
/// ## Channel capacity
/// Use a bounded channel with capacity ≥ 100 so that temporary backpressure
/// does not block emitters. The Dispatcher handles overflow with the
/// backpressure策略 defined in `interface_contract.md`.
#[derive(Debug, Clone)]
pub enum ShadowEvent {
    /// High-frequency, lightweight: task board mutation.
    /// Routed to TaskManager for Tasks.md CRUD.
    TaskProgress {
        task_id: String,
        action: TaskAction,
        description: String,
    },

    /// Low-frequency, heavy: a topic was naturally concluded.
    /// Routed to MemoryKeeper for async summarisation via SideQuery.
    TopicArchived {
        transcript: Vec<Message>,
    },

    /// Low-frequency, heavy: system-wide idle detected by Heartbeat.
    /// Routed to MemoryKeeper to flush pending buffered topics.
    SystemIdle {
        duration_secs: u64,
        transcript: Vec<Message>,
    },

    /// Low-frequency, async: a complex delegated project finished.
    /// Routed to Dispatcher for Discord proactive push and IPC push to TUI.
    ProjectCompleted {
        project_id: String,
        report: String,
        channel_id: String,
    },
}
