# Requirements Document

## Introduction

本文档定义 Nova V2 Release R2 "Crate 拆分" 的需求。R2 的目标是将当前 5 个 crate 的 workspace 重构为 8 个 crate，实现增量编译从分钟级降至秒级，并使各模块可独立测试。

R1 已完成 TurnPipeline 闭环（Classify→Track→Gate→Inject→ExecuteConfig）、main.rs 瘦化、技术债清理，78 个测试通过。R2 在此基础上进行 crate 级别的模块拆分。

### 当前 Workspace 结构（5 crates）

- `nova-core` — 主库，包含 agent、memory、tools、session、sidequery、coordinator 等所有核心逻辑
- `nova-api` — LLM API 客户端
- `nova-ipc` — IPC 协议
- `nova-daemon` — 守护进程入口（binary）
- `nova-tui` — TUI 客户端

### 目标 Workspace 结构（8 crates）

- `nova-core` — 共享类型 + trait 定义（提纯后）
- `nova-llm` — 原 `nova-api` 重命名
- `nova-tools` — 工具系统
- `nova-memory` — 记忆系统
- `nova-agent` — Agent 核心（pipeline、stages、loop）
- `nova-ipc` — IPC 协议（不变）
- `nova-daemon` — 守护进程入口（不变）
- `nova-tui` — TUI 客户端（不变）

## Glossary

- **Workspace**: Cargo workspace，包含多个 crate 的 Rust 项目组织方式
- **Nova_Core**: 提纯后的共享类型 crate，仅包含 `Message`、`Role`、`ToolCall`、`ShadowEvent`、`NovaConfig`、`TurnContext`、`PipelineStage` trait 等核心类型和 trait 定义
- **Nova_LLM**: 原 `nova-api` 重命名后的 LLM API 客户端 crate
- **Nova_Tools**: 独立的工具系统 crate，包含所有 Tool 实现和 `ToolRegistry`
- **Nova_Memory**: 独立的记忆系统 crate，包含 memory、sidequery、session 模块
- **Nova_Agent**: 独立的 Agent 核心 crate，包含 `TurnPipeline` 实现、stages、`QueryLoop`、`PreFlightChecker`、`Coordinator`、`SubagentSpawner`
- **Tool_Trait**: 当前 `nova-core/src/tools/registry.rs` 中定义的 `Tool` trait，工具的统一接口
- **ToolHandler_Trait**: 替代 `Tool` trait 的新 trait，增加 `ToolContext` 参数以消除全局状态依赖
- **ToolContext**: 传递给 `ToolHandler` 的上下文结构体，包含 `channel_id`、`workspace_dir` 等运行时信息
- **DAG**: 有向无环图，描述 crate 之间的依赖关系
- **CURRENT_CHANNEL_ID**: 当前 `nova-core/src/tools/mod.rs` 中通过 `task_local!` 宏定义的全局线程局部变量，用于传递 channel ID，属于需要消除的技术债
- **Delegate_Base**: 从 `delegate_task.rs` 和 `delegate_complex_project.rs` 中提取的公共基础模块

## Requirements

### Requirement 1: Nova_Core 提纯为共享类型 Crate

**User Story:** 作为开发者，我希望 Nova_Core 仅包含共享类型和 trait 定义，以便其他 crate 可以依赖一个轻量的核心库而不引入不必要的实现代码。

#### Acceptance Criteria

1. THE Nova_Core SHALL 仅导出以下模块：`message`（Message, Role, ToolCall）、`models/events`（ShadowEvent, TaskAction）、`config`（NovaConfig）、`agent/pipeline`（TurnContext, PipelineStage trait, PromptInjection, DecisionEntry）以及核心 trait 定义文件
2. WHEN Nova_Core 被其他 crate 依赖时，THE Nova_Core SHALL 不包含任何工具实现、记忆实现或 Agent 循环实现代码
3. THE Nova_Core SHALL 编译时间不超过 5 秒（clean build 除外）
4. WHEN 开发者修改 Nova_Tools 中的代码时，THE Workspace SHALL 不触发 Nova_Core 的重新编译

### Requirement 2: Nova_API 重命名为 Nova_LLM

**User Story:** 作为开发者，我希望 LLM API 客户端 crate 的名称能准确反映其职责，以便在 8 crate 架构中清晰辨识。

#### Acceptance Criteria

