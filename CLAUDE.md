# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build Commands

```bash
# Build all crates
cargo build --release

# Build a specific crate
cargo build -p nova-core
cargo build -p nova-daemon
cargo build -p nova-tui

# Run tests
cargo test

# Run tests for a specific crate
cargo test -p nova-core
cargo test -p nova-api

# Run clippy linter
cargo clippy --all-targets

# Run a single test
cargo test -p nova-core <test_name>
```

## Architecture Overview

NOVA is a Rust rewrite of OpenClaw Agent implementing all 16 Claude Code strategies with a cyberpunk TUI.

### Crate Structure

```
nova/
├── nova-api/        # LLM API client (Anthropic-compatible + SSE streaming)
├── nova-core/       # Core runtime: agent loop, tools, session, memory, hooks
├── nova-ipc/        # Unix Domain Socket IPC (JSON lines protocol)
├── nova-daemon/     # Daemon binary: nova run/stop
└── nova-tui/        # Cyberpunk TUI client (ratatui)
```

### Dependency Graph

```
nova-api → nova-core → nova-daemon
                     ↗
nova-ipc →──────────→ nova-tui
```

- `nova-daemon` is the main binary, depends on both `nova-core` and `nova-ipc`
- `nova-tui` connects to daemon via `/tmp/nova.sock`, depends only on `nova-ipc`

### Core Patterns

**QueryLoop** (`nova-core/src/agent/loop.rs`) is the central orchestrator:
1. Receives user message → builds API messages
2. Streams to LLM via `ApiClient` from `nova-api`
3. Parses SSE events (`StreamEvent`)
4. Executes tool calls via `ToolRegistry`
5. Applies hooks at post-sampling and stop points
6. Manages token budget and compact triggers

**Tool Registration**: Tools implement the `Tool` trait and register via `ToolRegistry::register_builtin()`. Built-in tools (bash, read_file, write_file, file_edit, glob, grep, browser, agentic_search) are registered in `nova-daemon/src/main.rs::make_tools()`.

**IPC Protocol**: `nova-ipc` uses JSON lines over Unix Domain Socket. Request/Event enums in `nova-ipc/src/protocol.rs` define the wire format.

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
| 1 | Query Loop | `agent/loop.rs` | ✅ 完整（含 v2 tracker 集成） |
| 2 | Token Budget | `token/budget.rs` | ✅ 完整 |
| 3 | Compact | `token/compact.rs` | ✅ 完整（双层熔断 + JSON 结构化） |
| 4 | Forked Agent | `agent/forked.rs` | ✅ 完整 |
| 5 | PostSampling Hooks | `hooks/post_sampling.rs` | ✅ 完整 |
| 6 | StopHooks | `hooks/stop.rs` | ✅ 完整 |
| 7 | Dual-Write Memory | `memory/dual_write.rs` | ✅ 完整 |
| 8 | Tool Pool Stable Sort | `tools/registry.rs` | ✅ 完整 |
| 9 | Team | `team/` | ✅ 完整（CRUD + 持久化） |
| 10 | Subagent spawn | `subagent/` | ✅ 完整（spawn + 并行） |
| 11 | SideQuery | `sidequery/query.rs` | ✅ 完整 |
| 12 | autoDream | `dream/engine.rs` | ✅ 完整（空闲检测 + 建议） |
| 13 | Worktree | `worktree/isolate.rs` | ✅ 完整（git worktree 管理） |
| 14 | Coordinator | `coordinator/orchestrator.rs` | ✅ 完整（4 阶段流水线） |
| 15 | Paste Store | `paste/store.rs` | ✅ 完整（hash 去重） |
| 16 | Session JSONL | `session/` | ✅ 完整 |

## NOVA v2 新增模块

| 模块 | 文件 | 说明 |
|:---|:---|:---|
| TopicTracker | `memory/topic_state.rs` | 话题状态机：Started → Active → Suspended → Archived |
| TensionTracker | `memory/tension_tracker.rs` | 张力值追踪：情绪/疲劳/意图检测 |
| ModeRouter | `memory/mode_router.rs` | 模式路由：Normal / SoftIntimate / HighIntimate / Cooling |
| MemoryBoard | `memory/memory_board.rs` | MEMORY.md 白板管理 |
| I/O Shield | `tools/constants.rs`, `tools/truncate.rs` | 工具输出物理截断 |
| `<nova_os>` | `daemon/main.rs`, `discord.rs` | 思考管道注入（过滤后不暴露给用户） |

## Running the Application

```bash
# Start daemon (background)
./target/release/nova-daemon run &

# Stop daemon
./target/release/nova-daemon stop

# Connect TUI
./target/release/nova-tui
```

## Discord Integration

The daemon embeds a Discord gateway as a tokio Task when `discord_enabled=true` and `DISCORD_TOKEN` is set. TUI and Discord share the same daemon session management.

**v2 增强**：
- `<nova_os>` 标签在发送消息前自动过滤，不会暴露给 Discord 用户
- 集成 TopicTracker、TensionTracker、ModeRouter（通过 QueryLoop）
- MemoryBoard 自动更新归档话题和偏好
