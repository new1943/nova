//! Tool factory — creates ToolRegistry instances for main agent and subagents.

use std::path::PathBuf;
use std::sync::Arc;

use nova_core::hooks::HookManager;
use nova_core::session::manager::SessionManager;
use nova_core::sidequery::SideQuery;
use nova_core::skills::SharedSkillsLoader;
use nova_core::tools::{ToolRegistry, ReadFileTool, WriteFileTool, FileEditTool, GlobTool, GrepTool, BrowserTool, AgenticSearchTool, WorktreeTool, AgentTool, TeamTool};
use nova_core::tools::bash::{BashTool, BashMode};
use nova_core::skills::{SkillManageTool, SkillsListTool, SkillViewTool};

use crate::dispatcher;

/// Create a ToolRegistry for Coordinator's SubAgents.
/// Contains ONLY subagent-appropriate tools: bash, read_file, write_file, file_edit, glob, grep, browser.
/// Does NOT include delegate_complex_project (avoids circular dependency).
///
/// SubAgent BrowserTool uses an isolated Chrome Profile to avoid CDP session conflicts.
pub fn make_subagent_tools(
    browser_chrome_path: Option<String>,
    _browser_profile_dir: Option<String>,
    browser_headless: bool,
    file_tracker: nova_core::tools::SharedFileReadTracker,
) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    tools.register_builtin(Box::new(BashTool::new(BashMode::Open)));
    tools.register_builtin(Box::new(ReadFileTool::new(file_tracker.clone())));
    tools.register_builtin(Box::new(WriteFileTool::new(file_tracker.clone())));
    tools.register_builtin(Box::new(FileEditTool::new(file_tracker.clone())));
    tools.register_builtin(Box::new(GlobTool));
    tools.register_builtin(Box::new(GrepTool));
    // SubAgent uses isolated Chrome Profile
    let subagent_profile = format!(
        "{}/.nova/chrome-subagent-{}",
        dirs::home_dir().unwrap().display(),
        &uuid::Uuid::new_v4().to_string()[..8]
    );
    tools.register_builtin(Box::new(BrowserTool::new(
        browser_chrome_path,
        Some(subagent_profile),
        browser_headless,
    )));
    tools
}

/// Create the full ToolRegistry for the main agent.
#[allow(clippy::too_many_arguments)]
pub fn make_tools(
    mode: &str,
    browser_chrome_path: Option<String>,
    browser_profile_dir: Option<String>,
    browser_headless: bool,
    side_query: SideQuery,
    session_manager: SessionManager,
    file_tracker: nova_core::tools::SharedFileReadTracker,
    repo_root: Option<PathBuf>,
    teams_dir: Option<PathBuf>,
    api_key: String,
    api_base_url: String,
    model: String,
    skills_dir: PathBuf,
    skills: SharedSkillsLoader,
    dispatcher_tx: Arc<dispatcher::DispatcherSender>,
    shadow_tx: tokio::sync::mpsc::Sender<nova_core::models::ShadowEvent>,
    subagent_tools: Option<Arc<ToolRegistry>>,
    workspace_dir: PathBuf,
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

    tools.register_builtin(Box::new(AgenticSearchTool::new(side_query, session_manager)));

    // Agent tool — spawn subagents for parallel/background tasks
    tools.register_builtin(Box::new(AgentTool::new(api_key.clone(), api_base_url.clone(), model.clone()).with_shadow_tx(shadow_tx.clone())));

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

    // delegate_complex_project
    let delegate_tool = nova_core::tools::DelegateComplexProjectTool::new(
        dispatcher_tx.clone(),
        shadow_tx.clone(),
        api_key.clone(),
        api_base_url.clone(),
        model.clone(),
    ).with_workspace_dir(workspace_dir.clone());
    let delegate_tool = if let Some(ref st) = subagent_tools {
        delegate_tool.with_tools(st.clone())
    } else {
        delegate_tool
    };
    tools.register_builtin(Box::new(delegate_tool));
    tools.register_builtin(Box::new(nova_core::tools::CancelDelegatedProjectTool::new()));

    // delegate_task — for Medium complexity single-task delegation
    let delegate_task_tool = nova_core::tools::DelegateTaskTool::new(
        dispatcher_tx.clone(),
        shadow_tx.clone(),
        api_key.clone(),
        api_base_url.clone(),
        model.clone(),
    ).with_workspace_dir(workspace_dir.clone());
    let delegate_task_tool = if let Some(st) = subagent_tools {
        delegate_task_tool.with_tools(st)
    } else {
        delegate_task_tool
    };
    tools.register_builtin(Box::new(delegate_task_tool));

    tools
}

/// Create HookManager (currently empty — old hooks disabled)
pub fn make_hooks() -> HookManager {
    HookManager::new()
}

/// Generate tool descriptions string (used by BootstrapLoader)
pub fn tool_descriptions(tools: &ToolRegistry) -> String {
    tools.describe_all()
}
