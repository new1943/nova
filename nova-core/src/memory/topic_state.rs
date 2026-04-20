//! Topic State Machine — tracks conversation topics lifecycle.
//!
//! Topics go through states: Started → Active → Suspended → Archived
//! TopicTracker detects topic switches via signal words and Compact events.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Topic status enum
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TopicStatus {
    /// New topic just started
    Started,
    /// Topic is ongoing
    Active,
    /// Topic temporarily suspended, can be resumed
    Suspended,
    /// Topic concluded and archived
    Archived,
}

/// A single topic with full metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topic {
    /// Unique topic ID
    pub id: String,
    /// Topic name
    pub name: String,
    /// Current status
    pub status: TopicStatus,
    /// When topic started
    pub started_at: DateTime<Utc>,
    /// When status last changed
    pub updated_at: DateTime<Utc>,
    /// Summary (generated at archive time)
    pub summary: Option<String>,
    /// Key conclusions from this topic
    pub conclusions: Vec<String>,
}

/// Topic summary for MEMORY.md (lighter weight version)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicSummary {
    /// Topic name
    pub name: String,
    /// Current status
    pub status: TopicStatus,
    /// Summary
    pub summary: String,
    /// Last update time (RFC3339)
    pub updated_at: String,
}

impl Topic {
    /// Create a new topic with Started status
    pub fn new(name: String) -> Self {
        let now = Utc::now();
        Self {
            id: uuid_v4(),
            name,
            status: TopicStatus::Started,
            started_at: now,
            updated_at: now,
            summary: None,
            conclusions: Vec::new(),
        }
    }

    /// Update status and timestamp
    pub fn set_status(&mut self, status: TopicStatus) {
        self.status = status;
        self.updated_at = Utc::now();
    }

    /// Add a conclusion to this topic
    pub fn add_conclusion(&mut self, conclusion: String) {
        self.conclusions.push(conclusion);
        self.updated_at = Utc::now();
    }

    /// Archive this topic with a summary
    pub fn archive(&mut self, summary: String) {
        self.status = TopicStatus::Archived;
        self.summary = Some(summary);
        self.updated_at = Utc::now();
    }
}

/// Topic transition result
#[derive(Debug, Clone, PartialEq)]
pub enum TopicTransition {
    /// Continue current topic
    Continue,
    /// Start a new topic
    NewTopic,
    /// Suspend current topic
    Suspend,
    /// Archive current topic
    Archive,
}

/// Signal words that indicate topic switching
const TOPIC_SWITCH_SIGNALS: &[&str] = &[
    "好",
    "搞定",
    "下一个",
    "换个话题",
    "先这样",
    "好了",
    "结束",
    "完成",
];

/// Topic tracker config
#[derive(Debug, Clone)]
pub struct TopicTrackerConfig {
    /// Minimum topic name length
    pub min_topic_length: usize,
    /// Auto-suspend after N turns of inactivity
    pub auto_suspend_turns: usize,
}

impl Default for TopicTrackerConfig {
    fn default() -> Self {
        Self {
            min_topic_length: 2,
            auto_suspend_turns: 10,
        }
    }
}

/// Topic tracker — manages current and historical topics
pub struct TopicTracker {
    /// Current active topic
    current_topic: RwLock<Option<Topic>>,
    /// Historical topics (archived/suspended)
    history: RwLock<Vec<Topic>>,
    /// Turns since last user message in current topic
    inactive_turns: RwLock<usize>,
    config: TopicTrackerConfig,
}

impl TopicTracker {
    /// Create a new TopicTracker
    pub fn new() -> Self {
        Self {
            current_topic: RwLock::new(None),
            history: RwLock::new(Vec::new()),
            inactive_turns: RwLock::new(0),
            config: TopicTrackerConfig::default(),
        }
    }

    /// Create with custom config
    pub fn with_config(config: TopicTrackerConfig) -> Self {
        Self {
            current_topic: RwLock::new(None),
            history: RwLock::new(Vec::new()),
            inactive_turns: RwLock::new(0),
            config,
        }
    }

    /// Process user message, detect topic transitions
    pub async fn on_user_message(&self, content: &str) -> TopicTransition {
        // Check for topic switch signals
        let has_signal = TOPIC_SWITCH_SIGNALS
            .iter()
            .any(|s| content.contains(s));

        if has_signal {
            // Archive or suspend current topic
            let transition = self.archive_current_topic().await;
            // Reset inactive turns
            *self.inactive_turns.write().await = 0;
            return transition;
        }

        // Increment inactive turns
        let mut turns = self.inactive_turns.write().await;
        *turns += 1;

        // Check for auto-suspend
        if *turns >= self.config.auto_suspend_turns {
            let _ = self.suspend_current_topic().await;
            *turns = 0;
            return TopicTransition::Suspend;
        }

        TopicTransition::Continue
    }

