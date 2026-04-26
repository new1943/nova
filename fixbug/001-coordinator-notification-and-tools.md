# FIXBUG-001: Coordinator 完成通知丢失 & SubAgent 无工具执行能力

> 创建日期：2026-04-24
> 优先级：P0（影响核心用户体验）
> 状态：方案已确认，待实施

---

## 问题 1：Coordinator 完成后通知丢失

### 现象

用户通过 TUI 触发 `delegate_complex_project` 后，后台 Coordinator 完成了全部 4 个阶段（Research → Synthesis → Implementation → Verification），但用户**从未收到完成通知**。

### 日志证据

```
2026-04-24T06:29:13.099164Z  INFO Subagent 'coordinator-verification' completed
2026-04-24T06:29:13.099600Z  INFO Dispatcher: ProjectCompleted → Discord (channel=coordinator)
2026-04-24T06:29:13.099628Z  WARN [V4] Invalid Discord channel ID: coordinator (tried 'coordinator' mapping)
```

### 根因分析

**问题链路**：

```
Coordinator 完成
  ↓
emitter.emit(ShadowEvent::ProjectCompleted { channel_id: "coordinator" })
  ↓
Dispatcher::handle_event() 收到事件
  ↓
尝试通过 Discord push channel 发送
  ↓
channel_id = "coordinator" 不是合法 Discord channel ID → WARN → 丢弃
```

**三个层面的问题**：

1. **`channel_id: "coordinator"` 是硬编码的占位符**（`delegate_complex_project.rs:134`），不是真实的 Discord channel ID
2. **ProjectCompleted 只有 Discord 推送一条路**（`dispatcher.rs:151-164`），没有 TUI/IPC 推送通道。即使 Discord 推送成功，TUI 用户也收不到
3. **Discord 端同样丢失 channel ID**：`discord.rs:114` 拿到了真实的 `msg.channel_id`，但这个值从未传递给 `DelegateComplexProjectTool`。工具执行时完全不知道请求来自哪个客户端（TUI/Discord）、也不知道应该回调到哪个地址

**TUI 端的问题**：主 Agent 的 `event_rx` 循环在 `TurnEnd` 后就退出了（`main.rs:703-718`），此时 IPC 连接仍然存活（等待下一条用户输入），但没有人在监听后台事件。

**Discord 端的问题**：即使 Discord push channel 配置正常，`channel_id: "coordinator"` 也会被 Dispatcher 视为非法 ID 而丢弃。正确的 Discord channel ID（如 `"1234567890"`）在 `process_discord_message()` 中就已经拿到，但没有透传到工具层。

### 解决方案

#### 架构变更 A：CallerContext 注入（解决 channel_id 丢失）

`DelegateComplexProjectTool` 需要知道"谁在调用我"。引入一个 `CallerContext` 结构，在每次 query loop 执行前设置，工具执行时读取。

```rust
// nova-core/src/agent/context.rs (新文件)

/// 调用方上下文 — 标识当前请求来自哪个客户端
#[derive(Debug, Clone)]
pub enum CallerContext {
    /// TUI 客户端 — 通知通过 IPC push 发送
    Tui,
    /// Discord 客户端 — 通知发送到指定 channel
    Discord { channel_id: String },
}
```

**注入路径**：

1. `main.rs` 的 TUI handler 在调用 `QueryLoop::run_turn()` 前设置 `CallerContext::Tui`
2. `discord.rs` 在调用 `QueryLoop::run_turn()` 前设置 `CallerContext::Discord { channel_id: msg.channel_id.to_string() }`
3. `QueryLoop` 持有当前的 `CallerContext`，并在工具执行时传递给工具
4. `DelegateComplexProjectTool::execute()` 从 context 获取真实 channel_id（如果是 Discord）或标记为 TUI

这需要修改 `Tool` trait 的 `execute` 签名，或者用 `tokio::task_local!` / `Arc<Mutex<CallerContext>>` 方式透传。

**推荐方案**：在 `DelegateComplexProjectTool` 中增加一个 `Arc<Mutex<CallerContext>>` 字段，由外部在每次 loop 开始前写入。

