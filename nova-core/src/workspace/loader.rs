use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

/// Bootstrap file loading constants (aligned with OpenClaw)
const MAX_PER_FILE_CHARS: usize = 20_000;
const MAX_TOTAL_CHARS: usize = 150_000;
const MIN_FILE_BUDGET: usize = 64;
const HEAD_RATIO: f64 = 0.7;
const TAIL_RATIO: f64 = 0.2;

/// Bootstrap files to load, in injection order.
/// MEMORY.md is excluded — accessed via tool search, not injected.
const BOOTSTRAP_FILES: &[&str] = &[
    "SOUL.md",
    "IDENTITY.md",
    "AGENTS.md",
    "USER.md",
    "STATE.md",
    "TASKS.md",
];

/// Cached file entry with mtime for change detection
#[derive(Debug, Clone)]
struct CachedFile {
    content: String,
    mtime: SystemTime,
}

/// Loaded workspace files (legacy, kept for backward compat)
#[derive(Debug, Default)]
pub struct Workspace {
    pub soul: String,
    pub identity: String,
    pub user: String,
    pub agents: String,
    pub memory: String,
    pub state: String,
    pub tools: String,
    pub tasks: String,
    pub heartbeat: String,
}

/// BootstrapLoader — hot-reloads workspace .md files with mtime caching.
///
/// On each call to `build_system_prompt()`:
/// - Checks file mtime; only re-reads from disk if changed
/// - Injects files in order: SOUL → IDENTITY → AGENTS → USER → STATE → TASKS
/// - Truncates per OpenClaw rules (single file 20K, total 150K)
/// - MEMORY.md is NOT injected (accessed via tool search)
/// - Appends tool descriptions at the end
pub struct BootstrapLoader {
    root: PathBuf,
    cache: HashMap<String, CachedFile>,
}

impl BootstrapLoader {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            cache: HashMap::new(),
        }
    }

    /// Build the full system prompt. Call this before every API request.
    /// Files are re-read only when their mtime changes.
    pub fn build_system_prompt(&mut self, tool_descriptions: &str) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut total_chars: usize = 0;

        for &name in BOOTSTRAP_FILES {
            let content = self.load_with_cache(name);
            if content.is_empty() {
                continue;
            }

            let budget = MAX_PER_FILE_CHARS.min(MAX_TOTAL_CHARS.saturating_sub(total_chars));
            if budget < MIN_FILE_BUDGET {
                break;
            }

            let truncated = truncate_bootstrap(&content, budget);
            total_chars += truncated.chars().count();
            parts.push(truncated);
        }

        // HEARTBEAT.md as independent section (not in bootstrap pipeline)
        let heartbeat = self.load_with_cache("HEARTBEAT.md");
        if !heartbeat.is_empty() {
            let hb_budget = MAX_PER_FILE_CHARS.min(MAX_TOTAL_CHARS.saturating_sub(total_chars));
            if hb_budget >= MIN_FILE_BUDGET {
                let section = format!("## Heartbeats\n\n{}", truncate_bootstrap(&heartbeat, hb_budget));
                parts.push(section);
            }
        }

        // Tool descriptions always appended
        if !tool_descriptions.is_empty() {
            parts.push(tool_descriptions.to_string());
        }

        parts.join("\n\n---\n\n")
    }

    /// Load a file with mtime caching. Returns empty string if file doesn't exist.
    fn load_with_cache(&mut self, name: &str) -> String {
        let path = self.root.join(name);

        // Get current mtime
        let current_mtime = std::fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok());

        // Check cache
        if let Some(cached) = self.cache.get(name) {
            if let Some(mtime) = current_mtime {
                if cached.mtime == mtime {
                    return cached.content.clone();
                }
            }
        }

        // Read from disk
        let content = std::fs::read_to_string(&path).unwrap_or_default();

        // Update cache
        if let Some(mtime) = current_mtime {
            self.cache.insert(name.to_string(), CachedFile {
                content: content.clone(),
                mtime,
            });
        }

        content
    }
}

/// Truncate content to fit within budget chars.
/// Keeps head 70% + tail 20%, inserts truncation marker in the middle.
fn truncate_bootstrap(content: &str, budget: usize) -> String {
    let char_count = content.chars().count();
    if char_count <= budget {
        return content.to_string();
    }

    let head_chars = (budget as f64 * HEAD_RATIO) as usize;
    let tail_chars = (budget as f64 * TAIL_RATIO) as usize;

    let head: String = content.chars().take(head_chars).collect();
    let tail: String = content.chars().rev().take(tail_chars).collect::<Vec<_>>().into_iter().rev().collect();

    format!("{}\n\n... (truncated {} chars) ...\n\n{}", head, char_count - head_chars - tail_chars, tail)
}

// Legacy loader — kept for backward compatibility
pub struct WorkspaceLoader {
    root: PathBuf,
}

impl WorkspaceLoader {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn load(&self) -> Result<Workspace> {
        Ok(Workspace {
            soul: self.read_file("SOUL.md"),
            identity: self.read_file("IDENTITY.md"),
            user: self.read_file("USER.md"),
            agents: self.read_file("AGENTS.md"),
            memory: self.read_file("MEMORY.md"),
            state: self.read_file("STATE.md"),
            tools: self.read_file("TOOLS.md"),
            tasks: self.read_file("TASKS.md"),
            heartbeat: self.read_file("HEARTBEAT.md"),
        })
    }

    fn read_file(&self, name: &str) -> String {
        let path = self.root.join(name);
        std::fs::read_to_string(&path).unwrap_or_default()
    }
}
