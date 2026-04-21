# NOVA 架构设计

**版本**: v3.1
**日期**: 2026-04-20
**状态**: Phase 1 MVP + Phase 1 (物理防御层) + Phase 1.5 + Phase 2 已完成

---

## 1. Cargo Workspace 结构（实际）

```
nova/
├── Cargo.toml                  # workspace 根（5 个 member）
├── nova-api/                   # LLM API 客户端（lib crate）
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs              # pub mod 导出
│       ├── client.rs           # ApiClient — HTTP + SSE streaming
│       ├── types.rs            # API 请求/响应/SSE 类型
│       └── stream.rs           # StreamEvent + AccumulatedToolCall
├── nova-core/                  # 核心运行时库（lib crate）
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs              # 19 个 pub mod 导出
│       ├── config.rs           # NovaConfig — TOML 配置加载
│       ├── message.rs          # Message / Role / ToolCall
│       ├── agent/
│       │   ├── mod.rs
│       │   ├── loop.rs         # QueryLoop — 策略 1 核心循环
│       │   ├── prompt.rs       # PromptBuilder — system prompt 拼接
│       │   └── forked.rs       # ForkedAgent — 策略 4
│       ├── token/
│       │   ├── mod.rs
│       │   ├── budget.rs       # TokenBudget — 策略 2 双阈值
│       │   └── compact.rs      # Compactor — 策略 3 对话压缩
│       ├── hooks/
│       │   ├── mod.rs          # HookManager — 注册 + 触发
│       │   ├── post_sampling.rs # MemoryExtractHook — 策略 5
│       │   └── stop.rs         # MemoryExtractStopHook — 策略 6
│       ├── memory/
│       │   ├── mod.rs
│       │   ├── dual_write.rs   # DualWriteMemory — 策略 7
│       │   ├── store.rs        # MemoryStore — 通用记忆存储
│       │   └── daily.rs        # DailyNotes — 每日笔记
│       ├── tools/
│       │   ├── mod.rs          # Tool trait 定义
│       │   ├── registry.rs     # ToolRegistry — 策略 8 稳定排序
│       │   ├── bash.rs         # BashTool — 受限模式 shell
│       │   ├── read_file.rs    # ReadFileTool
│       │   ├── write_file.rs   # WriteFileTool
│       │   └── glob.rs         # GlobTool
│       ├── session/
│       │   ├── mod.rs
│       │   ├── manager.rs      # SessionManager + Session — 策略 16
│       │   └── history.rs      # SessionHistory — JSONL 追加
│       ├── workspace/
│       │   ├── mod.rs
│       │   └── loader.rs       # WorkspaceLoader — 9 个 .md 文件
│       ├── team/               # 策略 9 完整
│       │   ├── mod.rs
│       │   ├── config.rs       # TeamManager / Team / Task
│       │   └── mailbox.rs      # Mailbox — per-agent 消息
│       ├── subagent/           # 策略 10 完整
│       │   ├── mod.rs
│       │   └── spawn.rs        # SubagentSpawner / SubagentConfig
│       ├── sidequery/          # 策略 11 完整
│       │   ├── mod.rs
│       │   └── query.rs        # SideQuery
│       ├── dream/              # 策略 12 完整
│       │   ├── mod.rs
│       │   └── engine.rs       # DreamEngine
│       ├── worktree/           # 策略 13 完整
│       │   ├── mod.rs
│       │   └── isolate.rs      # WorktreeManager / Worktree
│       ├── coordinator/        # 策略 14 完整
│       │   ├── mod.rs
│       │   └── orchestrator.rs # Coordinator / CoordinatorPhase
│       ├── paste/              # 策略 15 完整
│       │   ├── mod.rs
│       │   └── store.rs        # PasteStore — hash 去重
│       ├── heartbeat/
│       │   ├── mod.rs
│       │   └── scheduler.rs    # HeartbeatScheduler
│       ├── skills/
│       │   ├── mod.rs
│       │   └── loader.rs       # SkillsLoader / Skill / AutoTrigger
│       ├── sandbox/
│       │   ├── mod.rs
│       │   └── policy.rs       # SandboxPolicy / SandboxLevel
│       └── retry/
│           ├── mod.rs
│           └── policy.rs       # RetryPolicy — 指数退避
├── nova-ipc/                   # 进程间通信（lib crate）
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs              # pub use 导出
│       ├── protocol.rs         # Request / Event 枚举
│       ├── server.rs           # IpcServer / IpcConnection
│       └── client.rs           # IpcClient
├── nova-daemon/                # 守护进程（bin crate）
│   ├── Cargo.toml
│   └── src/
│       └── main.rs             # run_daemon / handle_connection
└── nova-tui/                   # TUI 客户端（bin crate）
    ├── Cargo.toml
    └── src/
        ├── main.rs             # run_app — 主渲染循环
        ├── app.rs              # App 状态 / DisplayMessage
        ├── ui.rs               # render — 三段式布局
        ├── input.rs            # handle_input — 键盘处理
        └── theme.rs            # Theme — 赛博朋克配色
```

### Crate 依赖关系

```
nova-api  ←── nova-core ←── nova-daemon
                                ↑
nova-ipc ←──────────────── nova-daemon
nova-ipc ←──────────────── nova-tui
```

- `nova-api`：无内部依赖（reqwest, serde, futures）
- `nova-core`：依赖 `nova-api`
- `nova-ipc`：无内部依赖（tokio, serde）
- `nova-daemon`：依赖 `nova-core` + `nova-ipc`
- `nova-tui`：依赖 `nova-ipc`（不依赖 nova-core）