```rust
pub struct DelegateComplexProjectTool {
    // ... existing fields ...
    caller_ctx: Arc<Mutex<CallerContext>>,  // 新增
}
```

在 `execute()` 中：
```rust
let ctx = self.caller_ctx.lock().await.clone();
let channel_id = match ctx {
    CallerContext::Discord { channel_id } => channel_id,
    CallerContext::Tui => "tui".to_string(),  // Dispatcher 根据此值走 IPC push
};
```

#### 架构变更 B：IPC 推送通道（解决 TUI 端通知丢失）

参照 Discord push 的模式，为 TUI 增加一条独立的推送通道。

##### Step 1: 在 `nova-ipc/src/protocol.rs` 中新增 `Event` 变体

```rust
// nova-ipc/src/protocol.rs
pub enum Event {
    // ... existing variants ...
    
    /// Background project completed — pushed asynchronously outside the query loop
    ProjectCompleted { project_id: String, report: String },
}
```

##### Step 2: 在 `ShadowEvent::ProjectCompleted` 中增加 `project_id` 字段

```rust
// nova-core/src/models/events.rs
pub enum ShadowEvent {
    // ... existing variants ...
    ProjectCompleted {
        project_id: String,  // 新增：用于关联任务
        report: String,
        channel_id: String,
    },
}
```

同步更新 `delegate_complex_project.rs` 中的 emit 调用，传入 `project_id`。

##### Step 3: 在 Dispatcher 中新增 IPC push channel

```rust
// nova-daemon/src/dispatcher.rs

/// IPC push payload for TUI clients
pub struct IpcPush {
    pub event: nova_ipc::Event,
}

pub struct Dispatcher {
    workspace_dir: PathBuf,
    memory_keeper: Option<Arc<MemoryKeeper>>,
    discord_push_tx: Option<Arc<mpsc::Sender<DiscordPush>>>,
    ipc_push_tx: Option<Arc<mpsc::Sender<IpcPush>>>,  // 新增
}

impl Dispatcher {
    pub fn with_ipc_push_tx(mut self, tx: Arc<mpsc::Sender<IpcPush>>) -> Self {
        self.ipc_push_tx = Some(tx);
        self
    }
}
```

在 `handle_event` 中，`ProjectCompleted` 分支同时发送到两个通道：

```rust
ShadowEvent::ProjectCompleted { project_id, report, channel_id } => {
    // 1. Discord push (如果配置了)
    if let Some(tx) = discord_push_tx {
        let push = DiscordPush {
            channel_id: channel_id.clone(),
            content: format!("✅ Project Completed\n\n{}", report),
        };
        let _ = tx.try_send(push);
    }
    
    // 2. IPC push (TUI 通道，始终发送)
    if let Some(tx) = ipc_push_tx {
        let push = IpcPush {
            event: Event::ProjectCompleted {
                project_id: project_id.clone(),
                report: report.clone(),
            },
        };
        if tx.try_send(push).is_err() {
            warn!("IPC push channel full or closed, dropping ProjectCompleted");
        }
    }
}
```

##### Step 4: 在 `main.rs` 的连接处理中启动后台推送监听器

在 `handle_connection()` 中，`while let Some(req) = conn.recv_request()` 主循环之外，启动一个独立的 tokio task 来监听 IPC push 并发送给 TUI：

```rust
// main.rs — handle_connection()

// 创建 IPC push channel
let (ipc_push_tx, mut ipc_push_rx) = mpsc::channel::<IpcPush>(32);

// 把 ipc_push_tx 传给 Dispatcher（通过 HandleConfig 或全局注册）
// ...

// 启动后台推送监听器（独立于主 loop，IPC 连接关闭时自动停止）
let conn_writer = conn.clone_writer(); // 需要 IpcConnection 支持 clone writer
tokio::spawn(async move {
    while let Some(push) = ipc_push_rx.recv().await {
        if conn_writer.send_event(&push.event).await.is_err() {
            break; // 连接已关闭
        }
    }
});
```

