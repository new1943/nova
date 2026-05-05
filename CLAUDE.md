# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build Commands

```bash
# Build all crates
cargo build --release

# Build a specific crate
cargo build -p nova-core
cargo build -p nova-llm
cargo build -p nova-tools
cargo build -p nova-memory
cargo build -p nova-agent
cargo build -p nova-ipc
cargo build -p nova-daemon
cargo build -p nova-discord

# Run tests
cargo test

# Run tests for a specific crate
cargo test -p nova-core
cargo test -p nova-llm
cargo test -p nova-agent

# Run clippy linter
cargo clippy --all-targets

# Run a single test
cargo test -p nova-core <test_name>
```

## Architecture Overview

NOVA is a Rust rewrite of OpenClaw Agent implementing all 16 Claude Code strategies with a cyberpunk TUI. The system uses a TurnPipeline architecture for per-turn processing, trait-based abstractions for LLM and platform connectivity, and an event-driven background system (ShadowEvent).

### Crate Structure (8 workspace crates + 1 standalone)

```
nova/
├── nova-core/       # 核心类型与 trait：TurnContext, PipelineStage, LlmBackend, PlatformAdapter, Config, Message, ShadowEvent
├── nova-llm/        # LLM API 客户端：Anthropic 兼容 SSE 流式，实现 LlmBackend trait
├── nova-tools/      # 工具系统：ToolRegistry, 内置工具（bash, read_file, write_file, glob, grep, browser, skills...）
├── nova-memory/     # 记忆系统：四层记忆架构, TopicTracker, TensionTracker, ModeRouter, SessionManager, DreamEngine
├── nova-agent/      # Agent 逻辑：AgentLoop, TurnPipeline Stages, Token 管理, Coordinator, SubAgent, Heartbeat
├── nova-ipc/        # 进程间通信：Unix Domain Socket, JSON lines 协议
├── nova-daemon/     # 守护进程：Dispatcher 事件路由, Discord 适配器, TaskManager, ToolFactory
├── nova-tui/        # 赛博朋克 TUI 客户端（ratatui + crossterm）
└── discord/         # (standalone) Discord 独立客户端：通过 nova-ipc 连接 daemon
```

### Dependency Graph

```
                    nova-core (基础层)
                   /    |     \      \
              nova-llm  |   nova-ipc  nova-memory
                 |      |      |  \      |
              nova-tools |     |   \     |
                 \      |     |    \    /
                  nova-agent  |   nova-tui
                      \       |
                       nova-daemon
                           
                        discord ──→ nova-ipc (standalone)
```

**依赖关系说明：**
- `nova-core`：零内部依赖，定义所有核心 trait 和类型
- `nova-llm`：依赖 `nova-core`（使用 CompletionRequest/Response 类型）
- `nova-memory`：依赖 `nova-core`（使用 Message, ShadowEvent 类型）
- `nova-tools`：依赖 `nova-core` + `nova-llm`
- `nova-agent`：依赖 `nova-core` + `nova-llm` + `nova-tools` + `nova-memory`
- `nova-ipc`：零内部依赖（独立协议层）
- `nova-tui`：依赖 `nova-ipc`（通过 UDS 连接 daemon）
- `nova-daemon`：依赖所有 crate（顶层组装）
- `discord`：(standalone) 仅依赖 `nova-ipc`（通过 IPC 连接 daemon）

### TurnPipeline Stage 说明

TurnPipeline 是每轮对话的处理流水线，各 Stage 通过共享的 `TurnContext` 协作：

```
Classify → Track → Gate → Inject → PlatformHint → ExecuteConfig
```

| Stage | 职责 | 写入字段 |
|:------|:-----|:---------|
| **ClassifyStage** | 调用 PreFlight 分类用户输入复杂度 | `complexity`, `preflight_result` |
| **TrackStage** | 检测话题切换、计算张力值 | `topic_shift`, `tension` |
| **GateStage** | 根据复杂度决定工具白名单 | `allowed_tools` |
| **InjectStage** | 注入上下文（记忆、Skills 摘要、auto_trigger） | `prompt_injections` |
| **PlatformHintStage** | 根据平台（Discord/TUI）注入平台特定提示 | `prompt_injections` |
| **ExecuteConfigStage** | 设置执行配置（如高复杂度时 terminate_after_tool） | `should_terminate_after_tool` |

**核心规则：** 每个 Stage "写自己负责的字段，读别人的字段"，Stage 之间不直接通信。

### LlmBackend Trait

`nova-core/src/llm_backend.rs` 定义了 LLM 后端抽象，使系统可对接任意 LLM provider：

