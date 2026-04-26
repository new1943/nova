pub mod registry;
pub mod constants;

tokio::task_local! {
    pub static CURRENT_CHANNEL_ID: String;
}
pub mod truncate;
pub mod bash;
pub mod read_file;
pub mod write_file;
pub mod file_edit;
pub mod glob;
pub mod grep;
pub mod browser;
pub mod agentic_search;
pub mod file_tracker;
pub mod worktree;
pub mod agent;
pub mod team;
pub mod delegate_complex_project;
pub mod delegate_task;

pub use registry::{Tool, ToolRegistry};
pub use bash::BashTool;
pub use read_file::ReadFileTool;
pub use write_file::WriteFileTool;
pub use file_edit::FileEditTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use browser::BrowserTool;
pub use agentic_search::AgenticSearchTool;
pub use file_tracker::{FileReadTracker, create_shared_tracker, SharedFileReadTracker};
pub use worktree::WorktreeTool;
pub use agent::AgentTool;
pub use team::TeamTool;
pub use delegate_complex_project::{DelegateComplexProjectTool, CancelDelegatedProjectTool};
pub use delegate_task::DelegateTaskTool;

// Skill tools (implemented in skills/ module)
pub use crate::skills::{SkillManageTool, SkillsListTool, SkillViewTool};