> **注意**：当前 `IpcConnection` 可能不支持 split/clone writer。如果不支持，需要把 `conn` 的写端通过 `Arc<Mutex>` 共享，或者用一个额外的 mpsc channel 将 push 事件汇入主循环。
>
> **替代方案（更简单）**：在主循环的 `while let Some(req) = conn.recv_request()` 中使用 `tokio::select!`，同时监听用户请求和 IPC push 事件：
>
> ```rust
> loop {
>     tokio::select! {
>         req = conn.recv_request() => {
>             match req? {
>                 Some(Request::UserMessage { .. }) => { /* 正常处理 */ }
>                 None => break, // 连接关闭
>             }
>         }
>         push = ipc_push_rx.recv() => {
>             if let Some(push) = push {
>                 conn.send_event(&push.event).await.ok();
>             }
>         }
>     }
> }
> ```

##### Step 5: TUI 端处理 `ProjectCompleted` 事件

```rust
// nova-tui 中收到 Event::ProjectCompleted 时
Event::ProjectCompleted { project_id, report } => {
    // 显示完成通知
    println!("\n✅ 后台任务完成 (ID: {})\n{}\n", project_id, report);
}
```

### 修改文件清单

| 文件 | 改动 |
|------|------|
| `nova-core/src/agent/context.rs` | **新文件**：定义 `CallerContext` 枚举（Tui / Discord { channel_id }） |
| `nova-core/src/agent/mod.rs` | 导出 `context` 模块 |
| `nova-ipc/src/protocol.rs` | `Event` 新增 `ProjectCompleted` 变体 |
| `nova-core/src/models/events.rs` | `ShadowEvent::ProjectCompleted` 新增 `project_id` 字段 |
| `nova-core/src/tools/delegate_complex_project.rs` | 新增 `caller_ctx` 字段；emit 时从 context 获取真实 channel_id |
| `nova-daemon/src/dispatcher.rs` | 新增 `IpcPush` 结构体、`ipc_push_tx` 字段、双通道分发（Discord + IPC） |
| `nova-daemon/src/main.rs` | 创建 IPC push channel，传入 Dispatcher；主循环改用 `tokio::select!`；设置 `CallerContext::Tui` |
| `nova-daemon/src/discord.rs` | 设置 `CallerContext::Discord { channel_id: msg.channel_id }` |
| `nova-tui/src/main.rs` | 处理 `Event::ProjectCompleted` |

---

## 问题 2：SubAgent 没有工具执行能力

### 现象

Coordinator 的 4 个阶段 subagent 在收到"上网搜索 code wiki"任务时，**没有调用任何工具**（如 browser、bash），而是靠 LLM 训练数据凭空生成了一份"搜索结果"。

### 日志证据

```
2026-04-24T06:28:43.601070Z  INFO Subagent 'coordinator-research' completed
```

Research 阶段 0.6 秒完成，只做了一次 API 调用，没有任何 Tool call 记录。

### 根因分析

`orchestrator.rs:86`:
```rust
let config = SubagentConfig {
    // ...
    tools: None, // [V4 Task 4.3] Future: enable tools for Full phases
    // ...
};
```

所有阶段都传了 `tools: None`，导致 `SubagentSpawner::spawn()` 走的是 `run_simple()`（单次无工具 API 调用）而非 `run_with_tools_loop()`（带工具执行循环）。

`spawn.rs:102-106`:
```rust
let result = if is_full && has_tools {
    Self::run_with_tools_loop(config_clone, task).await  // 有工具时走这条
} else {
    Self::run_simple(config_clone, task).await  // 当前所有阶段都走这条
};
```

### 解决方案

#### 核心思路

把主 Agent 的 `ToolRegistry`（`Arc<ToolRegistry>`）通过注入链传递到 Coordinator，再到 SubagentConfig。

#### 注入链路

```
main.rs (make_tools) 
  → DelegateComplexProjectTool { tools: Arc<ToolRegistry> }    [新增字段]
    → Coordinator { tools: Option<Arc<ToolRegistry>> }          [新增字段]
      → SubagentConfig { tools: Some(Arc<ToolRegistry>) }      [已有字段，激活]
        → SubagentSpawner::run_with_tools_loop()                [已有实现，激活]
```