1. THE Workspace SHALL 将 `nova-api` crate 重命名为 `nova-llm`，包括目录名、`Cargo.toml` 中的 `name` 字段以及所有 `use` / `extern crate` 引用
2. WHEN 重命名完成后，THE Workspace SHALL 通过 `cargo build --release` 编译
3. THE Nova_LLM SHALL 保持与原 `nova-api` 完全相同的公开 API（`client`、`types`、`stream` 模块）

### Requirement 3: Nova_Tools 独立 Crate

**User Story:** 作为开发者，我希望工具系统作为独立 crate 存在，以便修改工具代码时只需重编译 Nova_Tools 而不影响 Nova_Memory 或 Nova_Agent。

#### Acceptance Criteria

1. THE Nova_Tools SHALL 包含当前 `nova-core/src/tools/` 目录下的所有工具实现文件（bash、read_file、write_file、file_edit、glob、grep、browser、agentic_search、file_tracker、worktree、agent、team、delegate_complex_project、delegate_task 及其子模块）
2. THE Nova_Tools SHALL 包含 `ToolRegistry` 和 `Tool` trait 定义（或新的 ToolHandler_Trait）
3. THE Nova_Tools SHALL 仅依赖 Nova_Core 和 Nova_LLM，不依赖 Nova_Memory 或 Nova_Agent
4. WHEN 开发者运行 `cargo test -p nova-tools` 时，THE Nova_Tools SHALL 独立通过所有工具相关测试
5. WHEN 开发者修改 Nova_Tools 中的代码时，THE Workspace SHALL 不触发 Nova_Memory 的重新编译

### Requirement 4: ToolHandler Trait 替代 Tool Trait

**User Story:** 作为开发者，我希望工具接口通过显式参数传递上下文，以便消除 `task_local!` 全局状态 hack 并提高可测试性。

#### Acceptance Criteria

1. THE Nova_Tools SHALL 定义 `ToolHandler` trait，其 `execute` 方法签名包含 `ToolContext` 参数：`async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<String>`
2. THE ToolContext SHALL 至少包含 `channel_id: String` 和 `workspace_dir: Option<PathBuf>` 字段
3. WHEN ToolHandler_Trait 就位后，THE Nova_Tools SHALL 不再包含 `task_local! CURRENT_CHANNEL_ID` 宏调用
4. WHEN 所有工具迁移到 ToolHandler_Trait 后，THE Nova_Tools SHALL 中每个工具的 `execute` 方法通过 `ctx` 参数获取 `channel_id`，而非通过 `CURRENT_CHANNEL_ID.try_with()`

### Requirement 5: Delegate 工具代码去重

**User Story:** 作为开发者，我希望 `delegate_task` 和 `delegate_complex_project` 的公共逻辑被提取到共享基础模块，以便减少代码重复并统一维护。

#### Acceptance Criteria

1. THE Nova_Tools SHALL 包含 `delegate_base` 模块，提取 `delegate_task.rs` 和 `delegate_complex_project.rs` 中的公共逻辑（包括：RUNNING_PROJECTS 注册表管理、任务 ID 生成、TaskLogger 调用、system_prompt 动态加载、ShadowEvent::ProjectCompleted 发射、channel_id 获取）
2. WHEN `delegate_task` 执行时，THE Delegate_Base SHALL 处理后台任务生命周期管理（spawn、注册 AbortHandle、完成后清理、TaskLogger 更新）
3. WHEN `delegate_complex_project` 执行时，THE Delegate_Base SHALL 处理相同的后台任务生命周期管理逻辑
4. THE `delegate_task.rs` 和 `delegate_complex_project.rs` SHALL 各自仅包含差异化逻辑（delegate_task 使用 SubagentSpawner，delegate_complex_project 使用 Coordinator）

### Requirement 6: Nova_Memory 独立 Crate

**User Story:** 作为开发者，我希望记忆系统作为独立 crate 存在，以便修改记忆代码时只需重编译 Nova_Memory，并可独立运行记忆相关测试。

#### Acceptance Criteria

1. THE Nova_Memory SHALL 包含当前 `nova-core/src/memory/` 目录下的所有文件（dual_write、store、daily、recall、dream、consolidate、topic_state、tension_tracker、mode_router、memory_board）
2. THE Nova_Memory SHALL 包含当前 `nova-core/src/sidequery/` 目录下的所有文件（query、memory_keeper）
3. THE Nova_Memory SHALL 包含当前 `nova-core/src/session/` 目录下的所有文件（manager、history、search）
4. THE Nova_Memory SHALL 仅依赖 Nova_Core 和 Nova_LLM，不依赖 Nova_Tools 或 Nova_Agent
5. WHEN 开发者运行 `cargo test -p nova-memory` 时，THE Nova_Memory SHALL 独立通过所有记忆相关测试
6. WHEN 开发者修改 Nova_Memory 中的代码时，THE Workspace SHALL 不触发 Nova_Tools 的重新编译

