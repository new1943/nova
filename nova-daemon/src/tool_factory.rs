//! Tool factory — creates ToolRegistry instances for main agent and subagents.

use std::path::PathBuf;
use std::sync::Arc;

use nova_agent::hooks::HookManager;
use nova_memory::session::manager::SessionManager;
use nova_tools::skills::SharedSkillsLoader;
use nova_tools::{ToolRegistry, ReadFileTool, WriteFileTool, FileEditTool, GlobTool, GrepTool, BrowserTool, WorktreeTool, TeamTool, MemoryTool};
use nova_tools::bash::{BashTool, BashMode};
use nova_tools::skills::{SkillManageTool, SkillsListTool, SkillViewTool};
use nova_tools::SharedFileReadTracker;

use crate::agentic_search::AgenticSearchTool;

/// Create the full ToolRegistry for the main agent.
#[allow(clippy::too_many_arguments)]
pub fn make_tools(
    mode: &str,
    browser_chrome_path: Option<String>,
    browser_profile_dir: Option<String>,
    browser_headless: bool,
    file_tracker: SharedFileReadTracker,
    repo_root: Option<PathBuf>,
    teams_dir: Option<PathBuf>,
    skills_dir: PathBuf,
    skills: SharedSkillsLoader,
) -> ToolRegistry {
    let bash_mode = match mode {
        "sandbox" => BashMode::Sandbox,
        _ => BashMode::Open,
    };
    let mut tools = ToolRegistry::new();
    tools.register_builtin(Box::new(BashTool::new(bash_mode)));
    tools.register_builtin(Box::new(ReadFileTool::new(file_tracker.clone())));
    tools.register_builtin(Box::new(WriteFileTool::new(file_tracker.clone())));
    tools.register_builtin(Box::new(FileEditTool::new(file_tracker.clone())));
    tools.register_builtin(Box::new(GlobTool));
    tools.register_builtin(Box::new(GrepTool));

    tools.register_builtin(Box::new(BrowserTool::new(
        browser_chrome_path,
        browser_profile_dir,
        browser_headless,
    )));

    // v3: Memory tool — review/save/load
    tools.register_builtin(Box::new(MemoryTool::new()));

    // Agentic search — session history search
    // Note: AgenticSearchTool needs a SideQuery, but in v3 we pass it at construction time.
    // For now, we create a placeholder that will be replaced by the daemon.
    // This is a known limitation — the tool_factory doesn't have access to the LLM backend.
    // The daemon creates SideQuery and passes it via make_tools_with_search.
    // For simplicity, we skip AgenticSearchTool registration here if no SideQuery is available.
    // TODO: refactor to pass SideQuery through make_tools

    // Worktree tool — git worktree isolation per session
    if let Some(root) = repo_root {
        tools.register_builtin(Box::new(WorktreeTool::new(root)));
    }

    // Team tool — team/member/task management
    if let Some(dir) = teams_dir {
        tools.register_builtin(Box::new(TeamTool::new(dir)));
    }

    // Skill tools
    tools.register_builtin(Box::new(SkillManageTool::new(skills_dir.clone(), skills.clone())));
    tools.register_builtin(Box::new(SkillsListTool::new(skills.clone())));
    tools.register_builtin(Box::new(SkillViewTool::new(skills_dir, skills)));

    tools
}

/// Create the agentic search tool (requires SideQuery from daemon).
pub fn make_agentic_search_tool(
    side_query: nova_memory::sidequery::SideQuery,
    session_manager: SessionManager,
) -> AgenticSearchTool {
    AgenticSearchTool::new(side_query, session_manager)
}

/// Create HookManager with MemoryExtract hooks wired up.
pub fn make_hooks(workspace_dir: PathBuf) -> HookManager {
    use nova_agent::hooks::post_sampling::MemoryExtractHook;
    use nova_agent::hooks::stop::MemoryExtractStopHook;
    use nova_memory::memory::DualWriteMemory;
    use tokio::sync::Mutex;

    let dual_write = Arc::new(Mutex::new(DualWriteMemory::new(workspace_dir)));
    let mut hooks = HookManager::new();
    hooks.register_post_sampling(Box::new(MemoryExtractHook::new(dual_write.clone())));
    hooks.register_stop(Box::new(MemoryExtractStopHook::new(dual_write)));
    hooks
}

/// Generate tool descriptions string (used by BootstrapLoader)
pub fn tool_descriptions(tools: &ToolRegistry) -> String {
    tools.describe_all()
}
