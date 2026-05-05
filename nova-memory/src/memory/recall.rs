//! Memory Recall — Layer 2 episodic memory recall pipeline.
//!
//! Scans `~/.nova/memories/YYYY-MM-DD.md` diary files,
//! uses SideQuery to select the most relevant ones,
//! and formats them for injection into system prompt.

use anyhow::Result;
use std::path::PathBuf;

use crate::sidequery::SideQuery;
use crate::session::manager::SessionManager;

/// Metadata for a diary file (extracted without reading full content)
#[derive(Debug, Clone)]
pub struct DiaryMeta {
    /// Full date string, e.g. "2026-04-14"
    pub date: String,
    /// File path
    pub path: PathBuf,
    /// First meaningful line (section title or first entry)
    pub title: String,
    /// File modification time (for sorting)
    pub mtime: std::time::SystemTime,
}

/// Memory recall — scans diaries and injects relevant ones into system prompt.
pub struct MemoryRecall {
    memories_dir: PathBuf,
    side_query: SideQuery,
    #[allow(dead_code)]
    session_manager: SessionManager,
}

impl MemoryRecall {
    pub fn new(memories_dir: PathBuf, side_query: SideQuery, session_manager: SessionManager) -> Self {
        Self {
            memories_dir,
            side_query,
            session_manager,
        }
    }

    /// Scan `memories/` directory, extract date + first line from each .md file.
    pub fn scan_diaries(&self) -> Vec<DiaryMeta> {
        let mut diaries = Vec::new();

        let entries = std::fs::read_dir(&self.memories_dir).ok();
        let entries = match entries {
            Some(e) => e,
            None => return diaries,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }

            // Extract date from filename: YYYY-MM-DD.md
            let filename = path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let date = filename.trim_end_matches(".md").to_string();

            // Only include valid date filenames
            if date.len() != 10 || !date.starts_with(|c: char| c.is_ascii_digit()) {
                continue;
            }

            // Read first non-empty line as title
            let title = Self::extract_first_title(&path);
            let mtime = entry.metadata().ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

            diaries.push(DiaryMeta {
                date,
                path,
                title,
                mtime,
            });
        }

        // Sort by date descending (newest first)
        diaries.sort_by(|a, b| b.date.cmp(&a.date));
        diaries
    }

    /// Read the first non-empty, non-heading line from a diary file.
    fn extract_first_title(path: &PathBuf) -> String {
        if let Ok(content) = std::fs::read_to_string(path) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                // Return first meaningful content line (truncated)
                return if trimmed.len() > 80 {
                    format!("{}...", &trimmed[..80])
                } else {
                    trimmed.to_string()
                };
            }
        }
        String::new()
    }

    /// Use SideQuery to select the most relevant diaries for the given query.
    /// Returns at most `max_results` diaries.
    pub async fn find_relevant(&self, query: &str, max_results: usize) -> Result<Vec<DiaryMeta>> {
        let diaries = self.scan_diaries();
        if diaries.is_empty() {
            return Ok(Vec::new());
        }

        // Build prompt for LLM to select relevant diaries
        let diary_list: String = diaries.iter()
            .enumerate()
            .map(|(i, d)| {
                format!("[{}] {} — {}", i, d.date, d.title)
            })
            .collect::<Vec<_>>()
            .join("\n");

        let system = "You are a memory selector. Given a user query and a list of diary entries, \
select the most relevant ones. Return a JSON array of indices.\n\
Only select diaries that are truly relevant. Return empty array if none match.\n\
Format: [1, 3, 5] (only the indices, no other text).";

        let prompt = format!(
            "User query: {}\n\nAvailable diaries:\n{}\n\nRelevant indices (JSON array):",
            query, diary_list
        );

        let response = self.side_query.query_await(system, &prompt).await?;
        let response = response.trim();

        // Parse JSON array from response
        let indices: Vec<usize> = serde_json::from_str(response)
            .unwrap_or_else(|_| {
                // Fallback: try to extract numbers from response like "[1, 3, 5]"
                let nums: Vec<usize> = response
                    .chars()
                    .filter(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .split(|c: char| !c.is_ascii_digit())
                    .filter_map(|s| s.parse().ok())
                    .collect();
                nums
            });

        let result = indices.into_iter()
            .filter_map(|i| diaries.get(i).cloned())
            .take(max_results)
            .collect();

        Ok(result)
    }

    /// Read the content of selected diaries and format for system prompt injection.
    pub fn format_injection(&self, diaries: &[DiaryMeta]) -> String {
        if diaries.is_empty() {
            return String::new();
        }

        let mut sections = Vec::new();
        sections.push("<relevant_memories>".to_string());

        for diary in diaries {
            let content = std::fs::read_to_string(&diary.path)
                .unwrap_or_default();
            let excerpt = if content.chars().count() > 2000 {
                content.chars().take(2000).collect::<String>() + "\n... (truncated)"
            } else {
                content
            };

            sections.push(format!("\n### {} — Daily Diary\n\n{}", diary.date, excerpt));
        }

        sections.push("</relevant_memories>".to_string());
        sections.join("\n")
    }

    /// Full pipeline: scan → select relevant → format for injection.
    /// Returns the formatted injection string ready to append to system prompt.
    pub async fn recall(&self, query: &str, max_diaries: usize) -> Result<String> {
        let relevant = self.find_relevant(query, max_diaries).await?;
        Ok(self.format_injection(&relevant))
    }
}
