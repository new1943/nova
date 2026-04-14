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
const BOOTSTRAP_FILES: &[&str] = &[
    "SOUL.md",
    "IDENTITY.md",
    "AGENTS.md",
    "USER.md",
    "STATE.md",
    "TASKS.md",
];

/// MEMORY.md is loaded separately — it's the working memory layer (Layer 1).
const MEMORY_FILE: &str = "MEMORY.md";

/// Memory system guidance injected into system prompt.
/// This guides the LLM to actively maintain MEMORY.md.
const MEMORY_GUIDANCE: &str = r#"## 记忆系统

你有一个三层记忆系统：

### 层1：MEMORY.md（工作记忆，始终可见）
- 路径：`~/.nova/MEMORY.md`
- 内容：用户偏好、项目状态、重要决策、参考资料
- 分类：`## 用户` / `## 项目` / `## 反馈` / `## 参考`
- 维护方式：
  - 用户说"记住..." → 用 file_edit 立即更新对应区块
  - 重要决策后 → 更新 `## 项目` 区块
  - 收到反馈 → 更新 `## 反馈` 区块
  - 保持 <200 行，精炼表达
- 写入时机：
  - 用户显式要求："记住这个"、"以后都用..."
  - 重要反馈："偏好 XXX"、"不要做 YYY"
  - 项目关键节点：方案选型、架构决策、重大变更
  - 不要每句话都记，只记值得长期保留的

### 层2：情景记忆（自动写入，不需你操心）
- 路径：`~/.nova/memories/YYYY-MM-DD.md`
- 系统自动在 Compact 前和 Session 结束时写入
- 你可以 `/search <关键词>` 手动召回相关记忆

### 层3：历史 Session（完整细节）
- 路径：`~/.nova/sessions/<uuid>.jsonl`
- 通过 Agentic Session Search 自动召回相关历史"#;

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

        // Append memory guidance (always, even if MEMORY.md doesn't exist yet)
        parts.push(MEMORY_GUIDANCE.to_string());

        // Inject MEMORY.md actual content (Layer 1 working memory)
        let memory_content = self.load_memory();
        if !memory_content.is_empty() {
            let mem_budget = MAX_TOTAL_CHARS.saturating_sub(total_chars).min(10_000);
            if mem_budget >= MIN_FILE_BUDGET {
                let truncated = truncate_bootstrap(&memory_content, mem_budget);
                parts.push(format!(
                    "\n\n---\n\n## MEMORY.md (Your Working Memory)\n\n{}\n\n---\n\n",
                    truncated
                ));
            }
        }

        // Tool descriptions always appended
        if !tool_descriptions.is_empty() {
            parts.push(tool_descriptions.to_string());
        }

        parts.join("\n\n---\n\n")
    }

    /// Load MEMORY.md (Layer 1 working memory).
    /// Returns empty string if file doesn't exist.
    pub fn load_memory(&mut self) -> String {
        self.load_with_cache(MEMORY_FILE)
    }

    /// Build a system prompt segment for MEMORY.md injection.
    /// Call this after BOOTSTRAP_FILES to include working memory in context.
    pub fn build_memory_injection(&mut self, max_chars: usize) -> String {
        let content = self.load_memory();
        if content.is_empty() {
            return String::new();
        }
        let truncated = truncate_bootstrap(&content, max_chars);
        format!(
            "\n\n---\n\n## Working Memory (MEMORY.md)\n\n{}\n\n---\n\n",
            truncated
        )
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
