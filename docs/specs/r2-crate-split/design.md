# Design Document: R2 Crate 拆分

## Overview

将 Nova workspace 从 5 个 crate 重构为 8 个 crate，实现模块级编译隔离和独立测试。核心策略是"自底向上提取"：先提纯 nova-core 为共享类型层，再依次独立 nova-tools、nova-memory、nova-agent。

## Architecture

### 目标 Crate DAG

```
                    nova-core (共享类型 + trait)
                   /          |           \
              nova-llm    nova-tools    nova-memory
                   \          |           /
                    \         |          /
                     nova-agent
                         |
                    nova-daemon
                    /         \
              nova-ipc      nova-tui
```

### 依赖矩阵

| Crate | 依赖 |
|-------|------|
| nova-core | 无 workspace 依赖（仅第三方：serde, chrono, uuid, async-trait, anyhow, tokio, tracing） |
| nova-llm | 无 workspace 依赖（仅第三方：reqwest, serde, tokio, futures） |
| nova-tools | nova-core, nova-llm |
| nova-memory | nova-core, nova-llm |
| nova-agent | nova-core, nova-llm, nova-tools, nova-memory |
| nova-ipc | nova-core |
| nova-daemon | nova-core, nova-llm, nova-tools, nova-memory, nova-agent, nova-ipc |
| nova-tui | nova-core, nova-ipc |

### nova-core 提纯后的模块结构

```
nova-core/src/
├── lib.rs
├── message.rs          # Message, Role, ToolCall
├── models/
│   ├── mod.rs          # ShadowEventEmitter trait
│   └── events.rs       # ShadowEvent, TaskAction
├── config.rs           # NovaConfig
└── pipeline.rs         # TurnContext, PipelineStage trait, PromptInjection, DecisionEntry
```

关键决策：`TurnContext` 和 `PipelineStage` trait 保留在 nova-core 中，因为它们是 nova-agent 和 nova-daemon 都需要引用的共享接口。`agent/preflight.rs` 中的 `Complexity` 和 `PreFlightCheckResult` 也提升到 nova-core，因为 `TurnContext` 直接引用它们。

### nova-tools 模块结构

```
nova-tools/src/
├── lib.rs
├── registry.rs         # ToolHandler trait, ToolContext, ToolRegistry
├── constants.rs
├── truncate.rs
├── delegate_base.rs    # 新增：delegate 公共逻辑
├── delegate_task.rs
├── delegate_complex_project.rs
├── bash/
│   ├── mod.rs
│   └── security/
├── bash.rs
├── read_file.rs
├── write_file.rs
├── file_edit.rs
├── glob.rs
├── grep.rs
├── browser.rs
├── agentic_search.rs
├── file_tracker.rs
├── worktree.rs
├── agent.rs
└── team.rs
```

### nova-memory 模块结构

```
nova-memory/src/
├── lib.rs
├── memory/
│   ├── mod.rs
│   ├── dual_write.rs
│   ├── store.rs
│   ├── daily.rs
│   ├── recall.rs
│   ├── dream.rs
│   ├── consolidate.rs
│   ├── topic_state.rs
│   ├── tension_tracker.rs
│   ├── mode_router.rs
│   └── memory_board.rs
├── sidequery/
│   ├── mod.rs
│   ├── query.rs
│   └── memory_keeper.rs
└── session/
    ├── mod.rs
    ├── manager.rs
    ├── history.rs
    └── search.rs
```

### nova-agent 模块结构

```
nova-agent/src/
├── lib.rs
├── pipeline.rs         # TurnPipeline 实现（使用 nova-core 的 PipelineStage trait）
├── stages/
│   ├── mod.rs
│   ├── classify.rs
│   ├── track.rs
│   ├── gate.rs
│   ├── inject.rs
│   └── execute_config.rs
├── loop.rs             # QueryLoop
├── preflight.rs        # PreFlightChecker（实现逻辑）
├── context.rs          # AgentContext
├── forked.rs
├── coordinator/
│   ├── mod.rs
│   └── orchestrator.rs
└── subagent/
    ├── mod.rs
    └── spawn.rs
```

## Detailed Design

### 1. ToolHandler Trait 设计

当前 `Tool` trait 的 `execute` 方法没有上下文参数，导致 `delegate_task` 和 `delegate_complex_project` 必须通过 `task_local! CURRENT_CHANNEL_ID` 获取 channel ID。

新设计引入 `ToolContext`：

```rust
// nova-tools/src/registry.rs

/// 工具执行上下文 — 替代 task_local! 全局状态
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// 当前消息的 channel ID（TUI session ID 或 Discord channel ID）
    pub channel_id: String,
    /// 工作区目录
    pub workspace_dir: Option<std::path::PathBuf>,
}

/// ToolHandler trait — 替代原 Tool trait
#[async_trait]
pub trait ToolHandler: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> serde_json::Value;
    async fn execute(&self, input: serde_json::Value, ctx: &ToolContext) -> anyhow::Result<String>;
}
```

