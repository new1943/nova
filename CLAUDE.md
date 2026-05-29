# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build Commands

```bash
# Build all crates (including discord)
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

NOVA is a Rust agent system implementing Claude Code strategies with a cyberpunk TUI. The system uses a daemon + client architecture communicating via Unix Domain Socket IPC.

### Crate Structure (9 workspace crates)

```
nova/
├── nova-core/       # Core traits and types: LlmBackend, AgentPolicy, MemoryLayer, ToolExecutor, Config, Message, ShadowEvent
├── nova-llm/        # LLM API client: Anthropic-compatible SSE streaming, implements LlmBackend trait
├── nova-tools/      # Tool system: ToolRegistry, built-in tools (bash, read_file, write_file, glob, grep, browser, skills, executor_tools)
├── nova-memory/     # Memory system: TopicTracker, TensionTracker, ModeRouter, SessionManager, MemoryBoard, DailyNotes
├── nova-agent/      # Agent logic: QueryLoop, TurnPipeline, Token management, Hooks, ForkedAgent, Heartbeat, MemoryLayer impls
├── nova-ipc/        # IPC: Unix Domain Socket, JSON lines protocol
├── nova-daemon/     # Daemon: main.rs, session_handler, lifecycle, Dispatcher, Discord gateway, TaskManager, ToolFactory
├── nova-tui/        # Cyberpunk TUI client (ratatui + crossterm)
└── discord/         # Standalone Discord client: connects to daemon via nova-ipc
```

### Dependency Graph

```
                    nova-core (foundation)
                   /    |     \      \
              nova-llm  |   nova-ipc  nova-memory
                 |      |      |        |
              nova-tools |     |        |
                 \      |     |        /
                  nova-agent  |       /
                      \       |      /
                       nova-daemon
                      /       |
                   nova-tui  discord