##### Step 1: 给 `DelegateComplexProjectTool` 增加 `tools` 字段

```rust
// nova-core/src/tools/delegate_complex_project.rs

pub struct DelegateComplexProjectTool {
    emitter: Arc<dyn ShadowEventEmitter>,
    shadow_tx: tokio::sync::mpsc::Sender<ShadowEvent>,
    api_key: String,
    api_base_url: String,
    model: String,
    system_prompt: String,
    tools: Option<Arc<ToolRegistry>>,  // 新增
}

impl DelegateComplexProjectTool {
    pub fn new(
        emitter: Arc<dyn ShadowEventEmitter>,
        shadow_tx: tokio::sync::mpsc::Sender<ShadowEvent>,
        api_key: String,
        api_base_url: String,
        model: String,
    ) -> Self {
        Self {
            // ... existing fields ...
            tools: None,
        }
    }
    
    /// 注入工具注册表
    pub fn with_tools(mut self, tools: Arc<ToolRegistry>) -> Self {
        self.tools = Some(tools);
        self
    }
}
```

##### Step 2: 给 `Coordinator` 增加 `tools` 字段

```rust
// nova-core/src/coordinator/orchestrator.rs

pub struct Coordinator {
    api_key: String,
    api_base_url: String,
    model: String,
    system_prompt: String,
    shadow_tx: Option<mpsc::Sender<ShadowEvent>>,
    tools: Option<Arc<ToolRegistry>>,  // 新增
}

impl Coordinator {
    pub fn new(
        api_key: String,
        api_base_url: String,
        model: String,
        system_prompt: String,
        shadow_tx: Option<mpsc::Sender<ShadowEvent>>,
        tools: Option<Arc<ToolRegistry>>,  // 新增参数
    ) -> Self {
        Self { api_key, api_base_url, model, system_prompt, shadow_tx, tools }
    }
    
    pub async fn orchestrate(&self, task: &str) -> Result<CoordinatorResult> {
        // ... 在构造 SubagentConfig 时传入 tools ...
        let config = SubagentConfig {
            // ...
            tools: self.tools.clone(), // 激活！不再是 None
        };
        // ...
    }
}
```

##### Step 3: 在 `delegate_complex_project.rs` 的 `execute()` 中传递 tools

```rust
// execute() 方法内
let tools_clone = self.tools.clone();

let join_handle = tokio::spawn(async move {
    let coordinator = Coordinator::new(
        api_key,
        api_base_url,
        model,
        system_prompt,
        Some(shadow_tx),
        tools_clone,  // 传递 tools
    );
    // ...
});
```

##### Step 4: 在 `main.rs` 注册时传入 tools

```rust
// main.rs — make_tools()

// 先构建 ToolRegistry
let mut tools = ToolRegistry::new();
tools.register_builtin(Box::new(BashTool::new(bash_mode)));
// ... 注册其他工具 ...

// 把 tools 包成 Arc 用于共享
let tools_arc = Arc::new(tools); // ← 需要提前 Arc 化

// 注册 delegate_complex_project 时传入 tools_arc
tools_arc.register_builtin(Box::new(
    DelegateComplexProjectTool::new(
        dispatcher_tx.clone(),
        shadow_tx.clone(),
        api_key.clone(),
        api_base_url.clone(),
        model.clone(),
    ).with_tools(tools_arc.clone())  // 注入工具
));
```

> **⚠️ 循环引用注意**：`ToolRegistry` 包含 `DelegateComplexProjectTool`，而 `DelegateComplexProjectTool` 持有 `Arc<ToolRegistry>`。这不是 Rust 意义上的循环引用（不涉及 Drop 循环），但逻辑上 delegate tool 可以调用自己。
>
> **解决方式**：有两种选择：
>
> 1. **构建顺序分离**：先构建不含 delegate 的 ToolRegistry（`Arc<ToolRegistry>`），作为 subagent 的工具集传入 delegate；然后再把 delegate 注册到主 ToolRegistry。这需要两个 ToolRegistry 实例。
>
> 2. **构建后注入**（推荐）：`make_tools()` 先返回 `ToolRegistry`，然后在外部用 `Arc::get_mut()` 或 interior mutability 注入 tools。但 `ToolRegistry` 当前没有 `register_after_arc()` 方法。
>
> **推荐方案**：创建一个独立的 SubAgent ToolRegistry，只包含 subagent 需要的工具子集（bash, read_file, write_file, file_edit, glob, grep, browser），不包含 delegate_complex_project 自身（防止递归委托）。

