use anyhow::Result;
use async_trait::async_trait;
use nova_api::types::ToolSchema;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

/// Tool trait — all tools must implement this
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;
    async fn execute(&self, input: Value) -> Result<String>;
}

/// Tool registry with stable ordering (builtin first, MCP second)
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
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

    pub fn register_builtin(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name().to_string();
        self.builtin_order.push(name.clone());
        self.tools.insert(name, tool);
    }

    pub fn register_mcp(&mut self, tool: Box<dyn Tool>) {
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

    /// Execute a tool by name with timeout
    pub async fn execute(
        &self,
        name: &str,
        input: Value,
        timeout: Duration,
    ) -> Result<String> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Tool not found: {}", name))?;

        match tokio::time::timeout(timeout, tool.execute(input)).await {
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
