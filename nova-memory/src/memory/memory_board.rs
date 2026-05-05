//! Memory Board — MEMORY.md whiteboard management.
//!
//! Principle: Only keep Active and Suspended topics.
//! When a topic is archived, it is physically removed from MEMORY.md.
//! Permanent preferences are never cleaned.

use std::fs;
use std::path::PathBuf;
use tokio::sync::RwLock;

use super::topic_state::TopicStatus;
use super::topic_state::TopicSummary; // Re-export from topic_state

/// Preference store
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PreferenceStore {
    pub items: Vec<String>,
}

impl PreferenceStore {
    /// Add a new preference
    pub fn add(&mut self, pref: String) {
        if !self.items.contains(&pref) {
            self.items.push(pref);
        }
    }

    /// Remove a preference
    pub fn remove(&mut self, pref: &str) {
        self.items.retain(|p| p != pref);
    }

    /// Check if contains preference
    pub fn contains(&self, pref: &str) -> bool {
        self.items.iter().any(|p| p == pref)
    }
}

/// Memory board — manages MEMORY.md content
pub struct MemoryBoard {
    /// Path to MEMORY.md
    path: PathBuf,
    /// Active/suspended topics
    active_topics: RwLock<Vec<TopicSummary>>,
    /// Permanent preferences
    permanent_preferences: RwLock<PreferenceStore>,
}

