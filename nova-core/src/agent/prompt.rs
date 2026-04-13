use crate::workspace::Workspace;
use crate::tools::registry::ToolRegistry;

pub struct PromptBuilder;

impl PromptBuilder {
    /// Build system prompt from workspace files + tool schemas.
    /// Order: SOUL → IDENTITY → AGENTS → MEMORY → USER → tool descriptions
    pub fn build(workspace: &Workspace, tools: &ToolRegistry) -> String {
        let mut parts: Vec<&str> = Vec::new();

        let fields = [
            &workspace.soul,
            &workspace.identity,
            &workspace.agents,
            &workspace.memory,
            &workspace.user,
        ];

        for field in &fields {
            if !field.is_empty() {
                parts.push(field);
            }
        }

        let mut prompt = parts.join("\n\n---\n\n");

        // Append tool descriptions
        let tool_desc = tools.describe_all();
        if !tool_desc.is_empty() {
            prompt.push_str("\n\n---\n\n");
            prompt.push_str(&tool_desc);
        }

        prompt
    }
}