```rust
// main.rs

/// 为 SubAgent 创建精简工具集（不含 delegate/cancel 等元工具）
fn make_subagent_tools(
    browser_chrome_path: Option<String>,
    browser_profile_dir: Option<String>,
    browser_headless: bool,
    file_tracker: SharedFileReadTracker,
) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    tools.register_builtin(Box::new(BashTool::new(BashMode::Open)));
    tools.register_builtin(Box::new(ReadFileTool::new(file_tracker.clone())));
    tools.register_builtin(Box::new(WriteFileTool::new(file_tracker.clone())));
    tools.register_builtin(Box::new(FileEditTool::new(file_tracker.clone())));
    tools.register_builtin(Box::new(GlobTool));
    tools.register_builtin(Box::new(GrepTool));
    tools.register_builtin(Box::new(BrowserTool::new(
        browser_chrome_path,
        browser_profile_dir,
        browser_headless,
    )));
    tools
}
```

然后在 `make_tools()` 中：

```rust
let subagent_tools = Arc::new(make_subagent_tools(
    browser_chrome_path.clone(),
    browser_profile_dir.clone(),
    browser_headless,
    file_tracker.clone(),
));

tools.register_builtin(Box::new(
    DelegateComplexProjectTool::new(
        dispatcher_tx.clone(),
        shadow_tx.clone(),
        api_key.clone(),
        api_base_url.clone(),
        model.clone(),
    ).with_tools(subagent_tools)
));
```

### 修改文件清单

| 文件 | 改动 |
|------|------|
| `nova-core/src/tools/delegate_complex_project.rs` | 新增 `tools` 字段、`with_tools()` 方法；`execute()` 中传递 tools 给 Coordinator |
| `nova-core/src/coordinator/orchestrator.rs` | 新增 `tools` 字段；`orchestrate()` 中传递给 SubagentConfig |
| `nova-daemon/src/main.rs` | 新增 `make_subagent_tools()`；注册 delegate 时注入 subagent tools |

### 风险点

1. **资源竞争**：后台 subagent 和主 Agent 可能同时操作 browser。BrowserTool 内部有 session 管理，但未做并发控制。
   - **缓解**：subagent tools 使用独立的 `BrowserTool` 实例（独立 CDP session），不与主 Agent 共享。
   
2. **file_tracker 共享**：SubAgent 和主 Agent 共享同一个 `SharedFileReadTracker`。SubAgent 写文件时可能触发 read-first 检查。
   - **缓解**：SubAgent 的 `file_tracker` 可以用独立实例（`create_shared_tracker()`），或者干脆不做 read-first 检查。

3. **token 消耗**：SubAgent 使用工具后会产生多轮 API 调用，每轮都消耗 token。
   - **缓解**：`run_with_tools_loop()` 已有 `MAX_LOOPS: usize = 20` 安全上限。

---

## 实施顺序建议

1. **先做修复 2**（SubAgent 工具注入）— 改动集中在 3 个文件，不涉及 IPC/TUI 架构
2. **再做修复 1**（完成通知双通道）— 涉及 IPC 协议变更和 TUI 端改动，链路更长

## 验证方法

### 修复 2 验证
1. 重启 daemon
2. 在 TUI 中输入任务触发 `delegate_complex_project`
3. 观察 `daemon.log` 中 SubAgent 阶段是否出现 `Tool call:` 日志

### 修复 1 验证
1. 重启 daemon
2. 在 TUI 中触发委派任务
3. 等待后台完成
4. TUI 应自动显示 `✅ 后台任务完成 (ID: xxx)` 消息
