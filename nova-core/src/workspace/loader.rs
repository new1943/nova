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

/// 内置 Agent 行为宪法 — 不可被用户篡改
/// [V5] 通过 include_str! 编译嵌入，保证不可运行时篡改
const BUILTIN_AGENTS: &str = include_str!("../../prompts/AGENTS.md");

/// Bootstrap files to load, in injection order.
/// [V4 spec] Only SOUL.md, IDENTITY.md, USER.md are allowed.
/// AGENTS.md, STATE.md, TASKS.md are deprecated and must not be loaded.
const BOOTSTRAP_FILES: &[&str] = &[
    "SOUL.md",
    "IDENTITY.md",
    "USER.md",
];

/// MEMORY.md is loaded separately — it's the working memory layer (Layer 1).
const MEMORY_FILE: &str = "MEMORY.md";

/// Cached file entry with mtime for change detection
#[derive(Debug, Clone)]
struct CachedFile {
    content: String,
    mtime: SystemTime,
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
    ///
    /// [V5] 组装顺序：
    /// 第一层: SOUL.md, IDENTITY.md
    /// 第二层: BUILTIN_AGENTS（内置行为宪法，include_str! 嵌入）
    /// 第三层: 工具描述
    /// 第四层: USER.md, HEARTBEAT.md, MEMORY.md
    pub fn build_system_prompt(&mut self, tool_descriptions: &str) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut total_chars: usize = 0;

        // 第零层：当前系统时间和环境信息
        let current_time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z").to_string();
        parts.push(format!("# Current System Environment\n- Local Time: {}", current_time));

        // 第一层：SOUL.md, IDENTITY.md
        for &name in &["SOUL.md", "IDENTITY.md"] {
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

        // 第二层：内置 Agent 行为宪法（包含记忆规范、技能规范、派发规则、安全红线）
        parts.push(BUILTIN_AGENTS.to_string());

        // 第三层：工具描述
        if !tool_descriptions.is_empty() {
            parts.push(tool_descriptions.to_string());
        }

        // 第四层：USER.md, HEARTBEAT.md, tasks.md, MEMORY.md
        for &name in &["USER.md", "HEARTBEAT.md", "tasks.md"] {
            let content = self.load_with_cache(name);
            if !content.is_empty() {
                let budget = MAX_PER_FILE_CHARS.min(MAX_TOTAL_CHARS.saturating_sub(total_chars));
                if budget >= MIN_FILE_BUDGET {
                    let truncated = truncate_bootstrap(&content, budget);
                    total_chars += truncated.chars().count();
                    
                    // 为 tasks.md 增加包裹标签以防混淆
                    if name == "tasks.md" {
                        parts.push(format!("<current_tasks>\n{}\n</current_tasks>", truncated));
                    } else {
                        parts.push(truncated);
                    }
                }
            }
        }

        // MEMORY.md 单独注入（带专用截断，10_000 chars）
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