### Requirement 7: Nova_Agent 独立 Crate

**User Story:** 作为开发者，我希望 Agent 核心逻辑作为独立 crate 存在，以便 Agent 的 pipeline、loop、coordinator 等逻辑可独立编译和测试。

#### Acceptance Criteria

1. THE Nova_Agent SHALL 包含以下模块：`pipeline`（TurnPipeline 实现）、`stages/`（ClassifyStage、TrackStage、GateStage、InjectStage、ExecuteConfigStage）、`loop`（QueryLoop）、`preflight`（PreFlightChecker）、`coordinator`（Coordinator 4 阶段流水线）、`subagent`（SubagentSpawner）
2. THE Nova_Agent SHALL 依赖 Nova_Core、Nova_LLM、Nova_Tools 和 Nova_Memory
3. WHEN 开发者运行 `cargo test -p nova-agent` 时，THE Nova_Agent SHALL 独立通过所有 Agent 相关测试
4. THE Nova_Agent SHALL 导出 `QueryLoop`、`TurnPipeline`、`PreFlightChecker`、`Coordinator` 等公开类型供 Nova_Daemon 使用

### Requirement 8: Workspace DAG 依赖正确性

**User Story:** 作为开发者，我希望 8 个 crate 之间的依赖关系形成清晰的 DAG，以便确保无循环依赖且增量编译路径最优。

#### Acceptance Criteria

1. THE Workspace SHALL 包含以下 8 个 crate member：`nova-core`、`nova-llm`、`nova-tools`、`nova-memory`、`nova-agent`、`nova-ipc`、`nova-daemon`、`nova-tui`
2. THE Workspace 依赖 DAG SHALL 满足以下约束：Nova_Core 不依赖任何其他 workspace crate；Nova_LLM 不依赖任何其他 workspace crate；Nova_Tools 仅依赖 Nova_Core 和 Nova_LLM；Nova_Memory 仅依赖 Nova_Core 和 Nova_LLM；Nova_Agent 依赖 Nova_Core、Nova_LLM、Nova_Tools 和 Nova_Memory；Nova_Daemon 依赖 Nova_Core、Nova_LLM、Nova_Tools、Nova_Memory 和 Nova_Agent
3. WHEN 运行 `cargo build --release` 时，THE Workspace SHALL 编译成功且无循环依赖错误
4. IF 开发者引入了违反 DAG 约束的依赖，THEN Cargo SHALL 在编译时报告循环依赖错误

### Requirement 9: 编译隔离验证

**User Story:** 作为开发者，我希望验证 crate 拆分后的编译隔离效果，以确保修改一个模块不会触发不相关模块的重编译。

#### Acceptance Criteria

1. WHEN 开发者修改 Nova_Tools 中的源文件后运行 `cargo build` 时，THE Workspace SHALL 不重新编译 Nova_Memory
2. WHEN 开发者修改 Nova_Memory 中的源文件后运行 `cargo build` 时，THE Workspace SHALL 不重新编译 Nova_Tools
3. WHEN 开发者修改 Nova_Agent 中的源文件后运行 `cargo build` 时，THE Workspace SHALL 不重新编译 Nova_Tools 或 Nova_Memory
4. WHEN 开发者修改 Nova_Core 中的源文件后运行 `cargo build` 时，THE Workspace SHALL 重新编译所有依赖 Nova_Core 的 crate

### Requirement 10: 功能不退化

**User Story:** 作为开发者，我希望 crate 拆分后所有现有功能保持不变，以确保 TUI 和 Discord 的用户体验不受影响。

#### Acceptance Criteria

1. WHEN crate 拆分完成后，THE Workspace SHALL 通过 `cargo build --release` 编译成功
2. WHEN crate 拆分完成后，THE Workspace SHALL 通过所有现有的 78 个测试（`cargo test --workspace`）
3. WHEN 用户通过 TUI 发送消息时，THE Nova_Daemon SHALL 正常处理对话流程（Classify→Track→Gate→Inject→ExecuteConfig pipeline 正常运行）
4. WHEN 用户通过 Discord 发送消息时，THE Nova_Daemon SHALL 正常处理对话流程并返回响应
5. WHEN 用户触发 delegate_task 或 delegate_complex_project 时，THE Nova_Tools SHALL 正常执行后台任务委派
