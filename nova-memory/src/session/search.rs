use anyhow::Result;
use tracing::{info, warn};

use nova_core::message::{Message, Role};
use crate::session::manager::SessionManager;
use crate::sidequery::SideQuery;

/// Max chars of transcript per session sent to the LLM
const MAX_TRANSCRIPT_CHARS: usize = 1000;
/// Max messages to scan from start/end of a session
const MAX_MESSAGES_TO_SCAN: usize = 200;
/// Max sessions to send to the LLM for ranking
const MAX_SESSIONS_TO_SEARCH: usize = 50;

const SESSION_SEARCH_SYSTEM_PROMPT: &str = r#"Your goal is to find relevant sessions based on a user's search query.
You will be given a list of sessions with their metadata and a search query. Identify which sessions are most relevant.

Each session includes:
- Title (first user message)
- Summary (conversation excerpt)
- Turn count and message count

For each session, consider:
1. Title/first message matches (highest priority)
2. Summary content matches
3. Semantic similarity and related concepts

Be VERY inclusive. Include sessions that:
- Contain the query term anywhere
- Are semantically related (e.g. "testing" matches "unit tests", "QA")
- Discuss topics related to the query even in passing

Return sessions ordered by relevance (most relevant first).
If no sessions match, return an empty array.

Respond with ONLY valid JSON, no markdown:
{"relevant_indices": [2, 5, 0]}"#;

/// Metadata about a session for search purposes
#[derive(Debug, Clone)]
pub struct SessionSearchEntry {
    pub session_id: String,
    pub first_message: String,
    pub transcript: String,
    pub turn_count: usize,
    pub message_count: usize,
}

/// Extract searchable text from messages
fn extract_transcript(messages: &[Message]) -> String {
    if messages.is_empty() {
        return String::new();
    }

    // Take messages from start and end for context
    let to_scan: Vec<&Message> = if messages.len() <= MAX_MESSAGES_TO_SCAN {
        messages.iter().collect()
    } else {
        let half = MAX_MESSAGES_TO_SCAN / 2;
        messages.iter().take(half)
            .chain(messages.iter().rev().take(half))
            .collect()
    };

    let text: String = to_scan.iter()
        .filter(|m| m.role == Role::User || m.role == Role::Assistant)
        .filter_map(|m| m.content.as_deref())
        .collect::<Vec<_>>()
        .join(" ");

    // Collapse whitespace
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");

    // Truncate by chars (safe for UTF-8)
    if text.chars().count() > MAX_TRANSCRIPT_CHARS {
        let truncated: String = text.chars().take(MAX_TRANSCRIPT_CHARS).collect();
        format!("{}…", truncated)
    } else {
        text
    }
}

/// Extract the first user message as the "title"
fn extract_first_message(messages: &[Message]) -> String {
    messages.iter()
        .find(|m| m.role == Role::User)
        .and_then(|m| m.content.as_deref())
        .map(|s| {
            let chars: String = s.chars().take(200).collect();
            chars
        })
        .unwrap_or_default()
}

/// Check if a session entry contains the query (case-insensitive)
fn entry_contains_query(entry: &SessionSearchEntry, query_lower: &str) -> bool {
    entry.first_message.to_lowercase().contains(query_lower)
        || entry.transcript.to_lowercase().contains(query_lower)
}

/// Agentic Session Search — uses SideQuery + LLM to semantically search sessions.
pub struct AgenticSessionSearch {
    side_query: SideQuery,
    session_mgr: SessionManager,
}

impl AgenticSessionSearch {
    pub fn new(side_query: SideQuery, session_mgr: SessionManager) -> Self {
        Self { side_query, session_mgr }
    }

    /// Load all session metadata + transcripts for search
    fn load_search_entries(&self) -> Result<Vec<SessionSearchEntry>> {
        let all_metas = self.session_mgr.list_all_metas()?;
        let mut entries = Vec::new();

        for meta in all_metas {
            let history = self.session_mgr.history_for_id(&meta.session_id);
            let messages = history.load_all().unwrap_or_default();

            entries.push(SessionSearchEntry {
                session_id: meta.session_id.clone(),
                first_message: extract_first_message(&messages),
                transcript: extract_transcript(&messages),
                turn_count: meta.turn_count,
                message_count: messages.len(),
            });
        }

        Ok(entries)
    }

    /// Perform agentic search: pre-filter + LLM ranking
    pub async fn search(&self, query: &str) -> Result<Vec<SessionSearchEntry>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let all_entries = self.load_search_entries()?;
        if all_entries.is_empty() {
            return Ok(Vec::new());
        }

        let query_lower = query.to_lowercase();

        // Pre-filter: find entries that contain the query term
        let matching: Vec<&SessionSearchEntry> = all_entries.iter()
            .filter(|e| entry_contains_query(e, &query_lower))
            .collect();

        // Build the list to send to LLM: matching first, then recent non-matching
        let entries_to_search: Vec<&SessionSearchEntry> = if matching.len() >= MAX_SESSIONS_TO_SEARCH {
            matching.into_iter().take(MAX_SESSIONS_TO_SEARCH).collect()
        } else {
            let non_matching: Vec<&SessionSearchEntry> = all_entries.iter()
                .filter(|e| !entry_contains_query(e, &query_lower))
                .collect();
            let remaining = MAX_SESSIONS_TO_SEARCH.saturating_sub(matching.len());
            matching.into_iter()
                .chain(non_matching.into_iter().take(remaining))
                .collect()
        };

        info!("Agentic search: {}/{} sessions, query=\"{}\"", entries_to_search.len(), all_entries.len(), query);

        // Build prompt for LLM
        let session_list: String = entries_to_search.iter().enumerate()
            .map(|(i, e)| {
                let mut parts = vec![format!("{}:", i)];
                if !e.first_message.is_empty() {
                    parts.push(format!("Title: {}", e.first_message));
                }
                parts.push(format!("({} messages, {} turns)", e.message_count, e.turn_count));
                if !e.transcript.is_empty() {
                    parts.push(format!("Transcript: {}", e.transcript));
                }
                parts.join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n");

        let user_message = format!(
            "Sessions:\n{}\n\nSearch query: \"{}\"\n\nFind the sessions that are most relevant to this query.",
            session_list, query
        );

        // Call LLM via SideQuery
        let response = self.side_query.query_await(SESSION_SEARCH_SYSTEM_PROMPT, &user_message).await?;

        // Parse JSON response
        let indices = parse_relevant_indices(&response);

        info!("Agentic search found {} relevant sessions", indices.len());

        // Map indices back to entries
        let results: Vec<SessionSearchEntry> = indices.into_iter()
            .filter(|&i| i < entries_to_search.len())
            .map(|i| entries_to_search[i].clone())
            .collect();

        Ok(results)
    }
}

/// Parse the LLM response to extract relevant_indices
fn parse_relevant_indices(response: &str) -> Vec<usize> {
    // Try to find JSON in the response
    let json_start = response.find('{');
    let json_end = response.rfind('}');

    if let (Some(start), Some(end)) = (json_start, json_end) {
        if end > start {
            let json_str = &response[start..=end];
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
                if let Some(arr) = val.get("relevant_indices").and_then(|v| v.as_array()) {
                    return arr.iter()
                        .filter_map(|v| v.as_u64().map(|n| n as usize))
                        .collect();
                }
            }
        }
    }

    warn!("Failed to parse agentic search response: {}", &response[..response.len().min(200)]);
    Vec::new()
}
