# Tasks: R2 Crate 拆分

## Phase 1: nova-core 提纯 + nova-api 重命名 (Day 4 前半)

- [x] 1.1 将 `nova-api` 重命名为 `nova-llm`
  - [x] 1.1.1 重命名目录 `nova-api/` → `nova-llm/`
  - [x] 1.1.2 更新 `nova-llm/Cargo.toml` 中 `name = "nova-llm"`
  - [x] 1.1.3 更新根 `Cargo.toml` workspace members：`nova-api` → `nova-llm`
  - [x] 1.1.4 全局替换所有 `Cargo.toml` 中的 `nova-api` 依赖为 `nova-llm`
  - [x] 1.1.5 全局替换所有 `use nova_api::` 为 `use nova_llm::`，以及 `extern crate nova_api` 为 `extern crate nova_llm`
  - [x] 1.1.6 验证 `cargo build --release` 通过

- [x] 1.2 提纯 nova-core：提取 Complexity 和 PreFlightCheckResult 类型到 nova-core 顶层
  - [x] 1.2.1 在 `nova-core/src/` 创建 `preflight_types.rs`，将 `Complexity` enum 和 `PreFlightCheckResult` struct 的类型定义（不含实现逻辑）从 `agent/preflight.rs` 复制过来
  - [x] 1.2.2 更新 `nova-core/src/pipeline.rs`（从 `agent/pipeline.rs` 移动）使其引用新的 `preflight_types` 模块
  - [x] 1.2.3 更新 `nova-core/src/lib.rs` 导出 `preflight_types` 和 `pipeline` 模块

- [x] 1.3 提纯 nova-core：移除将要搬出的模块
  - [x] 1.3.1 从 `nova-core/src/lib.rs` 移除以下模块声明：`agent`、`tools`、`memory`、`session`、`sidequery`、`coordinator`、`subagent`、`hooks`、`token`、`skills`、`dream`、`heartbeat`、`team`、`paste`、`sandbox`、`task`、`workspace`、`worktree`（暂时注释，待新 crate 创建后再删除源文件）
  - [x] 1.3.2 保留 `nova-core/src/lib.rs` 中的：`config`、`message`、`models`、`pipeline`、`preflight_types`、`retry`
  - [x] 1.3.3 更新 `nova-core/Cargo.toml`：移除不再需要的依赖（如 `chromiumoxide`、`tiktoken-rs`、`glob`、`regex`、`strsim`、`yaml`、`base64`、`tokio-tungstenite`、`futures-util`、`once_cell`、`dirs`），仅保留共享类型所需的依赖
  - [x] 1.3.4 移除 `nova-core` 对 `nova-api`/`nova-llm` 的依赖（提纯后的 nova-core 不依赖任何 workspace crate）

## Phase 2: nova-tools 独立 (Day 4 后半)

- [x] 2.1 创建 nova-tools crate 骨架
  - [x] 2.1.1 创建 `nova-tools/Cargo.toml`，声明依赖 `nova-core`（path）和 `nova-llm`（path），以及所需的第三方依赖（serde, serde_json, anyhow, async-trait, tokio, tracing, once_cell, uuid, regex, glob, chromiumoxide, base64, futures, dirs, strsim, yaml）
  - [x] 2.1.2 创建 `nova-tools/src/lib.rs` 骨架
  - [x] 2.1.3 更新根 `Cargo.toml` 添加 `nova-tools` 到 workspace members

- [x] 2.2 实现 ToolHandler trait 和 ToolContext
  - [x] 2.2.1 在 `nova-tools/src/registry.rs` 定义 `ToolContext` struct（包含 `channel_id: String`、`workspace_dir: Option<PathBuf>`）
  - [x] 2.2.2 在 `nova-tools/src/registry.rs` 定义 `ToolHandler` trait（`name`、`description`、`input_schema`、`execute(&self, input: Value, ctx: &ToolContext) -> Result<String>`）
  - [x] 2.2.3 更新 `ToolRegistry` 使用 `ToolHandler` 替代 `Tool`，`execute` 方法增加 `ctx: &ToolContext` 参数

