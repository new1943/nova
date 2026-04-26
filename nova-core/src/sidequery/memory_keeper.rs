//! MemoryKeeper — async long-term memory extraction via SideQuery.
//!
//! Design:
//! - Receives `ShadowEvent::TopicArchived` events and buffers them in memory
//! - When `ShadowEvent::SystemIdle` fires (Heartbeat idle detection),
//!   processes ALL buffered topics through SideQuery with MEMORY_GUIDANCE
//! - Writes extracted memories to MEMORY.md
//!
//! Key principle: never blocks the main loop — all heavy work (LLM calls, file I/O)
//! happens in background tasks spawned from `handle_idle`.

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::fs;
use crate::message::Message;
use super::query::SideQuery;

/// System prompt for memory extraction — strict guidance for what to record.
const MEMORY_GUIDANCE: &str = r#"你是一个记忆提炼专家。请分析以下对话记录，提炼出有价值的记忆条目。

## 提炼规则
- **只记录持久化知识**：用户偏好、避坑指南、架构决策、技术方案
- **不记录**：任务进度、具体代码细节、临时调试信息
- **输出格式**：每条记忆一行，格式为 `- 记忆内容`
- **数量控制**：最多提炼 5 条最有价值的信息
- **语言**：使用用户对话所用的语言

只输出记忆条目，不要其他文字。"#;

/// MemoryKeeper: manages async topic archival and memory extraction.
///
/// Lives in nova-core so it can use SideQuery without circular dependencies.
/// Instantiated by Dispatcher in nova-daemon.
pub struct MemoryKeeper {
    workspace_dir: PathBuf,
    side_query: SideQuery,
    /// Buffered topic transcripts waiting for idle-time processing
    /// Arc so that cloned MemoryKeepers share the same buffer
    buffer: Arc<RwLock<Vec<BufferedTopic>>>,
}

/// A single buffered topic awaiting memory extraction.
#[derive(Debug)]
struct BufferedTopic {
    transcript: Vec<Message>,
}

impl MemoryKeeper {
    /// Create a new MemoryKeeper.
    pub fn new(workspace_dir: PathBuf, side_query: SideQuery) -> Self {
        Self {
            workspace_dir,
            side_query,
            buffer: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Handle a TopicArchived event — buffer the transcript for later processing.
    pub async fn handle_archived_topic(&self, transcript: Vec<Message>) {
        if transcript.is_empty() {
            return;
        }
        let topic = BufferedTopic { transcript };
        self.buffer.write().await.push(topic);
        tracing::debug!("MemoryKeeper: buffered {} topics", self.buffer.read().await.len());
    }

    /// Handle SystemIdle — process ALL buffered topics through SideQuery.
    /// Spawns a background task and returns immediately (fire-and-forget).
    pub fn handle_idle(&self, duration_secs: u64) {
        if duration_secs < 60 {
            // Only process on meaningful idle (≥ 1 minute)
            tracing::debug!("MemoryKeeper: idle {}s < 60s, skipping", duration_secs);
            return;
        }

        // Take ownership of buffer and other data for the background task
        let buffer = self.buffer.clone();
        let workspace_dir = self.workspace_dir.clone();
        let side_query = self.side_query.clone();

        tokio::spawn(async move {
            let result = Self::process_buffer(buffer, workspace_dir, side_query).await;
            match result {
                Ok(count) => {
                    tracing::info!("MemoryKeeper: extracted {} memory entries", count);
                }
                Err(e) => {
                    tracing::warn!("MemoryKeeper: extraction failed: {}", e);
                }
            }
        });
    }

    /// Process all buffered topics: call SideQuery for each, collect results, write MEMORY.md.
    async fn process_buffer(
        buffer: Arc<RwLock<Vec<BufferedTopic>>>,
        workspace_dir: PathBuf,
        side_query: SideQuery,
    ) -> Result<usize, anyhow::Error> {
        // Take all buffered topics
        let topics: Vec<BufferedTopic> = {
            let mut guard = buffer.write().await;
            std::mem::take(&mut *guard)
        };

        if topics.is_empty() {
            tracing::debug!("MemoryKeeper: no topics to process");
            return Ok(0);
        }

        tracing::info!("MemoryKeeper: processing {} buffered topics", topics.len());

        let mut all_memories = Vec::new();

        for topic in topics {
            let conversation = Self::format_transcript(&topic.transcript);
            if conversation.trim().is_empty() {
                continue;
            }

            match side_query.query_await(MEMORY_GUIDANCE, &conversation).await {
                Ok(result) => {
                    // Parse result — each line starting with "- " is a memory entry
                    let memories: Vec<String> = result
                        .lines()
                        .filter(|line| line.trim().starts_with("- "))
                        .map(|line| line.trim()[2..].to_string())
                        .collect();
                    all_memories.extend(memories);
                }
                Err(e) => {
                    tracing::warn!("MemoryKeeper: failed to extract memory from transcript: {}", e);
                }
            }
        }

        if all_memories.is_empty() {
            tracing::debug!("MemoryKeeper: no memories extracted");
            return Ok(0);
        }

        // Deduplicate
        all_memories.sort();
        all_memories.dedup();

        // Load existing MEMORY.md and merge
        let memory_path = workspace_dir.join("MEMORY.md");
        let existing = match fs::read_to_string(&memory_path).await {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(anyhow::anyhow!("failed to read MEMORY.md: {}", e)),
        };

        let updated = Self::merge_memories(&existing, all_memories.clone());
        fs::write(&memory_path, updated).await?;

        tracing::info!("MemoryKeeper: wrote memories to MEMORY.md");
        Ok(all_memories.len())
    }

    /// Get the current buffer size (for debugging/testing).
    pub async fn buffer_size(&self) -> usize {
        self.buffer.read().await.len()
    }

    /// Format a transcript for the LLM.
    fn format_transcript(transcript: &[Message]) -> String {
        let mut s = String::new();
        for msg in transcript {
            let role = match msg.role {
                crate::message::Role::User => "User",
                crate::message::Role::Assistant => "Assistant",
                crate::message::Role::Tool => "Tool",
                crate::message::Role::System => "System",
            };
            if let Some(ref content) = msg.content {
                s.push_str(&format!("{}: {}\n", role, content));
            }
        }
        s
    }

    /// Merge new memories into existing MEMORY.md content.
    fn merge_memories(existing: &str, new_memories: Vec<String>) -> String {
        let mut lines: Vec<String> = existing.lines().map(|l| l.to_string()).collect();

        // Find insertion point — after "## 偏好" section or at end
        let mut insert_idx = lines.len();
        for (i, line) in lines.iter().enumerate() {
            if line.starts_with("## 偏好") || line.starts_with("## Preferences") {
                insert_idx = i + 1;
                break;
            }
        }

        // Add new memories, avoiding duplicates
        for memory in new_memories {
            let trimmed = memory.trim();
            if trimmed.is_empty() {
                continue;
            }
            let formatted = format!("- {}", trimmed);
            // Deduplicate
            if !lines.contains(&formatted) {
                lines.insert(insert_idx, formatted);
                insert_idx += 1;
            }
        }

        lines.join("\n")
    }
}

impl Clone for MemoryKeeper {
    fn clone(&self) -> Self {
        Self {
            workspace_dir: self.workspace_dir.clone(),
            side_query: self.side_query.clone(),
            buffer: self.buffer.clone(),
        }
    }
}