`ToolRegistry` 更新为使用 `ToolHandler`：

```rust
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn ToolHandler>>,
    builtin_order: Vec<String>,
    mcp_order: Vec<String>,
}

impl ToolRegistry {
    pub async fn execute(
        &self,
        name: &str,
        input: Value,
        ctx: &ToolContext,
        timeout: Duration,
    ) -> Result<String> {
        let tool = self.tools.get(name)
            .ok_or_else(|| anyhow::anyhow!("Tool not found: {}", name))?;
        match tokio::time::timeout(timeout, tool.execute(input, ctx)).await {
            Ok(result) => result,
            Err(_) => anyhow::bail!("Tool '{}' timed out after {:?}", name, timeout),
        }
    }
}
```

### 2. Delegate Base 设计

提取 `delegate_task.rs` 和 `delegate_complex_project.rs` 的公共逻辑：

```rust
// nova-tools/src/delegate_base.rs

use std::sync::Arc;
use once_cell::sync::Lazy;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::AbortHandle;
use std::collections::HashMap;

/// 全局运行中项目注册表
pub static RUNNING_PROJECTS: Lazy<AsyncMutex<HashMap<String, AbortHandle>>> =
    Lazy::new(|| AsyncMutex::new(HashMap::new()));

/// 委派任务的公共配置
pub struct DelegateConfig {
    pub emitter: Arc<dyn ShadowEventEmitter>,
    pub shadow_tx: tokio::sync::mpsc::Sender<ShadowEvent>,
    pub api_key: String,
    pub api_base_url: String,
    pub model: String,
    pub system_prompt: String,
    pub tools: Option<Arc<ToolRegistry>>,
    pub workspace_dir: Option<std::path::PathBuf>,
}

/// 委派任务的公共生命周期管理
pub async fn spawn_delegated_task<F, Fut>(
    config: &DelegateConfig,
    task_description: &str,
    channel_id: String,
    executor: F,
) -> String
where
    F: FnOnce(DelegateConfig, String) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<String>> + Send,
{
    let project_id = uuid::Uuid::new_v4().to_string();
    // ... 公共逻辑：TaskLogger、spawn、AbortHandle 注册、完成回调
    project_id
}
```

### 3. nova-core 提纯策略

**保留在 nova-core 的内容：**
- `message.rs` — Message, Role, ToolCall（被所有 crate 使用）
- `models/events.rs` — ShadowEvent, TaskAction（事件总线协议）
- `models/mod.rs` — ShadowEventEmitter trait
- `config.rs` — NovaConfig（配置加载）
- `pipeline.rs` — TurnContext, PipelineStage trait, PromptInjection, DecisionEntry（pipeline 接口）
- `agent/preflight.rs` 中的类型定义部分 — Complexity, PreFlightCheckResult（被 TurnContext 引用）

**移出 nova-core 的内容：**
- `tools/` → nova-tools
- `memory/` → nova-memory
- `sidequery/` → nova-memory
- `session/` → nova-memory
- `agent/loop.rs`, `agent/stages/`, `agent/context.rs`, `agent/forked.rs` → nova-agent
- `coordinator/` → nova-agent
- `subagent/` → nova-agent
- `workspace/` → nova-agent（BootstrapLoader 被 agent 和 daemon 使用）
- `hooks/` → nova-agent
- `token/` → nova-agent（budget、compact、counter 被 QueryLoop 使用）
- `skills/` → nova-tools（skill 工具是 tools 的一部分）
- `dream/` → nova-memory（DreamEngine 是 Layer 4 记忆）
- `heartbeat/` → nova-agent
- `team/` → nova-tools（team 工具相关）
- `paste/` → nova-tools
- `sandbox/` → nova-tools
- `retry/` → nova-core（通用重试策略，可被多个 crate 使用）
- `task/` → nova-tools（TaskLogger 被 delegate 工具使用）
- `worktree/` → nova-tools

### 4. nova-api → nova-llm 重命名

步骤：
1. 重命名目录 `nova-api/` → `nova-llm/`
2. 更新 `nova-llm/Cargo.toml` 中 `name = "nova-llm"`
3. 更新根 `Cargo.toml` workspace members
4. 全局替换 `nova-api` → `nova-llm`（Cargo.toml 依赖、`use nova_api::` → `use nova_llm::`）

### 5. 循环依赖风险分析

**风险点 1：QueryLoop 依赖 memory + tools + sidequery**

QueryLoop（nova-agent）直接使用 `MemoryConsolidator`、`DailyNotes`、`SideQuery`、`TopicTracker`、`TensionTracker`、`MemoryBoard`（来自 nova-memory）和 `ToolRegistry`（来自 nova-tools）。

解决方案：nova-agent 同时依赖 nova-tools 和 nova-memory，这在 DAG 中是合法的，因为 nova-tools 和 nova-memory 互不依赖。

**风险点 2：delegate 工具依赖 Coordinator 和 SubagentSpawner**