- [x] 2.3 搬迁工具文件到 nova-tools
  - [x] 2.3.1 将 `nova-core/src/tools/` 下的纯工具文件搬入 `nova-tools/src/`：bash.rs、bash/（security 子模块）、read_file.rs、write_file.rs、file_edit.rs、glob.rs、grep.rs、browser.rs、agentic_search.rs、file_tracker.rs、worktree.rs、agent.rs、team.rs、constants.rs、truncate.rs
  - [x] 2.3.2 将 `nova-core/src/skills/` 搬入 `nova-tools/src/skills/`
  - [x] 2.3.3 将 `nova-core/src/paste/` 搬入 `nova-tools/src/paste/`
  - [x] 2.3.4 将 `nova-core/src/sandbox/` 搬入 `nova-tools/src/sandbox/`
  - [x] 2.3.5 将 `nova-core/src/task/` 搬入 `nova-tools/src/task/`（TaskLogger）
  - [x] 2.3.6 更新所有搬迁文件中的 `use crate::` 路径为正确的跨 crate 引用（`use nova_core::`）

- [x] 2.4 迁移所有工具到 ToolHandler trait
  - [x] 2.4.1 将每个工具的 `impl Tool for XxxTool` 改为 `impl ToolHandler for XxxTool`，`execute` 方法签名增加 `ctx: &ToolContext`
  - [x] 2.4.2 将所有 `CURRENT_CHANNEL_ID.try_with(|id| id.clone())` 调用替换为 `ctx.channel_id.clone()`
  - [x] 2.4.3 将所有 `workspace_dir` 字段从工具 struct 中移除，改为从 `ctx.workspace_dir` 获取
  - [x] 2.4.4 从 `nova-tools/src/lib.rs` 中移除 `task_local! { pub static CURRENT_CHANNEL_ID: String; }`
  - [x] 2.4.5 验证 `cargo build -p nova-tools` 通过

- [x] 2.5 创建 delegate_base 模块（在 nova-tools 中定义接口，实际 delegate 工具将在 nova-agent 中）
  - [x] 2.5.1 在 `nova-tools/src/delegate_base.rs` 中定义 `RUNNING_PROJECTS` 静态注册表和 `DelegateConfig` struct
  - [x] 2.5.2 实现 `spawn_delegated_task` 公共函数：生成 project_id、TaskLogger 记录、spawn 后台任务、注册 AbortHandle、完成后清理和事件发射
  - [x] 2.5.3 实现 `cancel_delegated_task` 公共函数
  - [x] 2.5.4 验证 `cargo test -p nova-tools` 通过

## Phase 3: nova-memory 独立 (Day 5)

- [x] 3.1 创建 nova-memory crate 骨架
  - [x] 3.1.1 创建 `nova-memory/Cargo.toml`，声明依赖 `nova-core`（path）和 `nova-llm`（path），以及所需第三方依赖（serde, serde_json, anyhow, tokio, tracing, chrono, uuid）
  - [x] 3.1.2 创建 `nova-memory/src/lib.rs` 骨架
  - [x] 3.1.3 更新根 `Cargo.toml` 添加 `nova-memory` 到 workspace members

- [x] 3.2 搬迁记忆相关文件到 nova-memory
  - [x] 3.2.1 将 `nova-core/src/memory/` 整个目录搬入 `nova-memory/src/memory/`
  - [x] 3.2.2 将 `nova-core/src/sidequery/` 整个目录搬入 `nova-memory/src/sidequery/`
  - [x] 3.2.3 将 `nova-core/src/session/` 整个目录搬入 `nova-memory/src/session/`
  - [x] 3.2.4 将 `nova-core/src/dream/` 搬入 `nova-memory/src/dream/`
  - [x] 3.2.5 更新所有搬迁文件中的 `use crate::` 路径为跨 crate 引用（`use nova_core::` 等）
  - [x] 3.2.6 更新 `nova-memory/src/lib.rs` 导出所有模块和关键类型

