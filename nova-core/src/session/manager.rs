use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

use crate::message::Message;
use crate::session::history::SessionHistory;

/// Token statistics
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TokenStats {
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
    pub turn_tokens: Vec<u32>,
    /// T21.4: mtime of MEMORY.md at the start of current turn (for dual-write mutex with Dream)
    pub memory_mtime: Option<std::time::SystemTime>,
}

/// Session state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub session_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub messages: Vec<Message>,
    pub turn_count: usize,
    pub max_turns: usize,
    pub token_stats: TokenStats,
    /// T23: Index of last message processed by memory consolidation sweep.
    #[serde(default)]
    pub last_memory_sweep_index: usize,
    /// T23: True if LLM actively wrote MEMORY.md since last sweep.
    #[serde(default)]
    pub memory_updated_mutex: bool,
}

impl Session {
    pub fn new(max_turns: usize) -> Self {
        Self {
            session_id: Uuid::new_v4().to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            messages: Vec::new(),
            turn_count: 0,
            max_turns,
            token_stats: TokenStats::default(),
            last_memory_sweep_index: 0,
            memory_updated_mutex: false,
        }
    }

    pub fn add_message(&mut self, msg: Message) {
        self.messages.push(msg);
        self.updated_at = Utc::now();
    }

    pub fn increment_turn(&mut self) -> bool {
        self.turn_count += 1;
        self.turn_count <= self.max_turns
    }

    /// Reset turn counter for a new user query.
    /// max_turns limits the agent loop depth per user message, not per session.
    pub fn reset_turns(&mut self) {
        self.turn_count = 0;
    }
}

/// Session metadata (stored separately from JSONL history)
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub session_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub turn_count: usize,
    pub token_stats: TokenStats,
    /// T23: Memory sweep cursor
    #[serde(default)]
    pub last_memory_sweep_index: usize,
    /// T23: Memory dual-write mutex
    #[serde(default)]
    pub memory_updated_mutex: bool,
}

impl From<&Session> for SessionMeta {
    fn from(s: &Session) -> Self {
        Self {
            session_id: s.session_id.clone(),
            created_at: s.created_at,
            updated_at: s.updated_at,
            turn_count: s.turn_count,
            token_stats: s.token_stats.clone(),
            last_memory_sweep_index: s.last_memory_sweep_index,
            memory_updated_mutex: s.memory_updated_mutex,
        }
    }
}

/// Session manager: create, resume, save, persist
#[derive(Clone)]
pub struct SessionManager {
    sessions_dir: PathBuf,
}

impl SessionManager {
    pub fn new(sessions_dir: PathBuf) -> Self {
        Self { sessions_dir }
    }

    pub fn create(&self, max_turns: usize) -> Result<Session> {
        std::fs::create_dir_all(&self.sessions_dir)?;
        Ok(Session::new(max_turns))
    }

    /// Get the JSONL history writer for a session
    pub fn history_for(&self, session: &Session) -> SessionHistory {
        let path = self.sessions_dir.join(format!("{}.jsonl", session.session_id));
        SessionHistory::new(path)
    }

    /// Get the JSONL history by session ID
    pub fn history_for_id(&self, session_id: &str) -> SessionHistory {
        let path = self.sessions_dir.join(format!("{}.jsonl", session_id));
        SessionHistory::new(path)
    }

    /// Append a message to both session and JSONL file
    pub fn append_message(&self, session: &mut Session, msg: Message) -> Result<()> {
        let history = self.history_for(session);
        history.append(&msg)?;
        session.add_message(msg);
        Ok(())
    }

    /// Clear a session's messages and reset its stats, useful for Discord where session ID is tied to channel
    pub fn clear_session(&self, session: &mut Session) -> Result<()> {
        let history = self.history_for(session);
        history.clear()?;
        
        session.messages.clear();
        session.turn_count = 0;
        session.token_stats = TokenStats::default();
        session.last_memory_sweep_index = 0;
        session.memory_updated_mutex = false;
        
        self.save_meta(session)?;
        Ok(())
    }


    /// Find and resume a specific session by ID
    pub fn resume_by_id(&self, session_id: &str) -> Result<Option<Session>> {
        let meta_path = self.sessions_dir.join(format!("{}.meta.json", session_id));
        if !meta_path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&meta_path)?;
        let meta: SessionMeta = serde_json::from_str(&content)?;
        
        let history_path = self.sessions_dir.join(format!("{}.jsonl", session_id));
        let history = SessionHistory::new(history_path);
        let messages = history.load_all()?;
        Ok(Some(Session {
            session_id: meta.session_id,
            created_at: meta.created_at,
            updated_at: meta.updated_at,
            messages,
            turn_count: meta.turn_count,
            max_turns: 20,
            token_stats: meta.token_stats,
            last_memory_sweep_index: meta.last_memory_sweep_index,
            memory_updated_mutex: meta.memory_updated_mutex,
        }))
    }

    /// Find and resume the most recently updated session
    pub fn resume_latest(&self) -> Result<Option<Session>> {
        if !self.sessions_dir.exists() {
            return Ok(None);
        }

        let mut latest: Option<SessionMeta> = None;

        for entry in std::fs::read_dir(&self.sessions_dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".meta.json") {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                if let Ok(meta) = serde_json::from_str::<SessionMeta>(&content) {
                    if latest.as_ref().is_none_or(|m| meta.updated_at > m.updated_at) {
                        latest = Some(meta);
                    }
                }
            }
        }

        match latest {
            Some(meta) => {
                let history_path = self.sessions_dir.join(format!("{}.jsonl", meta.session_id));
                let history = SessionHistory::new(history_path);
                let messages = history.load_all()?;
                Ok(Some(Session {
                    session_id: meta.session_id,
                    created_at: meta.created_at,
                    updated_at: meta.updated_at,
                    messages,
                    turn_count: meta.turn_count,
                    max_turns: 20,
                    token_stats: meta.token_stats,
                    last_memory_sweep_index: meta.last_memory_sweep_index,
                    memory_updated_mutex: meta.memory_updated_mutex,
                }))
            }
            None => Ok(None),
        }
    }

    /// Save session metadata to .meta.json
    pub fn save_meta(&self, session: &Session) -> Result<()> {
        std::fs::create_dir_all(&self.sessions_dir)?;
        let meta = SessionMeta::from(session);
        let path = self.sessions_dir.join(format!("{}.meta.json", session.session_id));
        std::fs::write(path, serde_json::to_string_pretty(&meta)?)?;
        Ok(())
    }

    /// List all session metadata files, sorted by updated_at descending
    pub fn list_all_metas(&self) -> Result<Vec<SessionMeta>> {
        if !self.sessions_dir.exists() {
            return Ok(Vec::new());
        }
        let mut metas = Vec::new();
        for entry in std::fs::read_dir(&self.sessions_dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".meta.json") {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                if let Ok(meta) = serde_json::from_str::<SessionMeta>(&content) {
                    metas.push(meta);
                }
            }
        }
        metas.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(metas)
    }
}