---

## 2. 系统架构

### 2.1 双组件架构

```
┌──────────────────────────────────────────────┐
│  nova-daemon (./nova-daemon run &)           │
│  ┌────────────┬────────────┬──────────────┐ │
│  │ QueryLoop  │ Heartbeat  │ Hooks        │ │
│  ├────────────┴────────────┴──────────────┤ │
│  │ Session │ Memory │ Tools │ Workspace   │ │
│  ├────────────────────────────────────────┤ │
│  │ IpcServer (Unix Socket) │ Discord API  │ │
│  └─────────────────────────┴──────────────┘ │
└──────────────────────────────────────────────┘
           ↕ /tmp/nova.sock (JSON lines)
┌──────────────────────────────────────────────┐
│  nova-tui (./nova-tui)                       │
│  ┌────────────────────────────────────────┐ │
│  │ ratatui 渲染 │ 输入处理 │ IpcClient    │ │
│  └────────────────────────────────────────┘ │
└──────────────────────────────────────────────┘
```

### 2.2 IPC 协议

传输格式：**JSON lines**（每条消息一行 JSON + `\n`）

```rust
// nova-ipc/src/protocol.rs

/// TUI → Daemon 请求
#[serde(tag = "type")]
pub enum Request {
    UserMessage { content: String },
    ResumeSession,
    NewSession,
    Shutdown,
}

/// Daemon → TUI 事件（流式）
#[serde(tag = "type")]
pub enum Event {
    TextDelta { content: String },
    ToolCallStart { id: String, name: String },
    ToolCallResult { id: String, content: String },
    TurnEnd,
    Notification { message: String },
    Error { message: String },
    TokenUsage { input: u32, output: u32, budget_pct: f32 },
    SessionRestored { session_id: String, message_count: usize },
    SessionCreated { session_id: String },
}
```

---

## 3. 核心类型（实际实现）

### 3.1 Message

```rust
// nova-core/src/message.rs

pub enum Role { System, User, Assistant, Tool }

pub struct Message {
    pub role: Role,
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
    pub timestamp: DateTime<Utc>,
}

pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}
```

### 3.2 API 类型

```rust
// nova-api/src/types.rs

pub struct ApiRequest {
    pub model: String,
    pub max_tokens: u32,
    pub system: String,
    pub messages: Vec<ApiMessage>,
    pub tools: Vec<ToolSchema>,
    pub stream: bool,
}

#[serde(tag = "role")]
pub enum ApiMessage {
    User { content: Content },
    Assistant { content: Content },
}

#[serde(untagged)]
pub enum Content {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[serde(tag = "type")]
pub enum ContentBlock {
    Text { text: String },
    Thinking { thinking: String, signature: Option<String> },
    ToolUse { id: String, name: String, input: Value },
    ToolResult { tool_use_id: String, content: String },
}
```

### 3.3 SSE 流式类型

```rust
// nova-api/src/types.rs

#[serde(tag = "type")]
pub enum SseEvent {
    MessageStart { message: MessageStartData },
    ContentBlockStart { index: usize, content_block: ContentBlockStartData },
    ContentBlockDelta { index: usize, delta: DeltaData },
    ContentBlockStop { index: usize },
    MessageDelta { delta: MessageDeltaData, usage: Option<Usage> },
    MessageStop,
    Ping,
    Error { error: ErrorData },
}

pub enum ContentBlockStartData { Text, Thinking, ToolUse }
pub enum DeltaData { TextDelta, ThinkingDelta, InputJsonDelta }
```

### 3.4 StreamEvent（高层抽象）

```rust
// nova-api/src/stream.rs

pub enum StreamEvent {
    TextDelta(String),
    ToolUseStart { id: String, name: String },
    ToolInputDelta(String),
    ToolUseEnd { index: usize },
    MessageStop { stop_reason: Option<String> },
    Usage(Usage),
    Error(String),
}
```

### 3.5 Config

```rust
// nova-core/src/config.rs

pub struct NovaConfig {
    pub api_key: String,              // 默认 ""
    pub model: String,                // 默认 "MiniMax-M2.7"
    pub api_base_url: String,         // 默认 "https://api.minimaxi.com/anthropic"
    pub context_window: usize,        // 默认 200_000
    pub char_delay_ms: u64,           // 默认 5
    pub workspace: PathBuf,           // 默认 ~/.nova
    pub heartbeat_interval_secs: u64, // 默认 300
    pub max_turns: usize,             // 默认 20
    pub tool_timeout_secs: u64,       // 默认 60
    pub compact_target_pct: f32,      // 默认 0.6
    pub budget_trigger_pct: f32,      // 默认 0.9
}
```

### 3.6 Session

```rust
// nova-core/src/session/manager.rs

pub struct Session {
    pub session_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub messages: Vec<Message>,
    pub turn_count: usize,
    pub max_turns: usize,
    pub token_stats: TokenStats,
}

pub struct TokenStats {
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
}
```

---

## 4. 模块设计

### 4.1 Query Loop 流程（策略 1）