- [x] 3.3 验证 nova-memory 独立性
  - [x] 3.3.1 确认 `nova-memory/Cargo.toml` 不包含对 `nova-tools` 或 `nova-agent` 的依赖
  - [x] 3.3.2 验证 `cargo build -p nova-memory` 通过
  - [x] 3.3.3 验证 `cargo test -p nova-memory` 通过

## Phase 4: nova-agent 独立 (Day 6 前半)

- [x] 4.1 创建 nova-agent crate 骨架
  - [x] 4.1.1 创建 `nova-agent/Cargo.toml`，声明依赖 `nova-core`、`nova-llm`、`nova-tools`、`nova-memory`（均为 path 依赖），以及所需第三方依赖（serde, serde_json, anyhow, tokio, tracing, async-trait, tiktoken-rs, once_cell, chrono, uuid）
  - [x] 4.1.2 创建 `nova-agent/src/lib.rs` 骨架
  - [x] 4.1.3 更新根 `Cargo.toml` 添加 `nova-agent` 到 workspace members

- [x] 4.2 搬迁 Agent 相关文件到 nova-agent
  - [x] 4.2.1 将 `nova-core/src/agent/loop.rs`、`agent/context.rs`、`agent/forked.rs`、`agent/preflight.rs`（实现逻辑部分）搬入 `nova-agent/src/`
  - [x] 4.2.2 将 `nova-core/src/agent/stages/` 整个目录搬入 `nova-agent/src/stages/`
  - [x] 4.2.3 将 `nova-core/src/coordinator/` 搬入 `nova-agent/src/coordinator/`
  - [x] 4.2.4 将 `nova-core/src/subagent/` 搬入 `nova-agent/src/subagent/`
  - [x] 4.2.5 将 `nova-core/src/hooks/` 搬入 `nova-agent/src/hooks/`
  - [x] 4.2.6 将 `nova-core/src/token/` 搬入 `nova-agent/src/token/`
  - [x] 4.2.7 将 `nova-core/src/workspace/` 搬入 `nova-agent/src/workspace/`
  - [x] 4.2.8 将 `nova-core/src/heartbeat/` 搬入 `nova-agent/src/heartbeat/`

- [x] 4.3 将 delegate 工具移入 nova-agent
  - [x] 4.3.1 将 `delegate_task.rs` 和 `delegate_complex_project.rs` 从 nova-tools 移入 `nova-agent/src/delegate/`
  - [x] 4.3.2 更新 delegate 工具使用 `nova_tools::delegate_base` 中的公共逻辑
  - [x] 4.3.3 更新 delegate 工具实现 `ToolHandler` trait（使用 `ToolContext`）

- [x] 4.4 更新所有搬迁文件的引用路径
  - [x] 4.4.1 更新所有 `use crate::` 为正确的跨 crate 引用（`use nova_core::`、`use nova_llm::`、`use nova_tools::`、`use nova_memory::`）
  - [x] 4.4.2 更新 `nova-agent/src/lib.rs` 导出所有模块和关键类型（QueryLoop、TurnPipeline、PreFlightChecker、Coordinator、SubagentSpawner 等）

- [x] 4.5 验证 nova-agent 独立性
  - [x] 4.5.1 验证 `cargo build -p nova-agent` 通过
  - [x] 4.5.2 验证 `cargo test -p nova-agent` 通过

## Phase 5: 清理 nova-core + 更新 nova-daemon (Day 6 后半)

