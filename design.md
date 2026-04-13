# NOVA 架构设计

**版本**: v3.0
**日期**: 2026-04-08

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
│       ├── team/               # 策略 9 骨架
│       │   ├── mod.rs
│       │   ├── config.rs       # TeamManager / Team / Task
│       │   └── mailbox.rs      # Mailbox — per-agent 消息
│       ├── subagent/           # 策略 10 骨架
│       │   ├── mod.rs
│       │   └── spawn.rs        # SubagentSpawner / SubagentConfig
│       ├── sidequery/          # 策略 11 骨架
│       │   ├── mod.rs
│       │   └── query.rs        # SideQuery
│       ├── dream/              # 策略 12 骨架
│       │   ├── mod.rs
│       │   └── engine.rs       # DreamEngine
│       ├── worktree/           # 策略 13 骨架
│       │   ├── mod.rs
│       │   └── isolate.rs      # WorktreeManager / Worktree
│       ├── coordinator/        # 策略 14 骨架
│       │   ├── mod.rs
│       │   └── orchestrator.rs # Coordinator / CoordinatorPhase
│       ├── paste/              # 策略 15 骨架
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
│  │ IpcServer (Unix Socket)                │ │
│  └────────────────────────────────────────┘ │
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
┌─ [●] KIKO v0.1 │ Session: xxx │ Tokens: N↑ N↓ │ Budget: N% ─┐
├───────────────────────────────────────────────────────────────┤
│  ◈ NOVA                                                       │
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
| 1 | Query Loop | nova-core | agent/loop.rs | `QueryLoop`, `QueryLoopConfig`, `LoopEvent` | ✅ |
| 2 | Token Budget | nova-core | token/budget.rs | `TokenBudget`, `BudgetCheck` | ✅ |
| 3 | Compact | nova-core | token/compact.rs | `Compactor` | ✅ |
| 4 | Forked Agent | nova-core | agent/forked.rs | `ForkedAgent` | ✅ |
| 5 | PostSampling | nova-core | hooks/post_sampling.rs | `MemoryExtractHook` | ✅ |
| 6 | StopHooks | nova-core | hooks/stop.rs | `MemoryExtractStopHook` | ✅ |
| 7 | 双写互斥 | nova-core | memory/dual_write.rs | `DualWriteMemory`, `MemoryType` | ✅ |
| 8 | 工具池排序 | nova-core | tools/registry.rs | `ToolRegistry` | ✅ |
| 9 | Team | nova-core | team/ | `TeamManager`, `Team`, `Mailbox` | 🔧 |
| 10 | Subagent | nova-core | subagent/spawn.rs | `SubagentSpawner`, `SubagentConfig` | 🔧 |
| 11 | SideQuery | nova-core | sidequery/query.rs | `SideQuery` | ✅ |
| 12 | autoDream | nova-core | dream/engine.rs | `DreamEngine` | 🔧 |
| 13 | Worktree | nova-core | worktree/isolate.rs | `WorktreeManager`, `Worktree` | 🔧 |
| 14 | Coordinator | nova-core | coordinator/orchestrator.rs | `Coordinator`, `CoordinatorPhase` | 🔧 |
| 15 | Paste Store | nova-core | paste/store.rs | `PasteStore` | 🔧 |
| 16 | Session JSONL | nova-core | session/ | `SessionManager`, `SessionHistory` | ✅ |
| — | Heartbeat | nova-core | heartbeat/scheduler.rs | `HeartbeatScheduler` | 🔧 |
| — | Skills | nova-core | skills/loader.rs | `SkillsLoader`, `Skill` | 🔧 |
| — | Sandbox | nova-core | sandbox/policy.rs | `SandboxPolicy`, `SandboxLevel` | 🔧 |
| — | Retry | nova-core | retry/policy.rs | `RetryPolicy` | 🔧 |
| — | IPC | nova-ipc | protocol.rs + server/client | `Request`, `Event`, `IpcServer`, `IpcClient` | ✅ |
| — | Daemon | nova-daemon | main.rs | `run_daemon`, `handle_connection` | ✅ |
| — | TUI | nova-tui | main/app/ui/input/theme | `App`, `render`, `handle_input`, `Theme` | ✅ |

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
| Phase 2/3 模块 | 未创建 | 全部骨架已搭建（19 个模块） |
| thinking block | 未考虑 | 已支持（MiniMax M2.7 特性） |


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
