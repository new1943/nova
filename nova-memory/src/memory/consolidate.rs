//! T23: Memory Consolidator — idle-time dual-write mutex engine for Layer 1 (MEMORY.md).
//!
//! Two trigger points:
//!   1. **Idle scan** — daemon detects >15min inactivity with unswept messages
//!   2. **Compact intercept** — `loop.rs` fires before context compression
//!
//! Pipeline:
//!   - If `memory_updated_mutex == true` → skip (LLM already wrote MEMORY.md)
//!   - Else → SideQuery with strict filter prompt → patch MEMORY.md if needed
//!   - Always advance `last_memory_sweep_index` afterwards

use anyhow::Result;
use std::fs;
use std::path::PathBuf;
use tracing::{info, warn};

use nova_core::atomic_write::atomic_write;
use nova_core::message::{Message, Role};
use crate::sidequery::SideQuery;

/// Memory consolidator — extracts high-value updates from recent conversation
/// into MEMORY.md, guarded by dual-write mutex.
#[derive(Clone)]
pub struct MemoryConsolidator {
    workspace_dir: PathBuf,
    side_query: SideQuery,
}

impl MemoryConsolidator {
    pub fn new(workspace_dir: PathBuf, side_query: SideQuery) -> Self {
        Self { workspace_dir, side_query }
    }

    /// Run the consolidation pipeline.
    ///
    /// Returns `true` if MEMORY.md was actually updated, `false` if skipped.
    ///
    /// # Arguments
    /// * `messages` — full session messages
    /// * `sweep_index` — index of last swept message (start scanning from here)
    /// * `mutex` — whether LLM already wrote MEMORY.md since last sweep
    pub async fn consolidate(
        &self,
        messages: &[Message],
        sweep_index: usize,
        mutex: bool,
    ) -> Result<bool> {
        // Nothing new to process
        if sweep_index >= messages.len() {
            return Ok(false);
        }

        let new_messages = &messages[sweep_index..];
        if new_messages.len() < 3 {
            return Ok(false); // Too few messages to bother
        }

        // Mutex check: LLM already wrote MEMORY.md this segment
        if mutex {
            info!("T23: Memory mutex is set (LLM wrote MEMORY.md), skipping consolidation");
            return Ok(false);
        }

        info!(
            "T23: Memory mutex not set, running consolidation on {} new messages (from index {})",
            new_messages.len(), sweep_index
        );

        // Collect conversation excerpt for the SideQuery
        let excerpt = Self::build_excerpt(new_messages);
        if excerpt.trim().len() < 20 {
            return Ok(false);
        }

        // Read current MEMORY.md
        let memory_path = self.workspace_dir.join("MEMORY.md");
        let current_memory = fs::read_to_string(&memory_path).unwrap_or_default();

        // SideQuery with extreme filter prompt
        let system = CONSOLIDATION_PROMPT;
        let prompt = format!(
            "<MEMORY_CONTENT>\n{}\n</MEMORY_CONTENT>\n\n<RECENT_CONVERSATION>\n{}\n</RECENT_CONVERSATION>",
            current_memory, excerpt
        );

        match self.side_query.query_await(system, &prompt).await {
            Ok(response) => {
                let response = response.trim();
                if response == "NO_UPDATE_NEEDED" || response.is_empty() {
                    info!("T23: Consolidation determined no update needed");
                    return Ok(false);
                }

                // Apply the update — SideQuery returns the full updated MEMORY.md
                atomic_write(&memory_path, response.as_bytes())?;
                info!("T23: MEMORY.md updated by consolidation (~{} chars)", response.len());
                Ok(true)
            }
            Err(e) => {
                warn!("T23: Consolidation SideQuery failed: {}", e);
                Ok(false)
            }
        }
    }

    /// Build a conversation excerpt from messages (user + assistant content only).
    fn build_excerpt(messages: &[Message]) -> String {
        let mut lines = Vec::new();
        for m in messages {
            match m.role {
                Role::User => {
                    if let Some(c) = &m.content {
                        let c = c.trim();
                        if !c.is_empty() {
                            lines.push(format!("User: {}", c));
                        }
                    }
                }
                Role::Assistant => {
                    if let Some(c) = &m.content {
                        let c = c.trim();
                        if !c.is_empty() {
                            // Truncate long assistant responses to save tokens
                            let truncated: String = c.chars().take(500).collect();
                            lines.push(format!("Assistant: {}", truncated));
                        }
                    }
                }
                _ => {}
            }
        }
        // Cap total excerpt at ~8K chars
        let joined = lines.join("\n");
        joined.chars().take(8000).collect()
    }
}

/// Extreme filter prompt — only extracts persistent user preferences and
/// milestone-level project state changes. Everything else is rejected.
const CONSOLIDATION_PROMPT: &str = r#"你是一个极简主义的记忆提纯引擎（Memory Consolidator）。
请阅读当前用户的 <MEMORY_CONTENT> 和一段未经整理的 <RECENT_CONVERSATION>。

【你的唯一任务】
只提取符合以下两条极其严格标准的信息，并在必要时产出更新后的完整 MEMORY.md：
1. 全局偏好与纠正 (User Rules)：如"不要用tailwind"、"后续所有服务使用 Rust 1.75"、"倾向于函数式编程"。
2. 系统状态的跃迁 (Project Shifts)：如某个长达多日的疑难杂症彻底解决，或架构层面发生了永久性迁移。

【禁忌】
绝对不要摘录日常调试日志、单次任务的切片过程、短期 Todos、或寒暄！

【输出格式】
- 如果增量内容中绝无符合上述2点标准的高价值信息，请必须且只能输出严格的字符串："NO_UPDATE_NEEDED"
- 如果存在必须补充的内容，输出更新后的完整 MEMORY.md（保持原有结构：## 用户 / ## 项目 / ## 反馈 / ## 参考，总量 < 200 行）"#;
