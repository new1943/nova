use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Decision from an approval request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ApprovalDecision {
    Allow,
    AllowSession,
    Deny,
}

/// Handler for requesting user approval before tool execution
#[async_trait]
pub trait ApprovalHandler: Send + Sync {
    async fn request_approval(&self, tool_name: &str, arguments: &serde_json::Value) -> ApprovalDecision;
}

/// Tools that require user approval before execution
pub fn requires_approval(tool_name: &str) -> bool {
    matches!(tool_name, "bash" | "write_file" | "file_edit" | "delegate_complex_project")
}
