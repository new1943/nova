//! nova-daemon library crate — exposes internal modules for integration testing.

pub mod dispatcher;
pub mod task_manager;

/// Discord proactive push message (used by Dispatcher for ProjectCompleted events).
#[derive(Clone, Debug)]
pub struct DiscordPush {
    pub channel_id: String,
    pub content: String,
}