    /// Record a tool call (resets inactive turns)
    pub async fn on_tool_call(&self) {
        *self.inactive_turns.write().await = 0;
    }

    /// Handle compact result — archive topics mentioned
    pub async fn on_compact(&self, archived_topics: &[String]) -> Option<TopicTransition> {
        if archived_topics.is_empty() {
            return None;
        }

        // Archive current topic if it's in the archived list
        let current = self.current_topic.read().await;
        if let Some(ref topic) = *current {
            if archived_topics.iter().any(|n| topic.name.contains(n) || n.contains(&topic.name)) {
                drop(current);
                return Some(self.archive_current_topic().await);
            }
        }
        None
    }

    /// Start a new topic
    pub async fn start_topic(&self, name: String) {
        let mut current = self.current_topic.write().await;
        // Archive existing topic first
        if current.is_some() {
            let mut existing = current.take().unwrap();
            existing.archive(format!("Switched to new topic: {}", name));
            self.history.write().await.push(existing);
        }
        // Start new topic
        *current = Some(Topic::new(name));
        *self.inactive_turns.write().await = 0;
    }

    /// Get current topic (clone)
    pub async fn current_topic(&self) -> Option<Topic> {
        self.current_topic.read().await.clone()
    }

    /// Get all active/suspended topics from history
    pub async fn active_topics(&self) -> Vec<Topic> {
        self.history
            .read()
            .await
            .iter()
            .filter(|t| {
                t.status == TopicStatus::Active || t.status == TopicStatus::Suspended
            })
            .cloned()
            .collect()
    }

    /// Get all archived topics
    pub async fn archived_topics(&self) -> Vec<Topic> {
        self.history
            .read()
            .await
            .iter()
            .filter(|t| t.status == TopicStatus::Archived)
            .cloned()
            .collect()
    }

    /// Archive current topic and move to history
    async fn archive_current_topic(&self) -> TopicTransition {
        let mut current = self.current_topic.write().await;
        if let Some(mut topic) = current.take() {
            topic.set_status(TopicStatus::Archived);
            topic.updated_at = Utc::now();
            self.history.write().await.push(topic);
        }
        TopicTransition::Archive
    }

    /// Suspend current topic (move to history as Suspended)
    async fn suspend_current_topic(&self) {
        let mut current = self.current_topic.write().await;
        if let Some(mut topic) = current.take() {
            topic.set_status(TopicStatus::Suspended);
            self.history.write().await.push(topic);
        }
    }

    /// Update topic name if it changed
    pub async fn update_topic_name(&self, new_name: String) {
        let mut current = self.current_topic.write().await;
        if let Some(ref mut topic) = *current {
            topic.name = new_name;
            topic.updated_at = Utc::now();
        }
    }
}

impl Default for TopicTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Simple UUID v4 generator (no external dependency)
fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let random: u64 = timestamp.wrapping_mul(0x517cc1b727220a95)
        .wrapping_add(std::process::id() as u64);
    format!("{:016x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        random,
        (random >> 48) as u16 & 0xfff,
        (random >> 36) as u16 & 0xfff,
        ((random >> 32) as u16 & 0x3fff) | 0x8000,
        (random & 0xffffffffffff)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_start_topic() {
        let tracker = TopicTracker::new();
        tracker.start_topic("Test Topic".to_string()).await;

        let current = tracker.current_topic().await;
        assert!(current.is_some());
        let topic = current.unwrap();
        assert_eq!(topic.name, "Test Topic");
        assert_eq!(topic.status, TopicStatus::Started);
    }

    #[tokio::test]
    async fn test_topic_switch_signal() {
        let tracker = TopicTracker::new();
        tracker.start_topic("Topic A".to_string()).await;

        let transition = tracker.on_user_message("好，换个话题").await;
        assert_eq!(transition, TopicTransition::Archive);

        // New topic should be started next
        let current = tracker.current_topic().await;
        assert!(current.is_none()); // Current is cleared after archive
    }

    #[tokio::test]
    async fn test_continue_topic() {
        let tracker = TopicTracker::new();
        tracker.start_topic("My Topic".to_string()).await;

        let transition = tracker.on_user_message("让我继续说说这个").await;
        assert_eq!(transition, TopicTransition::Continue);
    }

    #[tokio::test]
    async fn test_archive_topic() {
        let tracker = TopicTracker::new();
        tracker.start_topic("Topic to Archive".to_string()).await;

        let _ = tracker.archive_current_topic().await;

        let history = tracker.archived_topics().await;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].name, "Topic to Archive");
        assert_eq!(history[0].status, TopicStatus::Archived);
    }
}
