use anyhow::Result;
use async_trait::async_trait;
use nova_llm::types::ToolSchema;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tracing::debug;

/// Tool execution context — replaces task_local! global state
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// Current message channel ID (TUI session ID or Discord channel ID)
    pub channel_id: String,
    /// Workspace directory
    pub workspace_dir: Option<PathBuf>,
}

impl ToolContext {
    pub fn new(channel_id: String, workspace_dir: Option<PathBuf>) -> Self {
        Self { channel_id, workspace_dir }
    }
}

/// ToolHandler trait — replaces the original Tool trait
#[async_trait]
pub trait ToolHandler: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;
    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<String>;
}

/// Tool registry with stable ordering (builtin first, MCP second)
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn ToolHandler>>,
    builtin_order: Vec<String>,
    mcp_order: Vec<String>,
}

impl Default for ToolRegistry {
    fn default() -> Self { Self::new() }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            builtin_order: Vec::new(),
            mcp_order: Vec::new(),
        }
    }

    pub fn register_builtin(&mut self, tool: Box<dyn ToolHandler>) {
        let name = tool.name().to_string();
        self.builtin_order.push(name.clone());
        self.tools.insert(name, tool);
    }

    pub fn register_mcp(&mut self, tool: Box<dyn ToolHandler>) {
        let name = tool.name().to_string();
        self.mcp_order.push(name.clone());
        self.tools.insert(name, tool);
    }

    /// Export tool schemas in stable order (builtin first, MCP second)
    pub fn as_api_schemas(&self) -> Vec<ToolSchema> {
        self.builtin_order
            .iter()
            .chain(self.mcp_order.iter())
            .filter_map(|name| self.tools.get(name))
            .map(|t| ToolSchema {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect()
    }

    /// Export tool schemas filtered by name predicate.
    pub fn as_api_schemas_filtered<F>(&self, filter: F) -> Vec<ToolSchema>
    where
        F: Fn(&str) -> bool,
    {
        let names: Vec<&String> = self.builtin_order
            .iter()
            .chain(self.mcp_order.iter())
            .filter(|name| filter(name))
            .collect();
        debug!("[V4] Tool filtering: {} tools allowed: {:?}", names.len(), names);
        names
            .iter()
            .filter_map(|name| self.tools.get(*name))
            .map(|t| ToolSchema {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect()
    }

    /// Execute a tool by name with timeout
    pub async fn execute(
        &self,
        name: &str,
        input: Value,
        ctx: &ToolContext,
        timeout: Duration,
    ) -> Result<String> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Tool not found: {}", name))?;

        match tokio::time::timeout(timeout, tool.execute(input, ctx)).await {
            Ok(result) => result,
            Err(_) => anyhow::bail!("Tool '{}' timed out after {:?}", name, timeout),
        }
    }

    /// Human-readable description of all tools
    pub fn describe_all(&self) -> String {
        let mut desc = String::from("Available tools:\n");
        for name in self.builtin_order.iter().chain(self.mcp_order.iter()) {
            if let Some(tool) = self.tools.get(name) {
                desc.push_str(&format!("- {}: {}\n", tool.name(), tool.description()));
            }
        }
        desc
    }
}

/// ToolRegistry 实现 nova_core::executor::ToolExecutor trait，
/// 使 Executor 模块能通过抽象接口调用工具（解耦 nova-core ↔ nova-tools 依赖）。
#[async_trait]
impl nova_core::executor::ToolExecutor for ToolRegistry {
    async fn execute(
        &self,
        name: &str,
        args: Value,
        ctx: &nova_core::executor::ToolExecContext,
    ) -> Result<String> {
        let tool_ctx = ToolContext::new(ctx.channel_id.clone(), ctx.workspace_dir.clone());
        let timeout = Duration::from_secs(60);
        self.execute(name, args, &tool_ctx, timeout).await
    }

    fn is_read_only(&self, tool_name: &str) -> bool {
        // 写工具名称列表 — 不是只读的工具
        // 这里集中管理，取代之前 review.rs/project.rs 中分散的 substring 匹配
        const WRITE_TOOLS: &[&str] = &[
            "write_file", "file_edit", "execute_bash", "browser",
        ];
        !WRITE_TOOLS.contains(&tool_name)
    }
}