`delegate_complex_project` 使用 `Coordinator`（将在 nova-agent 中），`delegate_task` 使用 `SubagentSpawner`（将在 nova-agent 中）。但 nova-tools 不能依赖 nova-agent（会形成循环）。

解决方案：将 `delegate_task` 和 `delegate_complex_project` 移到 nova-agent 而非 nova-tools。nova-tools 只包含"纯工具"（bash、file、grep 等），delegate 工具因为依赖 Coordinator/SubagentSpawner 属于 agent 层。ToolRegistry 在 nova-tools 中定义，但 delegate 工具在 nova-agent 中实现并在 nova-daemon 的 tool_factory 中注册。

**风险点 3：BootstrapLoader 依赖 ToolRegistry**

`BootstrapLoader.build_system_prompt()` 接收 tool descriptions。BootstrapLoader 本身不依赖 ToolRegistry 类型，只接收 `&str`，所以可以放在 nova-agent 或 nova-core 中。放在 nova-agent 更合适，因为它是 agent 启动流程的一部分。

**风险点 4：Skills 模块依赖**

`skills/` 模块导出 `SkillManageTool`、`SkillsListTool`、`SkillViewTool`，这些是 Tool 实现。它们应该放在 nova-tools 中。

### 6. 修正后的模块归属

基于循环依赖分析，修正模块归属：

| 模块 | 目标 Crate | 原因 |
|------|-----------|------|
| delegate_task, delegate_complex_project | nova-agent | 依赖 Coordinator/SubagentSpawner |
| skills/ | nova-tools | 纯工具实现 |
| workspace/ | nova-agent | BootstrapLoader 是 agent 启动流程 |
| hooks/ | nova-agent | 被 QueryLoop 使用 |
| token/ | nova-agent | 被 QueryLoop 使用 |
| dream/ | nova-memory | Layer 4 记忆 |
| heartbeat/ | nova-agent | 被 agent 调度 |
| team/ | nova-tools | team 工具 |
| paste/ | nova-tools | paste 工具 |
| sandbox/ | nova-tools | sandbox 策略 |
| retry/ | nova-core | 通用重试策略 |
| task/ | nova-tools | TaskLogger |
| worktree/ (src/worktree/) | nova-tools | worktree 工具 |

## Correctness Properties

### Property 1: DAG 依赖无循环

对于 workspace 中的每个 crate，其 `Cargo.toml` 中声明的 workspace 依赖必须符合预定义的 DAG 约束。具体来说：
- nova-core 的 workspace 依赖集合为空
- nova-llm 的 workspace 依赖集合为空
- nova-tools 的 workspace 依赖集合 ⊆ {nova-core, nova-llm}
- nova-memory 的 workspace 依赖集合 ⊆ {nova-core, nova-llm}
- nova-agent 的 workspace 依赖集合 ⊆ {nova-core, nova-llm, nova-tools, nova-memory}

验证方式：解析所有 Cargo.toml 文件，提取 `[dependencies]` 中 `path = "../nova-*"` 的条目，检查是否符合约束。

### Property 2: 编译隔离 — nova-tools 与 nova-memory 互不依赖

nova-tools 的 Cargo.toml 不包含对 nova-memory 的依赖，nova-memory 的 Cargo.toml 不包含对 nova-tools 的依赖。这保证修改一方不触发另一方重编译。

验证方式：检查两个 Cargo.toml 文件的依赖列表。

### Property 3: task_local! CURRENT_CHANNEL_ID 消除

在整个 workspace 中，不存在 `task_local!` 宏定义 `CURRENT_CHANNEL_ID`，也不存在 `CURRENT_CHANNEL_ID.try_with` 调用。所有 channel_id 通过 `ToolContext` 参数传递。

验证方式：全局 grep 搜索 `CURRENT_CHANNEL_ID`。

### Property 4: 功能不退化

`cargo test --workspace` 通过所有现有测试（≥78 个），`cargo build --release` 编译成功。

验证方式：运行 cargo 命令。

### Property 5: Workspace 成员完整性

根 `Cargo.toml` 的 `[workspace] members` 列表恰好包含 8 个 crate：nova-core、nova-llm、nova-tools、nova-memory、nova-agent、nova-ipc、nova-daemon、nova-tui。

验证方式：解析根 Cargo.toml。

## Test Strategy

本次重构主要是结构性变更（文件搬迁、依赖调整），核心验证手段是：

1. **编译验证**：`cargo build --release` — 确保所有 crate 编译通过，无循环依赖
2. **测试回归**：`cargo test --workspace` — 确保所有现有测试通过
3. **依赖审计**：脚本检查每个 crate 的 Cargo.toml 依赖是否符合 DAG 约束
4. **全局搜索**：确认 `CURRENT_CHANNEL_ID` task_local 已完全消除
5. **独立测试**：`cargo test -p nova-tools`、`cargo test -p nova-memory`、`cargo test -p nova-agent` 各自通过

不使用 Property-Based Testing 的原因：本次变更是纯结构性重构，不涉及算法逻辑变更。验证点都是确定性的（编译通过/失败、依赖存在/不存在），不需要随机输入探索。