```
用户输入
  │
  ▼
session.add_message(user_msg)
  │
  ▼
┌─── Loop (max 20 turns) ──────────────────────┐
│  │                                            │
│  ▼                                            │
│  TokenBudget.check()                          │
│    ├─ NeedsCompact → Compactor.compact()      │
│    ├─ Diminishing → break                     │
│    └─ Ok → continue                           │
│  │                                            │
│  ▼                                            │
│  build_api_messages(session.messages)          │
│  ApiClient.stream(req, stream_tx)             │
│    → TextDelta → event_tx                     │
│    → ToolUseStart/Delta/End → 收集            │
│    → ThinkingDelta → 跳过                     │
│    → Usage → event_tx                         │
│  │                                            │
│  ▼                                            │
│  hooks.fire_post_sampling(assistant_msg)      │
│  session.add_message(assistant_msg)           │
│  │                                            │
│  ▼                                            │
│  有 tool_calls?                               │
│    ├─ 否 → hooks.fire_stop() → TurnEnd       │
│    └─ 是 → tools.execute(each) → 结果追加     │
│           → continue                          │
└───────────────────────────────────────────────┘
```

### 4.2 build_api_messages 转换规则

| Message.role | → ApiMessage |
|:---|:---|
| System | 跳过（system prompt 单独传） |
| User | `ApiMessage::User { content: Text(content) }` |
| Assistant | `ApiMessage::Assistant { content: Blocks([Text, ToolUse...]) }` |
| Tool | `ApiMessage::User { content: Blocks([ToolResult]) }` |

### 4.3 Daemon 连接处理

```
accept connection
  → recv ResumeSession → resume_latest() or create()
  → loop:
      recv UserMessage
        → session.add_message(user)
        → dual_write.clear_marker()
        → spawn QueryLoop.run()
        → forward LoopEvent → IPC Event
        → save session meta
      recv NewSession → create new session
      recv Shutdown → exit
```

### 4.4 TUI 架构

```
main → run_app
  │
  ├─ IpcClient.connect(/tmp/nova.sock)
  ├─ send ResumeSession
  │
  ├─ spawn IPC reader task:
  │    loop { select! {
  │      req_rx.recv → client.send_request
  │      client.recv_event → event_tx.send
  │    }}
  │
  └─ main render loop:
       loop {
         event_rx.try_recv → update app state
         handle_input → Submit/Quit/None
         terminal.draw(render)
       }
```

### 4.5 TUI 布局

```
┌─ [●] NOVA v1.0 │ Session: xxx │ Tokens: N↑ N↓ │ Budget: N% ─┐
├───────────────────────────────────────────────────────────────┤
│  ◈ Chat                                                       │
│  ▶ You: ...                                                   │
│  ◆ Kiko: ...                                                  │
│  ⚙ bash: ...                                                  │
│  ◇ System: ...                                                │
├───────────────────────────────────────────────────────────────┤
│  ⌨ Input: > _                                                 │
└───────────────────────────────────────────────────────────────┘

配色：BG=#0A0A19  Primary=Cyan  Secondary=Purple  Accent=Magenta
```

---

## 5. 策略 → 模块映射（实际）

| # | 策略 | crate | 模块路径 | 关键类型 | 状态 |
|:--|:---|:---|:---|:---|:---|
| 1 | Query Loop | nova-core | agent/loop.rs | `QueryLoop`, `QueryLoopConfig`, `LoopEvent` | ✅ 已接入 |
| 2 | Token Budget | nova-core | token/budget.rs | `TokenBudget`, `BudgetCheck` | ✅ 已接入 |
| 3 | Compact | nova-core | token/compact.rs | `Compactor` | ✅ 已接入 |
| 4 | Forked Agent | nova-core | agent/forked.rs | `ForkedAgent` | ✅ 已接入（SubagentSpawner 内部使用）|
| 5 | PostSampling | nova-core | hooks/post_sampling.rs | `MemoryExtractHook` | 🔧 已接入框架（T21 替代）|
| 6 | StopHooks | nova-core | hooks/stop.rs | `MemoryExtractStopHook` | 🔧 已接入框架（T21 替代）|
| 7 | 双写互斥 | nova-core | memory/dual_write.rs | `DualWriteMemory`, `MemoryType` | ❌ 已停用 |
| 8 | 工具池排序 | nova-core | tools/registry.rs | `ToolRegistry` | ✅ 已接入 |
| 9 | Team | nova-core | team/ | `TeamManager`, `Team`, `Mailbox` | 🔧 待接入 |
| 10 | Subagent | nova-core | subagent/spawn.rs | `SubagentSpawner`, `SubagentConfig` | 🔧 部分接入 |
| 11 | SideQuery | nova-core | sidequery/query.rs | `SideQuery` | ✅ 已接入 |
| 12 | autoDream | nova-core | dream/engine.rs | `DreamEngine` | ✅ 已接入 |
| 13 | Worktree | nova-core | worktree/isolate.rs | `WorktreeManager`, `Worktree` | 🔧 待接入 |
| 14 | Coordinator | nova-core | coordinator/orchestrator.rs | `Coordinator`, `CoordinatorPhase` | 🔧 待接入 |
| 15 | Paste Store | nova-core | paste/store.rs | `PasteStore` | 🔧 待接入 |
| 16 | Session JSONL | nova-core | session/ | `SessionManager`, `SessionHistory` | ✅ 已接入 |
| — | Heartbeat | nova-core | heartbeat/scheduler.rs | `HeartbeatScheduler` | ✅ 已接入（scheduler 已启动，event logged）|
| — | Skills | nova-core | skills/loader.rs | `SkillsLoader`, `Skill` | ✅ 已接入 |
| — | Sandbox | nova-core | sandbox/policy.rs | `SandboxPolicy`, `SandboxLevel` | ✅ 已接入 |
| — | Retry | nova-core | retry/policy.rs | `RetryPolicy` | ✅ 已接入 |
| — | IPC | nova-ipc | protocol.rs + server/client | `Request`, `Event`, `IpcServer`, `IpcClient` | ✅ 已接入 |
| — | Daemon | nova-daemon | main.rs | `run_daemon`, `handle_connection` | ✅ 已接入 |
| — | TUI | nova-tui | main/app/ui/input/theme | `App`, `render`, `handle_input`, `Theme` | ✅ 已接入 |

