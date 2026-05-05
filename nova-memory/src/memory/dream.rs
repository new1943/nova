//! Dream — Memory consolidation engine.
//!
//! Periodically reviews daily diaries and updates MEMORY.md with key insights.
//! Triggered: >=24h since last dream AND >=5 new sessions, or `/dream` manual.
//!
//! 4-phase process (inspired by Claude Code autoDream):
//!   1. Orient   — read MEMORY.md + list diaries
//!   2. Gather   — read recent diary entries
//!   3. Consolidate — distill key items, update MEMORY.md
//!   4. Prune    — keep MEMORY.md under 200 lines

use anyhow::Result;
use chrono::Utc;
use std::fs;
use std::path::PathBuf;
use tracing::debug;

use crate::sidequery::SideQuery;

/// Dream engine — memory consolidation via background agent.
pub struct DreamEngine {
    workspace_dir: PathBuf,
    #[allow(dead_code)]
    memories_dir: PathBuf,
    side_query: SideQuery,
}

impl DreamEngine {
    pub fn new(workspace_dir: PathBuf, memories_dir: PathBuf, side_query: SideQuery) -> Self {
        Self {
            workspace_dir,
            memories_dir,
            side_query,
        }
    }

    /// Check if dream should run:
    /// - Lock file missing or stale (>24h) AND >=5 new sessions since last dream
    pub fn should_dream(&self) -> bool {
        let lock_path = self.workspace_dir.join(".dream-lock");
        let sessions_dir = self.workspace_dir.join("sessions");

        // Check session count
        let session_count = fs::read_dir(&sessions_dir)
            .map(|e| e.filter_map(|d| d.ok()).count())
            .unwrap_or(0);

        debug!("Dream check: session_count={}, need={}", session_count, 5);

        if session_count < 5 {
            debug!("Dream skip: not enough sessions ({} < 5)", session_count);
            return false;
        }

        // Check lock file
        let lock_age_hours = fs::metadata(&lock_path).ok().and_then(|meta| {
            meta.modified().ok().map(|mtime| {
                Utc::now().signed_duration_since(chrono::DateTime::<Utc>::from(mtime)).num_hours()
            })
        }).unwrap_or(-1);

        debug!("Dream check: lock_age_hours={}, trigger={}", lock_age_hours, session_count >= 5);

        // If lock is fresh (<24h), skip
        if (0..24).contains(&lock_age_hours) {
            debug!("Dream skip: lock is fresh ({} hours < 24)", lock_age_hours);
            return false;
        }

        true
    }

    /// Try to acquire the dream lock (PID file).
    /// Returns false if another dream is already running.
    pub fn acquire_lock(&self) -> bool {
        let lock_path = self.workspace_dir.join(".dream-lock");
        let pid = std::process::id().to_string();

        // Check if lock is stale (process dead)
        if let Ok(existing) = fs::read_to_string(&lock_path) {
            if let Ok(pid) = existing.trim().parse::<u32>() {
                // Is process still alive?
                let alive = std::process::Command::new("kill")
                    .args(["-0", &pid.to_string()])
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                if alive {
                    return false; // Lock held by live process
                }
            }
        }

        // Write our PID
        fs::write(&lock_path, pid).is_ok()
    }

    /// Release the dream lock.
    pub fn release_lock(&self) {
        let lock_path = self.workspace_dir.join(".dream-lock");
        let _ = fs::remove_file(lock_path);
    }

    /// Run the 4-phase dream consolidation.
    /// Called by a forked agent in the background.
    /// `turn_start_mtime`: mtime of MEMORY.md at the start of current turn (from session.token_stats.memory_mtime).
    /// If Some and MEMORY.md has been modified since, LLM wrote to it this turn → skip MEMORY.md update.
    pub async fn dream(&self, turn_start_mtime: Option<std::time::SystemTime>) -> Result<()> {
        if !self.acquire_lock() {
            tracing::info!("Dream: lock not acquired, skipping");
            return Ok(());
        }

        // T21.4: Check if LLM modified MEMORY.md during this turn
        if let Some(turn_start) = turn_start_mtime {
            if let Ok(current_meta) = fs::metadata(self.workspace_dir.join("MEMORY.md")) {
                if let Ok(current_mtime) = current_meta.modified() {
                    if current_mtime > turn_start {
                        tracing::info!("Dream: MEMORY.md was modified by LLM this turn, skipping consolidation");
                        self.release_lock();
                        return Ok(());
                    }
                }
            }
        }

        let result = self.do_dream().await;
        self.release_lock();
        result
    }

