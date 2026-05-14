pub mod chain;
pub mod parallel;
pub mod project;
pub mod react;
pub mod registry;
pub mod review;
pub mod types;
pub mod util;

use async_trait::async_trait;
use serde_json::Value;

/// 工具执行抽象 — 由 ToolRegistry 实现，注入到 Executor。
/// 解决 nova-core 不能依赖 nova-tools 的依赖方向问题。
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// 执行指定工具，返回结果文本
    async fn execute(&self, name: &str, args: Value, ctx: &ToolExecContext) -> anyhow::Result<String>;

    /// 判断工具是否只读（用于 reviewer/verification agent 过滤可写工具）
    fn is_read_only(&self, _tool_name: &str) -> bool {
        false // 默认非只读，由实现方覆盖
    }
}

/// Executor 用的工具上下文（nova-core 内部使用，不依赖 nova-tools::ToolContext）
#[derive(Debug, Clone)]
pub struct ToolExecContext {
    pub channel_id: String,
    pub workspace_dir: Option<std::path::PathBuf>,
}

impl ToolExecContext {
    pub fn new(channel_id: String, workspace_dir: Option<std::path::PathBuf>) -> Self {
        Self { channel_id, workspace_dir }
    }
}