---

## 6. 设计文档 vs 实际代码差异说明

| 项目 | 原设计 | 实际实现 |
|:---|:---|:---|
| crate 命名 | runtime/daemon/tui/ipc | nova-core/nova-api/nova-daemon/nova-tui/nova-ipc（5 个） |
| API 客户端 | 在 runtime 内部 | 独立 `nova-api` crate |
| IPC 传输格式 | 4 字节长度前缀 + JSON | JSON lines（`\n` 分隔） |
| Message 类型 | tagged enum | struct + Role enum |
| TokenStats | per_turn_input Vec | 简化为 total_input/output |
| Workspace 文件 | .md 后缀 | .md 后缀（SOUL.md 等） |
| Phase 2/3 模块 | 未创建 | 全部完整实现（Team/Subagent/Dream/Worktree/Coordinator/Paste） |
| thinking block | 未考虑 | 已支持（MiniMax M2.7 特性） |
| I/O Shield | 未设计 | bash 20k / browser 15k 截断 |
| Compact 双层熔断 | 未设计 | Graceful (85-95%) / Forceful (>95%) |
| TopicTracker | 不存在 | 话题状态机已集成 loop |
| TensionTracker | 不存在 | 张力值追踪已集成 loop |
| ModeRouter | 不存在 | 模式路由已集成 loop，`<nova_os>` 已注入 |


---

## 7. 上下文与记忆架构 — 最终方案

> 综合 OpenClaw、Claude Code 和 NOVA 自身能力，取各自最优部分。

### 7.1 现状问题

| 问题 | 说明 |
|:--|:--|
| Bootstrap 只加载一次 | daemon 启动时读取 SOUL/IDENTITY 等，运行中改文件不生效 |
| 隔夜断片 | compact 有损压缩后早期对话细节丢失，session 恢复续不上 |
| 文件未注入 | STATE.md、TASKS.md 读了没用；HEARTBEAT.md 没独立区块 |
| MEMORY.md 膨胀 | 直接注入 system prompt，内容多了挤占 context window |

### 7.2 方案总览：三件事各取所长

```
┌──────────────────────────────────────────────────────────────┐
│  Bootstrap 热加载（取自 OpenClaw + mtime 缓存优化）          │
│                                                              │
│  每次 API 请求前检查文件 mtime：                             │
│    mtime 没变 → 用缓存                                      │
│    mtime 变了 → 重新读磁盘                                   │
│                                                              │
│  注入顺序：SOUL → IDENTITY → AGENTS → USER → STATE → TASKS  │
│  截断：单文件 20K，总量 150K                                 │
│  MEMORY.md 不注入，通过工具搜索（OpenClaw 方式）             │
│  HEARTBEAT.md 独立区块，不走 bootstrap 管道                  │
├──────────────────────────────────────────────────────────────┤
│  Agentic Search 跨 Session 检索（NOVA 独有）                 │
│                                                              │
│  每次用户消息 → 自动检索相关历史 session → 注入 system prompt │
│  原始对话永远在 JSONL 里，检索就是记忆，不搞摘要中间层       │
│  Session 恢复时也自动检索，解决"隔夜断片"                    │
├──────────────────────────────────────────────────────────────┤
│  分层 Compact（改良 OpenClaw）                               │
│                                                              │
│  最近 N 轮：原样保留                                         │
│  中期消息：保留 user 原文 + assistant 决策，删工具调用细节    │
│  关键决策/文件修改：标记 pinned，永不压缩                    │
│  （替代 OpenClaw 的全量压成一句摘要）                        │
└──────────────────────────────────────────────────────────────┘
```

### 7.3 三者对比 & NOVA 的选择

| 维度 | OpenClaw | Claude Code | NOVA 最终方案 |
|:--|:--|:--|:--|
| Bootstrap 加载 | 每次请求读磁盘 | memoize + 事件清缓存 | **每次请求 + mtime 缓存** |
| 热更新 | ✅ 实时 | 需特定事件触发 | ✅ 实时（mtime 变化即生效） |
| 截断 | 单文件 20K，总量 150K | 无硬限制 | **单文件 20K，总量 150K** |
| MEMORY.md | 不注入，工具搜索 | 注入 system prompt | **不注入，工具搜索** |
| 跨 Session 记忆 | ❌ 无 | ❌ 无 | **✅ Agentic Search** |
| 隔夜续接 | ❌ 断片 | ❌ 断片 | **✅ 恢复时自动检索** |
| Compact | 全量摘要（有损） | 全量摘要（有损） | **分层压缩 + pinned** |
| 复杂度 | 低 | 高（memoize + 事件系统） | 中 |

### 7.4 Bootstrap 热加载设计