```

**Dependencies:**
- `nova-core`: zero internal deps, defines all core traits and types
- `nova-llm`: depends on `nova-core` (CompletionRequest/Response types)
- `nova-memory`: depends on `nova-core` (Message, ShadowEvent types)
- `nova-tools`: depends on `nova-core` + `nova-llm`
- `nova-agent`: depends on `nova-core` + `nova-llm` + `nova-tools` + `nova-memory`
- `nova-ipc`: zero internal deps (standalone protocol layer)
- `nova-tui`: depends on `nova-ipc` (connects to daemon via UDS)
- `nova-daemon`: depends on all crates (top-level assembly)
- `discord`: depends on `nova-ipc` (connects to daemon via IPC)

### Core Traits (nova-core)

| Trait | File | Purpose | Implementations |
|:------|:-----|:--------|:----------------|
| `LlmBackend` | `llm_backend.rs` | LLM API abstraction | `ApiClient` (nova-llm) |
| `AgentPolicy` | `policy.rs` | Tool visibility + behavior per turn | `PassthroughPolicy` (default) |
| `MemoryLayer` | `memory_layer.rs` | Unified memory interface | `EpisodicMemory`, `ConsolidationMemory` (nova-agent) |
| `ToolHandler` | `executor/mod.rs` | Tool execution | `ToolRegistry` (nova-tools) |
| `PlatformAdapter` | `platform.rs` | Platform abstraction | `DiscordAdapter` (defined, not wired) |

### TurnPipeline (nova-agent)

The agent loop uses a `TurnPipeline` orchestrator with 5 composable stages:

| Stage | File | Responsibility |
|:------|:-----|:---------------|
| **PolicyStage** | `pipeline/stages/policy.rs` | Tool visibility + termination via `AgentPolicy::decide()` |
| **InjectStage** | `pipeline/stages/inject.rs` | Assembles system prompt with prompt injections |
| **BudgetStage** | `pipeline/stages/budget.rs` | Pre-flight token budget check |
| **StreamStage** | `pipeline/stages/stream.rs` | LLM API call + streaming response |
| **ExecuteStage** | `pipeline/stages/execute.rs` | Tool execution with approval checks |

`QueryLoop::run_turn()` creates a `TurnPipeline` once, then loops: each iteration runs all 5 stages via `pipeline.run()`, followed by post-pipeline control flow (compact, retry, overflow handling). A single `TurnContext` is reused across iterations via `reset_iteration()`.

### Memory Subsystems (nova-daemon)

The daemon initializes 5 memory subsystems per connection:

| Subsystem | Module | Purpose |
|:----------|:-------|:--------|
| **TensionTracker** | `nova-memory/src/memory/tension_tracker.rs` | Emotion/tension detection from user messages |
| **ModeRouter** | `nova-memory/src/memory/mode_router.rs` | Interaction mode (Normal/SoftIntimate/HighIntimate/Cooling) |
| **TopicTracker** | `nova-memory/src/memory/topic_state.rs` | Topic lifecycle (Started→Active→Suspended→Archived) |
| **MemoryBoard** | `nova-memory/src/memory/memory_board.rs` | MEMORY.md whiteboard management |
| **MemoryRecall** | `nova-memory/src/memory/recall.rs` | Diary-based relevant memory recall |

Mode hints are injected into the system prompt per turn. Topic transitions are tracked and archived to MemoryBoard.

### Executor System

The executor system provides 5 execution modes for different task complexities:

| Mode | Module | Description |
|:-----|:-------|:------------|
| **React** | `nova-core/src/executor/react.rs` | ReAct reasoning + action loop |
| **Chain** | `nova-core/src/executor/chain.rs` | Sequential step chain |
| **Parallel** | `nova-core/src/executor/parallel.rs` | Parallel sub-tasks |
| **WithReview** | `nova-core/src/executor/review.rs` | Adversarial self-review |
| **Project** | `nova-core/src/executor/project.rs` | 4-phase orchestrator (Research→Synthesis→Implementation→Verification) |

These are exposed as tools via `nova-tools/src/executor_tools.rs` and managed by `TaskRegistry` (global singleton).

### Daemon Structure (nova-daemon)

| File | Lines | Responsibility |
|:-----|:------|:---------------|
| `main.rs` | ~300 | Entry point, daemon lifecycle, IPC server loop, Discord gateway setup |
| `session_handler.rs` | ~560 | Per-connection session management, QueryLoop orchestration, memory subsystems |
| `lifecycle.rs` | ~45 | PID file management, PidGuard |
| `tool_factory.rs` | ~110 | ToolRegistry construction |
| `dispatcher.rs` | ~180 | ShadowEvent routing |
| `discord.rs` | ~890 | Discord gateway handler |

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

## Strategy Implementation

| # | Strategy | Module | Status |
|:--|:--|:--|:--|
| 1 | Query Loop | `nova-agent/src/agent_loop.rs` | Done |
| 2 | Token Budget | `nova-agent/src/token/budget.rs` | Done |
| 3 | Compact | `nova-agent/src/token/compact.rs` | Done |
| 4 | Forked Agent | `nova-agent/src/forked.rs` | Defined (not wired) |
| 5 | PostSampling Hooks | `nova-agent/src/hooks/post_sampling.rs` | Done |
| 6 | StopHooks | `nova-agent/src/hooks/stop.rs` | Done |
| 7 | Dual-Write Memory | `nova-memory/src/memory/dual_write.rs` | Done |
| 8 | Tool Pool Stable Sort | `nova-tools/src/registry.rs` | Done |
| 9 | Executor Tools | `nova-tools/src/executor_tools.rs` | Done |
| 10 | SideQuery | `nova-memory/src/sidequery/query.rs` | Done |
| 11 | Worktree | `nova-tools/src/worktree/` | Done |
| 12 | Paste Store | `nova-tools/src/paste/` | Defined (not wired) |
| 13 | Session JSONL | `nova-memory/src/session/` | Done |
| 14 | TurnPipeline | `nova-agent/src/pipeline/` | Done (5 stages) |
| 15 | MemoryLayer | `nova-agent/src/memory/` | Done (Episodic + Consolidation) |
| 16 | Memory Subsystems | `nova-memory/src/memory/` | Done (5 subsystems wired) |

## Not Yet Integrated

These modules are defined and tested but not wired into the production pipeline:

- `nova-core/src/retry/policy.rs` — `RetryPolicy` with exponential backoff, planned for LLM API retry
- `nova-core/src/injection_scanner.rs` — `InjectionScanner` for prompt injection detection, has property tests
- `nova-core/src/platform.rs` — `PlatformAdapter` trait for unified platform abstraction, `DiscordAdapter` exists but unused
- `nova-agent/src/forked.rs` — `ForkedAgent` for background task spawning with retry, defined but unused
- `nova-tools/src/paste/` — `PasteStore` for content deduplication by hash, defined but unused
- `nova-tools/src/sandbox/` — `SandboxPolicy` for file access control, defined but unused

## ShadowEvent System

`nova-core/src/models/events.rs` defines the event bus:

| Event | Frequency | Target | Purpose |
|:------|:----------|:-------|:--------|
| `TaskProgress` | High | TaskManager | Tasks.md CRUD |
| `TopicArchived` | Low | MemoryKeeper | Topic archiving → memory extraction |
| `SystemIdle` | Low | MemoryKeeper | Idle-time flush |
| `ProjectCompleted` | Low | Dispatcher | Push notification |

Event flow: `QueryLoop / Heartbeat → mpsc channel → Dispatcher → handlers`

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

Two ways to use Discord:

1. **Embedded** — daemon starts Discord gateway when `discord_enabled=true` and `DISCORD_TOKEN` is set
2. **Standalone** — run `discord/` binary separately, connects to daemon via IPC

TUI and Discord share the same daemon session management.

- Discord slash commands: `/new`, `/reset`, `/stop`, `/tasks`
- Background task completion notifications are pushed to Discord channels