    async fn do_dream(&self) -> Result<()> {
        tracing::info!("Dream: starting 4-phase consolidation");

        // Phase 1: Orient — read MEMORY.md + list diaries
        let memory_path = self.workspace_dir.join("MEMORY.md");
        let current_memory = fs::read_to_string(&memory_path)
            .unwrap_or_else(|_| String::new());

        let diary_list = self.list_recent_diaries(7); // last 7 days
        tracing::info!("Dream: found {} recent diary files", diary_list.len());

        // Phase 2: Gather — read diary contents
        let diary_contents: Vec<String> = diary_list.iter()
            .map(|(_, path)| {
                fs::read_to_string(path).unwrap_or_default()
            })
            .collect();

        let diary_text = diary_contents.join("\n\n---\n\n");
        if diary_text.trim().is_empty() {
            tracing::info!("Dream: no diary content to process, skipping");
            return Ok(());
        }

        // Phase 3: Consolidate — use SideQuery to distill updates to MEMORY.md
        let system = "You are a memory consolidator. Given the current MEMORY.md and recent diary entries, \
produce an updated MEMORY.md that captures the most important items.\n\n\
Rules:\n- Keep under 200 lines total\n- Maintain 4 sections: ## 用户 / ## 项目 / ## 反馈 / ## 参考\n- Use concise bullet points\n- Remove items that are no longer relevant\n- Add new items from the diary\n- Convert relative dates to absolute dates\n\n\
Output only the complete updated MEMORY.md content, no explanation.";

        let prompt = format!(
            "Current MEMORY.md:\n{}\n\nRecent diary entries:\n{}\n\nUpdated MEMORY.md:",
            current_memory, diary_text
        );

        debug!("Dream: phase 3 consolidate, memory_len={}, diary_len={}", current_memory.len(), diary_text.len());
        let updated_memory = self.side_query.query_await(system, &prompt).await?;
        debug!("Dream: phase 3 complete, updated_memory_len={}", updated_memory.len());

        // Write updated MEMORY.md
        fs::write(&memory_path, &updated_memory)?;
        tracing::info!("Dream: MEMORY.md updated (~{} chars)", updated_memory.len());

        // Phase 4: Prune — trim diary entries if too many (keep last 30 days)
        self.prune_old_diaries(30)?;

        tracing::info!("Dream: consolidation complete");
        Ok(())
    }

    /// List recent diary files (date, path), newest first.
    fn list_recent_diaries(&self, days: i64) -> Vec<(String, PathBuf)> {
        let memories_dir = self.workspace_dir.join("memories");
        let mut diaries = Vec::new();
        let cutoff = (Utc::now() - chrono::Duration::days(days))
            .format("%Y-%m-%d")
            .to_string();

        let entries = match fs::read_dir(memories_dir) {
            Ok(e) => e,
            Err(_) => return diaries,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let filename = path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let date = filename.trim_end_matches(".md").to_string();
            if date >= cutoff {
                diaries.push((date, path));
            }
        }

        diaries.sort_by(|a, b| b.0.cmp(&a.0));
        diaries
    }

    /// Delete diary files older than `days`.
    fn prune_old_diaries(&self, days: i64) -> Result<()> {
        let memories_dir = self.workspace_dir.join("memories");
        let cutoff = (Utc::now() - chrono::Duration::days(days))
            .format("%Y-%m-%d")
            .to_string();

        let entries = match fs::read_dir(&memories_dir) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };

        let mut removed = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            let filename = path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let date = filename.trim_end_matches(".md").to_string();
            if date < cutoff && date.len() == 10
                && fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
        }

        if removed > 0 {
            tracing::info!("Dream: pruned {} old diary files", removed);
        }
        Ok(())
    }
}