```rust
#[async_trait]
pub trait LlmBackend: Send + Sync {
    /// 非流式补全
    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse>;
    /// 流式补全 — 通过 channel 发送增量事件
    async fn stream(&self, req: &CompletionRequest, tx: Sender<StreamDelta>) -> Result<()>;
}
```

- `nova-llm` 提供 Anthropic 兼容的 HTTP 实现
- 测试中使用 mock 实现避免真实 API 调用
- `CompletionRequest` / `CompletionResponse` 是 provider 无关的统一类型

### PlatformAdapter Trait

`nova-core/src/platform.rs` 定义了平台适配器抽象，支持多平台接入：

```rust
#[async_trait]
pub trait PlatformAdapter: Send + Sync {
    fn platform(&self) -> Platform;
    async fn send(&self, channel_id: &str, content: &str) -> Result<()>;
    async fn start(&mut self, tx: Sender<PlatformMessage>) -> Result<()>;
}
```

- `Platform` 枚举：`Discord` | `Tui`
- `nova-daemon` 中的 `DiscordAdapter` 实现此 trait
- 统一的 `PlatformMessage` 结构上报消息给 Agent

## Configuration

Config file: `~/.nova/config` (TOML, top-level key-value pairs)

Key settings:
- `api_key`, `model`, `api_base_url` — LLM endpoint
- `context_window` — model context size (default 200000)
- `max_turns` — max turns per session (default 20)
- `tool_timeout_secs` — tool execution timeout (default 60)
- `compact_target_pct`, `budget_trigger_pct` — token budget thresholds

Workspace directory: `~/.nova/` containing:
- SOUL.md, IDENTITY.md, AGENTS.md, USER.md, STATE.md, TASKS.md — loaded into system prompt
- MEMORY.md — long-term memory (not injected, accessed via tools)
- sessions/ — JSONL session storage

## Claude Code 16 Strategy Implementation

| # | Strategy | Module | Status |
|:--|:--|:--|:--|
| 1 | Query Loop | `nova-agent/src/agent_loop.rs` | ✅ |
| 2 | Token Budget | `nova-agent/src/token/budget.rs` | ✅ |
| 3 | Compact | `nova-agent/src/token/compact.rs` | ✅ |
| 4 | Forked Agent | `nova-agent/src/forked.rs` | ✅ |
| 5 | PostSampling Hooks | `nova-agent/src/hooks/post_sampling.rs` | ✅ |
| 6 | StopHooks | `nova-agent/src/hooks/stop.rs` | ✅ |
| 7 | Dual-Write Memory | `nova-memory/src/memory/dual_write.rs` | ✅ |
| 8 | Tool Pool Stable Sort | `nova-tools/src/registry.rs` | ✅ |
| 9 | Team | `nova-tools/src/team/` | ✅ |
| 10 | Subagent spawn | `nova-agent/src/subagent/` | ✅ |
| 11 | SideQuery | `nova-memory/src/sidequery/query.rs` | ✅ |
| 12 | autoDream | `nova-memory/src/dream/engine.rs` | ✅ |
| 13 | Worktree | `nova-tools/src/worktree/` | ✅ |
| 14 | Coordinator | `nova-agent/src/coordinator/orchestrator.rs` | ✅ |
| 15 | Paste Store | `nova-tools/src/paste/` | ✅ |
| 16 | Session JSONL | `nova-memory/src/session/` | ✅ |

## ShadowEvent System

`nova-core/src/models/events.rs` 定义了统一事件总线：

| Event | 频率 | 路由目标 | 用途 |
|:------|:-----|:---------|:-----|
| `TaskProgress` | 高频 | TaskManager | Tasks.md CRUD |
| `TopicArchived` | 低频 | MemoryKeeper | 话题归档 → 记忆提取 |
| `SystemIdle` | 低频 | MemoryKeeper | 空闲时 flush 缓冲 |
| `ProjectCompleted` | 低频 | Dispatcher | 推送通知 |

事件流：`QueryLoop / Heartbeat → mpsc channel → Dispatcher → handlers`

## Running the Application

```bash
# Start daemon (background)
./target/release/nova-daemon run &

# Stop daemon
./target/release/nova-daemon stop

# Connect TUI (if nova-tui is built)
./target/release/nova-tui
```

## Discord Integration

The daemon embeds a Discord gateway as a tokio Task when `discord_enabled=true` and `DISCORD_TOKEN` is set. TUI and Discord share the same daemon session management.

- `<nova_os>` 标签在发送消息前自动过滤，不会暴露给 Discord 用户
- 集成 TopicTracker、TensionTracker、ModeRouter（通过 Pipeline）
- MemoryBoard 自动更新归档话题和偏好