impl MemoryBoard {
    /// Create a new MemoryBoard
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            active_topics: RwLock::new(Vec::new()),
            permanent_preferences: RwLock::new(PreferenceStore::default()),
        }
    }

    /// Load MEMORY.md from disk
    pub async fn load(&self) -> std::io::Result<()> {
        if !self.path.exists() {
            return Ok(());
        }

        let content = fs::read_to_string(&self.path)?;

        // Parse existing content
        // Expected format:
        // # 记忆
        //
        // ## 当前话题
        // - 🔵 TopicName: summary
        // - 🟡 SuspendedTopic: summary
        //
        // ## 偏好
        // - preference 1
        // - preference 2

        let mut topics = Vec::new();
        let mut preferences = Vec::new();
        let mut current_section = None;

        for line in content.lines() {
            let trimmed = line.trim();

            if trimmed == "## 当前话题" {
                current_section = Some("topics");
                continue;
            } else if trimmed == "## 偏好" {
                current_section = Some("preferences");
                continue;
            } else if trimmed.starts_with("# ") || trimmed.is_empty() {
                continue;
            }

            if let Some(section) = current_section {
                if section == "topics" && trimmed.starts_with("- ") {
                    // Parse topic line: "- 🔵 TopicName: summary" or "- 🟡 TopicName: summary"
                    let content_part = trimmed.trim_start_matches("- ");
                    let (emoji, rest) = content_part.split_at(3);
                    let rest = rest.trim_start_matches(' ');

                    let (name, summary) = if let Some(pos) = rest.find(": ") {
                        (rest[..pos].to_string(), rest[pos + 2..].to_string())
                    } else {
                        (rest.to_string(), String::new())
                    };

                    let status = if emoji.contains("🔵") {
                        TopicStatus::Active
                    } else if emoji.contains("🟡") {
                        TopicStatus::Suspended
                    } else {
                        TopicStatus::Active
                    };

                    topics.push(TopicSummary {
                        name,
                        status,
                        summary,
                        updated_at: chrono::Utc::now().to_rfc3339(),
                    });
                } else if section == "preferences" && trimmed.starts_with("- ") {
                    let pref = trimmed.trim_start_matches("- ").to_string();
                    preferences.push(pref);
                }
            }
        }

        *self.active_topics.write().await = topics;
        *self.permanent_preferences.write().await = PreferenceStore { items: preferences };

        Ok(())
    }

    /// Save MEMORY.md to disk
    pub async fn save(&self) -> std::io::Result<()> {
        let content = self.render_to_markdown().await;
        fs::write(&self.path, content)
    }

    /// Update from compact result
    pub async fn update_from_compact(
        &self,
        archived_topics: &[String],
        extracted_preferences: &[String],
        active_summary: &str,
    ) -> std::io::Result<()> {
        // 1. Remove archived topics
        {
            let mut topics = self.active_topics.write().await;
            topics.retain(|t| {
                !archived_topics.iter().any(|n| t.name.contains(n) || n.contains(&t.name))
            });
        }

        // 2. Add active summary as new topic if not empty
        if !active_summary.is_empty() && active_summary != "无" {
            let mut topics = self.active_topics.write().await;
            // Check if there's already a current topic
            if !topics.iter().any(|t| t.status == TopicStatus::Active) {
                topics.push(TopicSummary {
                    name: active_summary.chars().take(50).collect(),
                    status: TopicStatus::Active,
                    summary: active_summary.to_string(),
                    updated_at: chrono::Utc::now().to_rfc3339(),
                });
            }
        }

        // 3. Add new preferences
        if !extracted_preferences.is_empty() {
            let mut prefs = self.permanent_preferences.write().await;
            for pref in extracted_preferences {
                prefs.add(pref.clone());
            }
        }

        // 4. Save to disk
        self.save().await
    }

    /// Add a new topic
    pub async fn add_topic(&self, name: String, summary: String) -> std::io::Result<()> {
        let mut topics = self.active_topics.write().await;

        // Remove if already exists
        topics.retain(|t| t.name != name);

        // Add new topic
        topics.push(TopicSummary {
            name,
            status: TopicStatus::Active,
            summary,
            updated_at: chrono::Utc::now().to_rfc3339(),
        });

        drop(topics);
        self.save().await
    }

    /// Archive a topic (remove from active list)
    pub async fn archive_topic(&self, name: &str) -> std::io::Result<()> {
        {
            let mut topics = self.active_topics.write().await;
            topics.retain(|t| t.name != name);
        }
        self.save().await
    }

    /// Suspend a topic
    pub async fn suspend_topic(&self, name: &str) -> std::io::Result<()> {
        {
            let mut topics = self.active_topics.write().await;
            if let Some(topic) = topics.iter_mut().find(|t| t.name == name) {
                topic.status = TopicStatus::Suspended;
                topic.updated_at = chrono::Utc::now().to_rfc3339();
            }
        }
        self.save().await
    }

    /// Add a preference
    pub async fn add_preference(&self, pref: String) -> std::io::Result<()> {
        {
            let mut prefs = self.permanent_preferences.write().await;
            prefs.add(pref);
        }
        self.save().await
    }

    /// Get all active topics
    pub async fn active_topics(&self) -> Vec<TopicSummary> {
        self.active_topics
            .read()
            .await
            .iter()
            .filter(|t| t.status == TopicStatus::Active)
            .cloned()
            .collect()
    }

    /// Get all suspended topics
    pub async fn suspended_topics(&self) -> Vec<TopicSummary> {
        self.active_topics
            .read()
            .await
            .iter()
            .filter(|t| t.status == TopicStatus::Suspended)
            .cloned()
            .collect()
    }

    /// Get all preferences
    pub async fn preferences(&self) -> Vec<String> {
        self.permanent_preferences.read().await.items.clone()
    }

    /// Render to markdown string
    async fn render_to_markdown(&self) -> String {
        let mut lines = vec![
            "# 记忆".to_string(),
            "".to_string(),
        ];

        // Active topics
        lines.push("## 当前话题".to_string());
        let topics = self.active_topics.read().await;
        if topics.is_empty() {
            lines.push("- （无进行中话题）".to_string());
        } else {
            for topic in topics.iter() {
                let status_icon = match topic.status {
                    TopicStatus::Active => "🔵",
                    TopicStatus::Suspended => "🟡",
                    TopicStatus::Started => "🟢",
                    TopicStatus::Archived => "⚫",
                };
                lines.push(format!(
                    "- {} {}: {}",
                    status_icon,
                    topic.name,
                    topic.summary
                ));
            }
        }
        lines.push("".to_string());

        // Preferences
        lines.push("## 偏好".to_string());
        let prefs = self.permanent_preferences.read().await;
        if prefs.items.is_empty() {
            lines.push("- （暂无偏好记录）".to_string());
        } else {
            for pref in &prefs.items {
                lines.push(format!("- {}", pref));
            }
        }
        lines.push("".to_string());

        lines.join("\n")
    }

    /// Prune topics to keep under limit
    pub async fn prune(&self, max_topics: usize) -> std::io::Result<()> {
        {
            let mut topics = self.active_topics.write().await;
            if topics.len() > max_topics {
                // Keep most recently updated
                topics.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                topics.truncate(max_topics);
            }
        }
        self.save().await
    }

    /// Check if memory is getting too large
    pub async fn needs_pruning(&self) -> bool {
        // Count lines
        let content = self.render_to_markdown().await;
        content.lines().count() > 200
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_add_topic() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("MEMORY_TEST.md");
        let board = MemoryBoard::new(path.clone());

        board.add_topic("Test Topic".to_string(), "Test summary".to_string()).await.unwrap();

        let topics = board.active_topics().await;
        assert_eq!(topics.len(), 1);
        assert_eq!(topics[0].name, "Test Topic");

        // Cleanup
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn test_archive_topic() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("MEMORY_TEST2.md");
        let board = MemoryBoard::new(path.clone());

        board.add_topic("Topic 1".to_string(), "Summary 1".to_string()).await.unwrap();
        board.add_topic("Topic 2".to_string(), "Summary 2".to_string()).await.unwrap();

        board.archive_topic("Topic 1").await.unwrap();

        let topics = board.active_topics().await;
        assert_eq!(topics.len(), 1);
        assert_eq!(topics[0].name, "Topic 2");

        // Cleanup
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn test_render_markdown() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("MEMORY_TEST3.md");
        let board = MemoryBoard::new(path.clone());

        board.add_topic("Active Topic".to_string(), "Summary".to_string()).await.unwrap();
        board.add_preference("Likes coffee".to_string()).await.unwrap();

        let content = board.render_to_markdown().await;

        assert!(content.contains("# 记忆"));
        assert!(content.contains("## 当前话题"));
        assert!(content.contains("🔵 Active Topic"));
        assert!(content.contains("## 偏好"));
        assert!(content.contains("Likes coffee"));

        // Cleanup
        let _ = std::fs::remove_file(path);
    }
}