```rust
// 新增：BootstrapLoader（替代当前的 WorkspaceLoader + PromptBuilder 组合）

struct CachedFile {
    content: String,
    mtime: SystemTime,
}

struct BootstrapLoader {
    workspace_dir: PathBuf,
    cache: HashMap<String, CachedFile>,  // filename → cached content
}

impl BootstrapLoader {
    /// 每次 API 请求前调用，返回拼接好的 system prompt
    fn build_system_prompt(&mut self, tools: &ToolRegistry) -> String {
        let files = [
            "SOUL.md", "IDENTITY.md", "AGENTS.md",
            "USER.md", "STATE.md", "TASKS.md",
        ];

        let mut parts = Vec::new();
        let mut total_chars = 0;
        const MAX_PER_FILE: usize = 20_000;
        const MAX_TOTAL: usize = 150_000;

        for name in &files {
            let content = self.load_with_cache(name);
            if content.is_empty() { continue; }

            let budget = MAX_PER_FILE.min(MAX_TOTAL - total_chars);
            if budget < 64 { break; }

            let truncated = truncate_bootstrap(&content, budget);
            total_chars += truncated.len();
            parts.push(truncated);
        }

        // 工具描述始终追加
        parts.push(tools.describe_all());
        parts.join("\n\n---\n\n")
    }

    fn load_with_cache(&mut self, name: &str) -> String {
        let path = self.workspace_dir.join(name);
        let mtime = fs::metadata(&path).ok().and_then(|m| m.modified().ok());

        if let (Some(cached), Some(mtime)) = (self.cache.get(name), mtime) {
            if cached.mtime == mtime {
                return cached.content.clone();  // mtime 没变，用缓存
            }
        }

        // 重新读取
        let content = fs::read_to_string(&path).unwrap_or_default();
        if let Some(mtime) = mtime {
            self.cache.insert(name.to_string(), CachedFile { content: content.clone(), mtime });
        }
        content
    }
}
```

截断规则（对齐 OpenClaw）：
- 头部 70% + 尾部 20% + 中间 `\n...(truncated)...\n`
- 剩余预算 < 64 字符时跳过后续文件

### 7.5 Agentic Search 增强

当前已实现基础版，需要增强：

| 改动 | 说明 |
|:--|:--|
| transcript 提取量 2K → 8K | 覆盖更多对话内容，提高检索准确率 |
| 注入量动态调整 | 根据 context window 剩余空间决定注入多少（而非固定 500 字符） |
| Session 恢复时自动检索 | daemon 恢复 session 时，用最后一条 user 消息触发检索 |

### 7.6 分层 Compact 设计

替代当前的全量摘要：

```
消息列表（从旧到新）：
┌─────────────────────────────────────────┐
│  [pinned] 关键决策/文件修改 → 永不压缩  │  ← 新增
│  早期消息 → LLM 摘要（bullet points）   │  ← 现有，但改为结构化
│  中期消息 → 删工具调用，保留对话原文     │  ← 新增
│  最近 N 轮 → 原样保留                   │  ← 现有
└─────────────────────────────────────────┘
```

pinned 标记规则：
- assistant 消息包含 `write_file` 工具调用 → pinned
- user 消息包含明确决策指令（"用方案A"、"改成..."） → pinned
- 手动 `/pin` 命令标记

### 7.7 实现优先级

| 优先级 | 改动 | 复杂度 | 依赖 |
|:--|:--|:--|:--|
| P0 | BootstrapLoader 热加载 + mtime 缓存 | 低 | 无 |
| P0 | STATE.md / TASKS.md 注入 system prompt | 低 | P0 热加载 |
| P0 | Agentic Search transcript 2K → 8K | 低 | 无 |
| P1 | 检索注入量动态调整 | 中 | 无 |
| P1 | Session 恢复时自动检索 | 低 | 无 |
| P1 | MEMORY.md 改为工具搜索（不注入） | 中 | 需新增 memory_search 工具 |
| P2 | 分层 Compact + pinned 消息 | 高 | 需改 Message 结构 |
| P2 | HEARTBEAT.md 独立区块 | 低 | P0 热加载 |

---

## 8. P0 新增模块设计

### 8.1 FileEditTool — 精确字符串替换

```
nova-core/src/tools/file_edit.rs
```

**核心逻辑**：

```rust
pub struct FileEditTool;

// 参数
struct FileEditInput {
    file_path: String,
    old_string: String,
    new_string: String,
    replace_all: bool,  // 默认 false
}

// 执行流程：
// 1. 读取文件全文
// 2. 查找 old_string 出现次数
//    - 0 次 → 返回错误 "old_string not found"
//    - 1 次 → 替换
//    - >1 次 + replace_all=false → 返回错误 "old_string found N times, use replace_all"
//    - >1 次 + replace_all=true → 全部替换
// 3. 写回文件
// 4. 返回 { file_path, replacements_made, preview }
```

**与 write_file 的区别**：
- write_file 全量覆盖，适合新建文件或小文件
- file_edit 精确替换，适合修改大文件中的特定代码段

**注册**：在 `tools/mod.rs` 中导出，在 daemon `make_tools()` 中注册。

### 8.2 三层记忆系统

废弃原有 JSONL 记忆存储，改为三层 markdown 记忆模型。

#### 8.2.1 架构总览

```
~/.nova/
├── MEMORY.md                    # 层1：工作记忆（始终注入 system prompt）
├── memories/                    # 层2：情景记忆（按天组织的日记）
│   ├── 2026-04-13.md
│   ├── 2026-04-14.md
│   └── ...
└── sessions/                    # 层3：细节记忆（完整对话，已有）
    ├── <uuid>.jsonl
    └── <uuid>.meta.json
```

