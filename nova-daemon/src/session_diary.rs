//! Session diary — MapReduce-based session summarization and diary writing.

use std::path::Path;
use tracing::{info, warn};

use nova_core::memory::daily::DailyNotes;
use nova_core::sidequery::SideQuery;

const MAP_CHUNK_SIZE: usize = 180_000; // ~180K chars per Map batch

/// Record MEMORY.md mtime at the start of each turn.
/// Used by Dream to detect if LLM modified MEMORY.md during this turn.
pub fn record_memory_mtime(session: &mut nova_core::session::manager::Session, workspace_dir: &Path) {
    let memory_path = workspace_dir.join("MEMORY.md");
    if let Ok(meta) = std::fs::metadata(&memory_path) {
        if let Ok(mtime) = meta.modified() {
            session.token_stats.memory_mtime = Some(mtime);
        }
    }
}

/// Preprocess session: keep user original + assistant decisions, strip tool details.
fn preprocess_session(messages: &[nova_core::message::Message]) -> String {
    let mut lines = Vec::new();
    for m in messages {
        match m.role {
            nova_core::message::Role::User => {
                if let Some(c) = &m.content {
                    let c = c.trim();
                    if !c.is_empty() && !c.contains("<system_notification>") {
                        lines.push(format!("User: {}", c));
                    }
                }
            }
            nova_core::message::Role::Assistant => {
                if let Some(c) = &m.content {
                    let c = c.trim();
                    if !c.is_empty() {
                        lines.push(format!("Assistant: {}", c));
                    }
                }
            }
            _ => {}
        }
    }
    lines.join("\n")
}

/// Map phase: extract key points from one chunk by dimension.
async fn map_chunk(sq: &SideQuery, chunk: &str) -> anyhow::Result<String> {
    let system = "You are a key-point extractor. Given a conversation chunk, \
extract important information along these dimensions:
- 事件 (events that happened)
- 反馈 (user feedback / opinions)
- 用户偏好 (user preferences)
- 项目状态 (project status / decisions)
- 重要决策 (key decisions made)
- 参考资料 (references, links, configurations)

Output a concise list of key points, one per line, in Chinese. \
If a dimension has no information, skip it. Do not add explanatory text.";

    sq.query_await(system, chunk).await
}

/// Reduce phase: combine all map results into ~200 char final summary.
async fn reduce_summaries(sq: &SideQuery, map_results: &[String]) -> anyhow::Result<String> {
    let combined = map_results.join("\n\n");
    let system = "你是一名会话记录员。根据以下会话要点，写一段 100-200 字的中文总结，要有头有尾，连贯自然。

重点记录：
- 发生了什么（事件）
- 用户说了什么、反馈如何
- 项目进展或重要决策
- 用户的偏好或习惯

要求：
- 用完整的句子叙述，不是罗列要点
- 一口气说完，不要分段
- 100-200 字为宜
- 只输出中文总结，不加标签不加格式";

    sq.query_await(system, &combined).await
}

/// Write a MapReduce-summarized session diary entry.
pub async fn write_session_diary(
    daily: &DailyNotes,
    sq: &SideQuery,
    session: &nova_core::session::manager::Session,
) -> anyhow::Result<()> {
    // Step 1: preprocess — keep user + assistant decisions, strip tool calls
    let preprocessed = preprocess_session(&session.messages);
    if preprocessed.len() < 10 {
        return Ok(());
    }

    // Step 2: map phase — split into ~180K chunks
    let mut map_results = Vec::new();
    for chunk in preprocessed.chars().collect::<Vec<_>>().chunks(MAP_CHUNK_SIZE) {
        let chunk_str: String = chunk.iter().collect();
        match map_chunk(sq, &chunk_str).await {
            Ok(result) if !result.trim().is_empty() => {
                map_results.push(result);
            }
            Ok(_) => {}
            Err(e) => {
                warn!("Map chunk failed: {}", e);
            }
        }
    }

    if map_results.is_empty() {
        return Ok(());
    }

    // Step 3: reduce phase — combine all map results into final ~200 char summary
    let final_summary = reduce_summaries(sq, &map_results).await?;
    if final_summary.trim().is_empty() {
        return Ok(());
    }

    // Step 4: write to diary
    daily.append_session(&final_summary)?;
    info!("Session diary written: {} chars ({} map chunks)", final_summary.len(), map_results.len());
    Ok(())
}