- [x] 5.1 清理 nova-core 中已搬出的源文件
  - [x] 5.1.1 删除 `nova-core/src/tools/` 目录
  - [x] 5.1.2 删除 `nova-core/src/memory/` 目录
  - [x] 5.1.3 删除 `nova-core/src/session/` 目录
  - [x] 5.1.4 删除 `nova-core/src/sidequery/` 目录
  - [x] 5.1.5 删除 `nova-core/src/agent/` 目录（pipeline.rs 已移到 nova-core 顶层，其余移到 nova-agent）
  - [x] 5.1.6 删除 `nova-core/src/coordinator/` 目录
  - [x] 5.1.7 删除 `nova-core/src/subagent/` 目录
  - [x] 5.1.8 删除 `nova-core/src/hooks/` 目录
  - [x] 5.1.9 删除 `nova-core/src/token/` 目录
  - [x] 5.1.10 删除 `nova-core/src/skills/` 目录
  - [x] 5.1.11 删除 `nova-core/src/dream/` 目录
  - [x] 5.1.12 删除 `nova-core/src/heartbeat/` 目录
  - [x] 5.1.13 删除 `nova-core/src/team/` 目录
  - [x] 5.1.14 删除 `nova-core/src/paste/` 目录
  - [x] 5.1.15 删除 `nova-core/src/sandbox/` 目录
  - [x] 5.1.16 删除 `nova-core/src/task/` 目录
  - [x] 5.1.17 删除 `nova-core/src/workspace/` 目录
  - [x] 5.1.18 删除 `nova-core/src/worktree/` 目录（如果存在）

- [x] 5.2 更新 nova-daemon 依赖
  - [x] 5.2.1 更新 `nova-daemon/Cargo.toml` 添加对 `nova-tools`、`nova-memory`、`nova-agent` 的依赖
  - [x] 5.2.2 更新 `nova-daemon/src/` 中所有 `use nova_core::tools::` 为 `use nova_tools::`
  - [x] 5.2.3 更新 `nova-daemon/src/` 中所有 `use nova_core::memory::` 为 `use nova_memory::`
  - [x] 5.2.4 更新 `nova-daemon/src/` 中所有 `use nova_core::agent::` 为 `use nova_agent::`
  - [x] 5.2.5 更新 `nova-daemon/src/` 中所有 `use nova_core::session::` 为 `use nova_memory::session::`
  - [x] 5.2.6 更新 `nova-daemon/src/` 中所有 `use nova_core::sidequery::` 为 `use nova_memory::sidequery::`
  - [x] 5.2.7 更新 tool_factory 中的 `CURRENT_CHANNEL_ID` task_local 使用为 `ToolContext` 传递
  - [x] 5.2.8 更新 `nova-daemon` 中 `ToolRegistry::execute` 调用，传入 `ToolContext`

- [x] 5.3 更新其他依赖 crate
  - [x] 5.3.1 更新 `nova-ipc/Cargo.toml` 和源文件中的引用（如有需要）
  - [x] 5.3.2 更新 `nova-tui/Cargo.toml` 和源文件中的引用（如有需要）
  - [x] 5.3.3 更新 `discord/` crate 的引用（如有需要）

## Phase 6: 最终验证 (Day 6 收尾)

- [x] 6.1 全量编译验证
  - [x] 6.1.1 运行 `cargo build --release` 确认编译通过
  - [x] 6.1.2 运行 `cargo test --workspace` 确认所有测试通过（≥78 个）

- [x] 6.2 DAG 依赖审计
  - [x] 6.2.1 检查 nova-core/Cargo.toml 无 workspace 依赖
  - [x] 6.2.2 检查 nova-llm/Cargo.toml 无 workspace 依赖
  - [x] 6.2.3 检查 nova-tools/Cargo.toml 仅依赖 nova-core 和 nova-llm
  - [x] 6.2.4 检查 nova-memory/Cargo.toml 仅依赖 nova-core 和 nova-llm
  - [x] 6.2.5 检查 nova-agent/Cargo.toml 仅依赖 nova-core、nova-llm、nova-tools、nova-memory

- [x] 6.3 编译隔离验证
  - [x] 6.3.1 运行 `cargo test -p nova-tools` 独立通过
  - [x] 6.3.2 运行 `cargo test -p nova-memory` 独立通过
  - [x] 6.3.3 运行 `cargo test -p nova-agent` 独立通过

- [x] 6.4 全局搜索验证
  - [x] 6.4.1 确认 workspace 中不存在 `task_local!` 定义 `CURRENT_CHANNEL_ID`
  - [x] 6.4.2 确认 workspace 中不存在 `CURRENT_CHANNEL_ID.try_with` 调用
  - [x] 6.4.3 确认根 Cargo.toml workspace members 恰好包含 8 个 crate