#### 8.2.2 层1：MEMORY.md（工作记忆）

当前核心事项，LLM 每次对话都能看到。

**存储格式**：纯 markdown，精炼的索引+核心事项，<200 行。

```markdown
# 记忆

## 用户
- 全栈开发者，偏好 Rust + TypeScript
- 喜欢简洁代码，不要过度注释

## 当前项目
- NOVA：Rust 重写的 OpenClaw，赛博朋克 TUI
- 正在实现 P0 工具（file_edit, 记忆系统, browser CDP）

## 反馈
- 不要在回答末尾总结，用户能看 diff
- 优先用 file_edit 而非 write_file 修改文件

## 参考
- OpenClaw 源码：/Users/.../openclaw/projects/openclaw
- Claude Code 源码：/Users/.../openclaw/projects/claude-code-main
```

**写入机制**（基于闲时与双写互斥的混合管线）：
1. **优先（主动写入）** — LLM 主动写：prompt 引导，在日常对话中由框架鼓励调用工具更新，同时将后台会话级别的锁 `memory_updated_mutex` 置 true（表明不需要本轮后续多余插手）。
2. **兜底（防重空闲检测）** — 随着长时间交互导致游离的 15m 空余（闲时），或因空间濒临上限遭截断（预留池 Compact），都会拉起断点校验；若检查出此前互斥锁未 true，起子线程 SideQuery 给极其冷嘲严格的 Prompt 补全差异后标记新游标；若已被 LLM 写过（锁已阻断），则将扫描位推进直接抛弃防重叠。
3. **低频整理：Dream** — 从 memories/*.md 日记中提炼更慢节奏的梳理。

**读取**：BootstrapLoader 每次 API 请求前加载（已有 mtime 缓存），注入 system prompt。

#### 8.2.3 层2：memories/YYYY-MM-DD.md（情景记忆）

按天组织的日记，沿时间线检索。

**存储格式**：markdown，追加式写入，带时间戳。

```markdown
# 2026-04-14

## 14:30 — NOVA 工具系统扩展
- 实现了 file_edit 工具（精确字符串替换）
- 讨论了三层记忆模型设计
- 决定废弃 JSONL 记忆，改用 markdown

## 16:45 — 浏览器 CDP 设计
- 参考了 OpenClaw extensions/browser/ 源码
- 确定用 chromiumoxide crate
- 第一版只做核心 action
```

**写入机制**（系统自动，不依赖 LLM 自觉）：
1. **Compact 前** — 信息即将丢失，用 SideQuery 生成即将被压缩的消息摘要，追加到当天日记
2. **Session 结束时** — 退出 TUI / /new，预处理 session（保留 user 原文 + assistant 决策，删工具调用细节），map-reduce 分层摘要（Map 按维度提取关键点 → Reduce 汇总成 ~200 字），追加到当天日记
3. **每 N 个 turn**（可选）— 定期用 SideQuery 追加增量摘要

**预处理 + Map-Reduce 摘要流程**：
1. 预处理：保留 user 原文 + assistant 决策，删除工具调用细节
2. Map 阶段：按 ~180K 字符分批，每批按维度（事件、反馈、用户偏好、项目状态、重要决策、参考资料）提取关键点
3. Reduce 阶段：合并所有批次摘要，汇总成 ~200 字最终摘要（纯文本，无 markdown）

**整理**：Dream 分析日记内容，生成摘要索引，清理冗余。

**读取/召回**：用户消息到达时，sideQuery 扫描 memories/*.md 文件名（日期）+ 首行标题，选相关的注入 system prompt。

#### 8.2.4 层3：sessions/（细节记忆）

已有，不变。JSONL 完整对话记录 + Agentic Session Search。

#### 8.2.5 Dream — 记忆整理

参考 Claude Code autoDream + OpenClaw dreaming。

**触发条件**：
- 距上次整理 ≥ 24 小时 + 有 ≥ 5 个新 session
- 或用户手动 `/dream`
- 每个 turn 结束时检查（stopHooks 中）

**执行方式**：forked agent（后台 SideQuery），拿到 read_file + file_edit + write_file（限 memory 目录）。

**整理流程**：
1. **Orient** — 读 MEMORY.md + ls memories/ 目录
2. **Gather** — 读最近的 memories/YYYY-MM-DD.md 日记，必要时 grep session transcript
3. **Consolidate** — 从日记中提炼核心事项更新 MEMORY.md，合并重复，相对日期→绝对日期，删除矛盾
4. **Prune** — MEMORY.md 保持 <200 行，日记中的冗余条目精简

**锁机制**：`~/.nova/memories/.dream-lock`，PID 文件锁防并发。

#### 8.2.6 召回管线

```
用户消息到达
  │
  ├─ Agentic Session Search（已有）
  │    → 搜索历史 session → 注入 <relevant_history>
  │
  └─ Memory Recall（新增）

### 8.3 AgenticSearchTool — 暴露语义搜索

```text
nova-core/src/tools/agentic_search.rs
```

**核心逻辑**：
将内置的 `AgenticSessionSearch` 封装为主动调用的工具暴露给 LLM。

```rust
pub struct AgenticSearchTool {
    side_query: SideQuery,
    session_manager: SessionManager,
}

// 接收 query，调用 AgenticSessionSearch 获取最相关的历史，格式化输出
```
**特点**：
- **无范围限制**：默认扫描所有可用 Session，支持跨项目检索。
- **主动触发**：通过明确的 description 指导大模型在特定语境（“回忆一下…”）主动调用。
- **依赖注入**：在 `make_tools()` 时注入 `SideQuery`（用于二次 LLM 排序）和 `SessionManager`（克隆支持）。
  │    → 扫描 memories/*.md 文件名+首行
  │    → SideQuery 选相关日记（最多 3 天）
  │    → 读取选中日记内容
  │    → 注入 <relevant_memories> 到 system prompt
  │
  ▼
QueryLoop.run()
  （MEMORY.md 已在 system prompt 中，无需额外召回）
```

#### 8.2.7 模块结构

```
nova-core/src/memory/
├── mod.rs              # pub mod 导出
├── dual_write.rs       # 废弃（保留兼容，后续删除）
├── store.rs            # 废弃（保留兼容，后续删除）
├── daily.rs            # DailyNotes → Layer 2 日记（memories/YYYY-MM-DD.md）
├── recall.rs           # MemoryRecall → 记忆召回管线（实现）
└── dream.rs           # DreamEngine → 记忆整理（实现）
```

#### 8.2.8 实现优先级

| 阶段 | 内容 | 复杂度 |
|:---|:---|:---|
| 1 | 日记写入（Compact 前 + Session 结束时） | 中 |
| 2 | 召回管线（扫描日记 + SideQuery 选择 + 注入） | 中 |
| 3 | MEMORY.md prompt 引导（AGENTS.md 中描述用法） | 低 |
| 4 | Dream 整理（定期后台整理 MEMORY.md + 日记） | 高 |

### 8.3 BrowserTool — Playwright MCP 浏览器自动化（已实现）

```
nova-core/src/tools/browser.rs   # 单文件，~230 行
```

**技术选型（最终）**：放弃 `chromiumoxide`（纯 Rust CDP），改用 `@playwright/mcp`（Microsoft 官方）。

核心原因：
- Playwright 内置 Chromium 极易被反爬检测；`@playwright/mcp` 支持 `executablePath` 指定本机 Chrome
- `chromiumoxide` 需自己实现 CDP 协议解析（复杂），不如直接 shell out 给成熟库

**架构（每次调用独立子进程）**：

```
BrowserTool.execute(action, params)
  │
  ├─ write_config() — 刷新 ~/.nova/playwright-mcp.json
  │    └─ 自动探测本机 Chrome 路径
  │
  └─ call_tool(tool_name, arguments)
       │
       ├─ spawn: npx @playwright/mcp@latest --config ~/.nova/playwright-mcp.json
       │    stdin: [initialize JSON-RPC]
       │            [notifications/initialized JSON-RPC]
       │            [tools/call JSON-RPC]
       │
       ├─ 从 stdout 逐行读，找 id 匹配的响应（60s 超时）
       │
       └─ parse_mcp_response() → 提取 content[].text 或 resource.uri
```

**MCP 消息格式（stdio，每条一行 JSON）**：

```json
{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"nova","version":"1.0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_navigate","arguments":{"url":"https://example.com"}}}
```

**playwright-mcp 配置文件**（自动生成到 `~/.nova/playwright-mcp.json`）：

```json
{
  "browser": {
    "launchOptions": {
      "executablePath": "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
      "headless": true,
      "args": ["--no-sandbox", "--disable-blink-features=AutomationControlled", "--disable-infobars"]
    },
    "userDataDir": "~/.nova/browser-profile",
    "contextOptions": {
      "userAgent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 ..."
    }
  },
  "outputDir": "~/.nova/browser-screenshots",
  "imageResponses": "allow"
}
```

**Action → playwright-mcp tool 映射**：

| Nova action | playwright-mcp tool | 参数 |
|:---|:---|:---|
| `navigate` | `browser_navigate` | `url` |
| `snapshot` | `browser_snapshot` | — |
| `click` | `browser_click` | `ref` 或 `selector` |
| `type` | `browser_type` | `ref`/`selector` + `text` |
| `press` | `browser_press_key` | `key` |
| `scroll_down` | `browser_scroll_down` | — |
| `scroll_up` | `browser_scroll_up` | — |
| `screenshot` | `browser_take_screenshot` | — |
| `go_back` | `browser_navigate_back` | — |
| `close` | `browser_close` | — |

**Chrome 自动探测顺序**（`discover_chrome`）：
1. config 指定的 `browser_chrome_path`
2. `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`
3. `/Applications/Chromium.app/Contents/MacOS/Chromium`
4. `/usr/bin/google-chrome` / `/usr/bin/chromium-browser`
5. 以上均无 → fallback 用 Playwright 内置 Chromium

**NovaConfig 新增字段**：

```rust
pub browser_chrome_path: Option<String>,   // None = 自动探测
pub browser_profile_dir: Option<String>,   // None = ~/.nova/browser-profile
pub browser_headless: Option<bool>,        // None = true
```

**注册位置**：`nova-daemon/src/main.rs` → `make_tools()` → `tools.register_builtin(Box::new(BrowserTool::new(...)))>`

**前提依赖**：Node.js >= 18（`@playwright/mcp` 通过 `npx --yes` 自动按需下载，首次运行有短暂延迟）。



---

## 9. 工具追平 Claude Code 设计（T28-T33）

> 参考源码：`~/Documents/openclaw/projects/claude-code-main/tools/`
> 目标：Nova 已有工具对标 Claude Code 42 个检查项全部对齐

### 9.1 FileReadTracker — 全局读写追踪

**作用**：read_file/write_file/file_edit 共用的状态追踪器，防止盲写/盲改。

```rust
// nova-core/src/tools/read_tracker.rs

pub struct ReadTracker {
    /// file_path -> (mtime_ns, is_partial_view)
    reads: RwLock<HashMap<PathBuf, FileReadState>>,
}

#[derive(Clone)]
pub struct FileReadState {
    pub timestamp: u64,       // mtime in nanoseconds
    pub content: Option<String>, // full content snapshot
    pub is_partial_view: bool,  // true if read with start_line/end_line
    pub offset: Option<usize>,  // start_line (1-based)
    pub limit: Option<usize>,   // end_line
}

impl ReadTracker {
    pub fn record_read(&self, path: &Path, state: FileReadState);
    pub fn get_read_state(&self, path: &Path) -> Option<FileReadState>;
    pub fn invalidate(&self, path: &Path);  // 文件被外部修改时调用
}
```

**Mtime 检测逻辑**：
- 写入前比对 mtime，防止 linter/用户修改覆盖
- Windows 云同步场景：用 content hash fallback

### 9.2 bash 安全加固（T28）

**参考**：`claude-code-main/tools/BashTool/bashSecurity.ts`（~1000 行，22 种检查）

**安全检查分类**：

| 类别 | 检查项 | Claude Code 行数 |
|:---|:---|:---|
| Zsh 危险命令 | zmodload/emulate/sysopen/zpty/ztcp 等 20+ 个 | ~200 行 |
| JQ 安全 | jq --run . script / jq -f | ~100 行 |
| Curl/Wget | 可疑 URL/主机/端口检测 | ~80 行 |
| Shell 语法 | 命令替换 / brace expansion / control chars | ~200 行 |
| Git commit | git commit -m 等注入检测 | ~80 行 |
| Heredoc 安全 | 安全 heredoc 模式验证 | ~200 行 |
| Proc environ | /proc/self/environ 读取检测 | ~30 行 |

### 9.3 write_file/file_edit 原子写入

**参考**：`claude-code-main/tools/FileWriteTool/FileWriteTool.ts`

```rust
// nova-core/src/tools/atomic_write.rs

/// 原子写入：temp file + rename
pub fn atomic_write(path: &Path, content: &str) -> Result<()> {
    let temp_dir = path.parent().unwrap_or(Path::new("."));
    let temp_file = temp_dir.join(".tmp.xxx");
    std::fs::write(&temp_file, content)?;
    std::fs::rename(&temp_file, path)?;  // 原子替换
    Ok(())
}

/// 文件历史备份
pub fn backup_before_write(path: &Path) -> Result<PathBuf> {
    let history_dir = dirs::home_dir().unwrap().join(".nova/file_history");
    std::fs::create_dir_all(&history_dir)?;
    let backup_name = format!(
        "{}_{}.bak",
        path.to_string_lossy().replace("/", "_"),
        chrono::Utc::now().format("%Y%m%d_%H%M%S")
    );
    let backup_path = history_dir.join(backup_name);
    std::fs::copy(path, &backup_path)?;
    Ok(backup_path)
}
```

### 9.4 Structured Patch 输出

**参考**：`claude-code-main/tools/FileEditTool/types.ts`

```rust
// nova-core/src/tools/diff.rs

#[derive(Debug, Serialize)]
pub struct DiffHunk {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Serialize)]
pub struct DiffLine {
    pub line_type: LineType, // Add/Remove/Context
    pub content: String,
}
```

### 9.5 grep head_limit + offset 分页

**参考**：`claude-code-main/tools/GrepTool/GrepTool.ts`

| 参数 | 类型 | 说明 |
|:---|:---|:---|
| output_mode | enum | content / files_with_matches / count |
| head_limit | usize | 最多返回 N 条（默认 250，0=无限制） |
| offset | usize | 跳过前 N 条（分页） |
| -B | usize | 匹配前 N 行上下文 |
| -A | usize | 匹配后 N 行上下文 |

### 9.6 工具文件结构（增强后）

```
nova-core/src/tools/
├── mod.rs              # Tool trait 定义
├── registry.rs         # ToolRegistry
├── bash.rs            # BashTool（增强后 ~500 行）
├── bash/
│   └── security.rs    # BashSecurity — 22 种检查（T28）
├── read_file.rs       # ReadFileTool（增强后）
├── read_tracker.rs    # FileReadTracker — 全局读写追踪（T29）
├── write_file.rs      # WriteFileTool（增强后 ~200 行）
├── file_edit.rs       # FileEditTool（增强后 ~200 行）
├── glob.rs            # GlobTool（增强后 ~150 行）
├── grep.rs            # GrepTool（增强后 ~300 行）
├── diff.rs            # Structured patch 算法（T30-T31）
├── browser.rs         # BrowserTool（T22）
└── agentic_search.rs  # AgenticSearchTool（T24）
```

### 9.7 优先级与依赖

| 任务 | 预估 | 复杂度 |
|:---|:---|:---|
| T28 bash 安全 | 6h | 高 |
| T29 read_file 增强 | 3h | 中 |
| T30 write_file 增强 | 3h | 中 |
| T31 file_edit 增强 | 2h | 中 |
| T32 grep 增强 | 2h | 低 |
| T33 glob 增强 | 1h | 低 |

**预计总工作量**：~17h
