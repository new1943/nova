# Nova 任务系统工程化改造方案（v2 — Claude Code 架构）

**日期：** 2026-05-08
**基于：** 005-appendix-wechat-article.md 诊断报告 + Claude Code 源码分析
**参考源码：** `/Users/zhenglingbing/Documents/openclaw/projects/claude-code-main/`

---

## 一、问题本质

当前系统的核心缺陷：**subagent 是 fire-and-forget 的，没有生命周期管理。**

具体表现：
1. 任务丢失 — preflight 判断不准，工具被收回，任务派不出去
2. 执行失联 — subagent 从未被真正拉起，tasks.md 只是状态记录
3. 通知绕路 — subagent 完成通知直达用户，主 Agent 完全不知情
4. 结果断联 — 主 Agent 派了任务，自己不知道何时完成、结果是什么

### 为什么之前的 Preflight 方案不行？

之前设计了一个前置分类器（Preflight），用一个 LLM 调用来猜另一个 LLM 该怎么做。
但实践中发现：**preflight 给了 Low 评级 → GateStage 收回工具 → 主 Agent 想派发却没工具可用。**

Claude Code 的做法完全不同：**没有 Preflight，LLM 自己决定一切。** 通过工具描述 + 系统提示词引导 LLM 做出正确的派发决策。

---

## 二、Claude Code 的核心设计（参考）

### 2.1 没有 Preflight，LLM 就是路由器

Claude Code 的数据流极简：

```
用户消息 → LLM 看到所有工具 → LLM 自己决定调哪个工具 → 执行
```

LLM 通过两层 prompt engineering 知道什么时候该派发：

**第一层：Agent 工具描述**
```
Launch a new agent to handle complex, multi-step tasks autonomously.

When NOT to use:
- If you want to read a specific file path, use Read directly
- If searching for a specific class definition, use Glob/Grep directly
- If searching code within 2-3 files, use Read directly
```

**第二层：每个 agent 类型的 whenToUse**
```
Explore agent: "Use only when a simple directed search proves
insufficient or when your task will clearly require more than 3 queries."

Plan agent: "Use this when you need to plan the implementation
strategy for a task."
```

### 2.2 Plan Mode — LLM 自己决定进入

LLM 调用 `EnterPlanMode` 工具进入规划模式。工具描述里写明了什么时候该用：
- New Feature Implementation
- Multiple Valid Approaches
- Multi-File Changes (2-3+ files)
- Unclear Requirements

进入后系统注入约束：`you MUST NOT make any edits, run any non-readonly tools`

### 2.3 通知机制 — 消息队列注入

Claude Code 不用 channel 回调，而是把子任务结果作为 **user message 注入主对话**：

```xml
<task-notification>
<task-id>{taskId}</task-id>
<status>{status}</status>
<result>{finalMessage}</result>
</task-notification>
```

主循环在每个 turn 边界 drain 消息队列，LLM 自然看到子任务结果，自己决定怎么处理。

### 2.4 优先级队列

```
now (0)   — 系统级
next (1)  — 用户输入  ← always drain
later (2) — 任务通知  ← 只在空闲时 drain
```

用户输入永远优先于子任务通知。

### 2.5 /stop 中断

ESC 键 → AbortController.abort() → 当前 LLM 调用中断，回到用户输入。
子任务如果是 sync 模式，共享父级 AbortController，父级中断子任务也中断。
子任务如果是 async 模式，有独立 AbortController，需要显式 kill。

---

## 三、Nova 改造方案

### 3.1 架构总览

```
用户消息 (TUI/Discord)
  │
  ▼
主 Agent（LLM）── 看到所有工具描述 ── 自己决定调哪个工具
  │
  ├─ 简单任务 → 直接用 bash/read_file/write_file 执行
  │
  ├─ 需要派发 → 调用 delegate_task 工具
  │                │
  │                ▼
  │           TaskExecutor.submit()
  │                │
  │                ├─ spawn subagent（sync 或 async）
  │                │   Channel 上下文透传
  │                │
  │                └─ 完成 → 消息注入主 Agent 对话
  │                          LLM 看到结果，决定怎么回复用户
  │
  ├─ 回复带 <topic_shift>true</topic_shift>
  │                │
  │                ▼
  │           调用 memory tool (action: review)
  │                │
  │                ▼
  │           写入 memories/YYYY-MM-DD.md
  │
  └─ /stop → AbortController.abort() → 中断当前任务
```

### 3.2 去掉 Preflight，话题转移由主 Agent 标签驱动

**Preflight 整个删除。** 不再需要前置分类器。

话题转移检测改为主 Agent 在回复中输出标签，turn 结束时解析：

```
主 Agent 回复:
  "... 这是给用户的回答 ...\n\n<nova_meta><topic_shift>false</topic_shift></nova_meta>"
```

解析逻辑加在主循环（discord.rs / main.rs）turn 完成后：

```rust
let topic_shift = extract_tag(&agent_response, "topic_shift")
    .map(|s| s == "true")
    .unwrap_or(false);
```

主 LLM 比 Preflight 看到的上下文更完整（全量对话 vs 滑动窗口），判断更准。
LLM 忘记输出标签时 fallback 为 `false`，和 Preflight 超时的默认行为一致。

GateStage 不再根据 complexity 收工具。**所有工具始终对 LLM 可见。**

### 3.3 delegate_task 工具 — 唯一的派发入口

参考 Claude Code 的 `AgentTool`，重写 delegate_task 的工具描述：

```rust
const DELEGATE_TASK_DESCRIPTION: &str = r#"
将任务委派给后台子 Agent 执行。子 Agent 拥有完整的工具集（bash, read_file,
write_file, grep, glob, browser 等），可以独立完成多步骤任务。

## 什么时候用
- 任务需要 ≥3 轮工具调用（多文件修改、搜索+分析+写入）
- 任务需要长时间运行，你不希望阻塞与用户的对话
- 多个独立任务可以并行执行

## 什么时候不用
- 单步操作（读一个文件、执行一条命令）→ 直接用对应工具
- 简单问答、理论解释 → 直接回答
- 搜索代码（≤3个文件）→ 直接用 grep/glob/read_file

## 参数
- task: 任务描述（清晰、完整，子 Agent 没有你的上下文）
- context: 额外上下文（可选，如相关文件路径、注意事项）

## 行为
- 子 Agent 在后台执行，不阻塞你与用户的对话
- 完成后你会收到通知（task-notification），包含执行结果
- 你可以随时用 /stop 取消正在运行的子任务
"#;
```

### 3.4 TaskExecutor — 生命周期管理器

```rust
// nova-core/src/task_executor.rs

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex};
use tokio::task::AbortHandle;

pub type TaskId = String;

/// 任务状态
#[derive(Debug, Clone)]
pub enum TaskStatus {
    Pending,
    Running { started_at: Instant },
    Completed { output: String, duration: Duration },
    Failed { error: String, duration: Duration },
    TimedOut { duration: Duration },
    Cancelled,
}

/// 执行模式 — 由 LLM 通过 delegate_task 的参数选择（或不选，用默认）
#[derive(Debug, Clone, Default)]
pub enum ExecMode {
    #[default]
    Auto,            // 让 subagent 自己决定（默认）
    PlanAndExecute,  // 先规划再执行
    Parallel(Vec<String>), // 并行多个独立子任务
}

/// 渠道上下文 — spawn 时从 TurnContext 透传，通知时原样带回
#[derive(Debug, Clone)]
pub struct ChannelContext {
    pub platform: Platform,
    pub channel_id: String,
    pub reply_to: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Platform {
    Tui,
    Discord,
}

/// 任务请求
#[derive(Debug)]
pub struct TaskRequest {
    pub id: TaskId,
    pub name: String,
    pub task_prompt: String,           // 给 subagent 的完整 prompt
    pub exec_mode: ExecMode,
    pub timeout: Duration,             // maxTurns 对应的时间兜底
    pub max_turns: usize,              // 工具调用轮数上限（主要限制手段）
    pub tools: Vec<String>,            // subagent 可用工具
    pub channel: ChannelContext,        // 渠道上下文
}

/// 任务通知 — 注入主 Agent 对话
#[derive(Debug, Clone)]
pub struct TaskNotification {
    pub id: TaskId,
    pub name: String,
    pub status: TaskStatus,
    pub output: Option<String>,
    pub channel: ChannelContext,
}

/// TaskExecutor — 任务生命周期管理
pub struct TaskExecutor {
    /// 所有任务状态（可观测）
    registry: Arc<Mutex<HashMap<TaskId, TaskStatus>>>,
    /// 通知注入 channel → 主 Agent 消息队列
    notify_tx: mpsc::Sender<TaskNotification>,
    /// 并发上限
    max_concurrency: usize,
    /// 当前运行数
    running_count: Arc<std::sync::atomic::AtomicUsize>,
    /// AbortHandle 用于 /stop 取消
    handles: Arc<Mutex<HashMap<TaskId, AbortHandle>>>,
}

impl TaskExecutor {
    pub fn new(
        notify_tx: mpsc::Sender<TaskNotification>,
        max_concurrency: usize,
    ) -> Self {
        Self {
            registry: Arc::new(Mutex::new(HashMap::new())),
            notify_tx,
            max_concurrency,
            running_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            handles: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 提交任务 — delegate_task 工具调用此方法
    pub async fn submit(&self, req: TaskRequest) -> TaskId {
        let id = req.id.clone();
        self.registry.lock().await.insert(id.clone(), TaskStatus::Pending);

        if self.running_count.load(std::sync::atomic::Ordering::Relaxed) >= self.max_concurrency {
            // 超过并发上限，返回错误让 LLM 知道
            // （Claude Code 也不排队，超了就告诉用户）
            return id;
        }

        self.spawn_task(req).await;
        id
    }

    /// /stop 命令 — 取消指定任务
    pub async fn stop(&self, id: &TaskId) -> anyhow::Result<()> {
        if let Some(handle) = self.handles.lock().await.remove(id) {
            handle.abort();
            self.registry.lock().await.insert(id.clone(), TaskStatus::Cancelled);
            self.running_count.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            // 注入取消通知
            let _ = self.notify_tx.send(TaskNotification {
                id: id.clone(),
                name: String::new(),
                status: TaskStatus::Cancelled,
                output: None,
                channel: ChannelContext {
                    platform: Platform::Tui,
                    channel_id: String::new(),
                    reply_to: None,
                },
            }).await;
        }
        Ok(())
    }

    /// /stop-all — 取消所有任务
    pub async fn stop_all(&self) {
        let handles: Vec<_> = self.handles.lock().await.drain().collect();
        for (id, handle) in handles {
            handle.abort();
            self.registry.lock().await.insert(id.clone(), TaskStatus::Cancelled);
        }
        self.running_count.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// 查询任务状态
    pub async fn status(&self, id: &TaskId) -> Option<TaskStatus> {
        self.registry.lock().await.get(id).cloned()
    }

    /// 列出所有活跃任务
    pub async fn active_tasks(&self) -> Vec<(TaskId, TaskStatus)> {
        self.registry.lock().await
            .iter()
            .filter(|(_, s)| matches!(s, TaskStatus::Pending | TaskStatus::Running { .. }))
            .map(|(id, s)| (id.clone(), s.clone()))
            .collect()
    }

    /// 内部：spawn subagent
    async fn spawn_task(&self, req: TaskRequest) {
        let id = req.id.clone();
        let name = req.name.clone();
        let channel = req.channel.clone();
        let timeout = req.timeout;
        let notify_tx = self.notify_tx.clone();
        let registry = self.registry.clone();
        let running_count = self.running_count.clone();
        let handles = self.handles.clone();

        running_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        registry.lock().await.insert(id.clone(), TaskStatus::Running { started_at: Instant::now() });

        let task_registry = registry.clone();
        let task_id = id.clone();
        let task_name = name.clone();
        let task_channel = channel.clone();
        let task_notify_tx = notify_tx.clone();
        let task_running = running_count.clone();

        let handle = tokio::spawn(async move {
            let start = Instant::now();

            // ---- 调用 SubagentSpawner 执行 ----
            let output = super::subagent::SubagentSpawner::run_with_tools_loop(
                &req.task_prompt,
                &req.tools,
                req.max_turns,
            ).await;

            let duration = start.elapsed();
            let (status, result_output) = match output {
                Ok(out) => (TaskStatus::Completed { output: out.clone(), duration }, Some(out)),
                Err(e) => (TaskStatus::Failed { error: e.to_string(), duration }, None),
            };

            task_registry.lock().await.insert(task_id.clone(), status.clone());

            // 注入通知到主 Agent 消息队列
            let _ = task_notify_tx.send(TaskNotification {
                id: task_id,
                name: task_name,
                status,
                output: result_output,
                channel: task_channel,
            }).await;

            task_running.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        });

        handles.lock().await.insert(id.clone(), handle.abort_handle());

        // Watchdog — 时间兜底（主要靠 maxTurns，这是安全网）
        let wd_id = id.clone();
        let wd_registry = registry.clone();
        let wd_notify = notify_tx.clone();
        let wd_channel = channel.clone();
        let wd_handles = handles.clone();
        let wd_running = running_count.clone();

        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            if let Some(h) = wd_handles.lock().await.remove(&wd_id) {
                h.abort();
            }
            wd_registry.lock().await.insert(wd_id.clone(), TaskStatus::TimedOut { duration: timeout });
            let _ = wd_notify.send(TaskNotification {
                id: wd_id,
                name,
                status: TaskStatus::TimedOut { duration: timeout },
                output: None,
                channel: wd_channel,
            }).await;
            wd_running.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        });
    }
}
```

### 3.5 通知机制 — 消息注入（不是 channel 回调）

参考 Claude Code 的全局消息队列，子任务完成后的通知**注入为主 Agent 对话中的 user message**：

```
之前（错误）：
  subagent 完成 → ShadowEvent → Discord 直达用户（主 Agent 不知道）

Claude Code 方案：
  subagent 完成 → 消息队列 → 主循环 drain → 注入为 user message → LLM 自然看到

我们的实现：
  subagent 完成 → TaskExecutor.notify_tx → 消息队列 → 主 Agent drain → 注入对话
```

**注入格式**（参考 Claude Code 的 `<task-notification>` XML）：

```rust
fn format_notification(n: &TaskNotification) -> String {
    match &n.status {
        TaskStatus::Completed { output, duration } => {
            format!(
                "<task-notification>\n<task-id>{}</task-id>\n<name>{}</name>\n\
                 <status>completed</status>\n<duration>{:.1}s</duration>\n\
                 <result>{}</result>\n</task-notification>",
                n.id, n.name, duration.as_secs_f64(),
                output.as_deref().unwrap_or("(no output)")
            )
        }
        TaskStatus::TimedOut { duration } => {
            format!(
                "<task-notification>\n<task-id>{}</task-id>\n<name>{}</name>\n\
                 <status>timed_out</status>\n<duration>{:.1}s</duration>\n\
                 <result>任务超时已终止</result>\n</task-notification>",
                n.id, n.name, duration.as_secs_f64()
            )
        }
        TaskStatus::Failed { error, .. } => {
            format!(
                "<task-notification>\n<task-id>{}</task-id>\n<name>{}</name>\n\
                 <status>failed</status>\n<result>{}</result>\n</task-notification>",
                n.id, n.name, error
            )
        }
        TaskStatus::Cancelled => {
            format!(
                "<task-notification>\n<task-id>{}</task-id>\n<name>{}</name>\n\
                 <status>cancelled</status>\n</task-notification>",
                n.id, n.name
            )
        }
        _ => String::new(),
    }
}
```

**主循环注入**（AgentLoop）：

```rust
impl AgentLoop {
    pub async fn run(&mut self) {
        loop {
            tokio::select! {
                // 用户输入（高优先级）
                msg = self.platform_rx.recv() => {
                    self.handle_user_message(msg).await;
                }

                // 子任务通知（低优先级，只在用户输入间隙处理）
                // 注意：不用 biased select，让 tokio 随机选
                // 但用户输入 channel 通常先有数据
                Some(notification) = self.notify_rx.recv() => {
                    let text = format_notification(&notification);
                    // 注入为 user message，LLM 下一轮会看到
                    self.inject_user_message(text, &notification.channel).await;
                }
            }
        }
    }
}
```

### 3.6 /stop 命令

TUI 和 Discord 都暴露 `/stop` 命令：

```rust
// 用户输入 "/stop" 或 "/stop <task_id>"
// Discord: 发送消息 "/stop"
// TUI: 输入 "/stop"

match parse_command(&user_input) {
    Some("/stop") => {
        // 停止当前所有任务
        self.task_executor.stop_all().await;
        // 中断当前 LLM 调用
        self.abort_controller.abort();
        self.reply("所有任务已停止。").await;
    }
    Some("/stop <id>") => {
        self.task_executor.stop(&task_id).await;
        self.reply(&format!("任务 {} 已停止。", task_id)).await;
    }
    Some("/tasks") => {
        // 列出活跃任务
        let tasks = self.task_executor.active_tasks().await;
        self.reply(&format_active_tasks(&tasks)).await;
    }
    _ => { /* 正常处理 */ }
}
```

### 3.7 渠道上下文透传

```
TurnContext (platform + channel_id)
    → delegate_task 工具
        → TaskRequest.channel
            → TaskNotification.channel
                → platform.send(channel_id, notification)
```

主 Agent 收到通知时，从 `notification.channel` 知道该回复到哪。

### 3.8 Sync vs Async 子任务

参考 Claude Code 的两种模式：

| 模式 | 行为 | 适用场景 |
|------|------|---------|
| **Sync** | 主 Agent 阻塞等待结果 | 简单任务，用户在等答案 |
| **Async**（默认） | 主 Agent 立即返回，后台执行 | 复杂任务，不阻塞对话 |

delegate_task 默认走 Async。主 Agent 调用后立即得到：

```
任务 abc123 已提交（后台执行中）。完成后会通知你。
你可以用 /stop 取消，用 /tasks 查看状态。
```

子任务完成后，通知注入主 Agent 对话，LLM 决定如何回复用户。


### 3.9 记忆系统 — 封装为 memory tool

#### 设计思路

把记忆系统封装为一个 tool（参考 Hermes 的 memory tool），主 Agent 通过调用工具完成所有记忆操作。
不再需要 ShadowEvent、MemoryKeeper、SideQuery 这套事件链路。

#### 架构对比

```
之前（事件链路）：
  topic_shift → ShadowEvent → MemoryKeeper buffer → 等 SystemIdle → SideQuery → 写 memories/
  4 个组件，3 次跳转，等空闲才执行

之后（tool 封装）：
  主 Agent 调用 memory tool → tool 内部：加载文件 + 调 LLM + 写入 → 返回结果
  1 个 tool，0 跳转，立即执行
```

#### memory tool 定义

```rust
/// 记忆工具 — 封装所有记忆操作
pub struct MemoryTool {
    workspace_dir: PathBuf,  // ~/.nova/
    llm: Arc<dyn LlmBackend>,
}

/// 工具参数
/// {
///   "action": "review" | "save" | "load",
///   // review: 对当前对话做摘要，写入 memories/YYYY-MM-DD.md
///   // save:   保存一条持久记忆到 MEMORY.md
///   // load:   读取记忆文件，返回给主 Agent
///   "content": "...",        // save 时的记忆内容
///   "date": "2026-05-08"    // load 时指定日期（可选，默认今天）
/// }
```

#### action: review（话题转移触发）

主 Agent 检测到话题转移后，调用 memory tool 的 review action。
tool 内部完成：加载对话 → 调 LLM 提炼摘要 → 写入每日文件。

```rust
async fn review(&self, transcript: &[Message]) -> Result<String> {
    // 1. 格式化对话
    let conversation = format_transcript(transcript);

    // 2. 调 LLM 提炼摘要
    let summary = self.llm.complete(&CompletionRequest {
        system: DAILY_SUMMARY_GUIDANCE.to_string(),
        messages: vec![conversation],
        ..Default::default()
    }).await?;

    // 3. 写入 memories/YYYY-MM-DD.md
    let today = Local::now().format("%Y-%m-%d").to_string();
    let path = self.workspace_dir.join("memories").join(format!("{}.md", today));
    let timestamp = Local::now().format("%H:%M:%S");
    let entry = format!("\n## {} — Session Summary\n\n{}\n", timestamp, summary);
    append_to_file(&path, &entry).await?;

    // 4. 返回确认
    Ok(format!("已记录到 memories/{}", today))
}
```

#### action: save（用户说"记住这个"）

```rust
async fn save(&self, content: &str) -> Result<String> {
    let path = self.workspace_dir.join("MEMORY.md");
    append_to_file(&path, &format!("- {}\n", content)).await?;
    Ok("已保存到 MEMORY.md".into())
}
```

#### action: load（新会话启动时加载）

```rust
async fn load(&self, date: Option<&str>) -> Result<String> {
    let date = date.unwrap_or(&Local::now().format("%Y-%m-%d").to_string());
    let path = self.workspace_dir.join("memories").join(format!("{}.md", date));
    match fs::read_to_string(&path).await {
        Ok(content) => Ok(content),
        Err(_) => Ok(format!("{} 无记忆记录", date)),
    }
}
```

#### 工具描述（给 LLM 看）

```rust
const MEMORY_TOOL_DESCRIPTION: &str = r#"
记忆工具，用于管理对话记忆和持久知识。

## actions
- review: 对当前对话做摘要，写入今日记忆文件。在话题转移时使用。
- save: 保存一条持久记忆（用户偏好、避坑指南、技术决策）到 MEMORY.md。
- load: 读取指定日期的记忆文件。默认读取今天。

## 什么时候用 review
- 话题明显转移（换了一个完全不同的主题）
- 一段对话自然结束，想记录要点

## 什么时候用 save
- 用户说"记住这个"、"以后注意"
- 发现重要的用户偏好、项目决策、避坑经验
- 修复了一个值得记录的 bug

## 什么时候用 load
- 新会话开始，需要了解今天的上下文
- 需要回忆之前讨论过什么
"#;
```

#### 提示词（待定）

```rust
// TODO: 最终提示词待讨论
const DAILY_SUMMARY_GUIDANCE: &str = r#"请分析以下对话片段，生成一段简洁的会话摘要。

## 要求
- 记录：讨论了什么主题、做了什么决策、遇到了什么问题、得出了什么结论
- 不记录：具体的工具调用细节、中间调试过程
- 格式：以时间戳开头，一段话概括
- 语言：使用用户对话所用的语言

示例格式：
## HH:MM:SS — Session Summary

用户讨论了XXX问题，最终决定YYY。过程中发现ZZZ的bug，记录为待修复。
"#;
```

#### 新会话加载策略（待定）

spawn 新会话时，主 Agent 调用 `memory tool (action: load)` 加载记忆。
具体加载哪些文件、注入到 system prompt 还是作为 tool result，待讨论。

参考方案：
- 启动时自动调用 load（今天）+ load（MEMORY.md）
- 可选：加载最近 N 天

#### 删除的组件

不再需要：
- `nova-memory/src/sidequery/` — MemoryKeeper + SideQuery
- `nova-core/src/models/events.rs` — `ShadowEvent::TopicArchived` 事件
- `nova-daemon/src/dispatcher.rs` — TopicArchived 路由逻辑

---

## 四、与之前方案的对比

| 维度 | 之前方案（Preflight 驱动） | 现在方案（LLM 自主决策） |
|------|--------------------------|------------------------|
| **谁决定派发** | Preflight 分类器 | 主 Agent LLM 自己 |
| **工具可见性** | GateStage 根据 complexity 过滤 | 所有工具始终可见 |
| **额外 API 调用** | 每轮 1 次 preflight | 0 次 |
| **灵活性** | 低（被分类器框死） | 高（LLM 灵活判断） |
| **可靠性** | 分类器错误 → 工具被收回 → 任务丢失 | 工具始终在，LLM 总能派发 |
| **通知机制** | channel 回调 | 消息注入（LLM 自然看到） |
| **中断能力** | 无 | /stop 命令 |
| **话题转移检测** | Preflight LLM 调用 | 主 Agent 回复标签解析 |
| **记忆系统** | 事件链路（ShadowEvent → MemoryKeeper → SideQuery） | memory tool 封装（1 个 tool 搞定） |
| **记忆写入** | MEMORY.md | memories/YYYY-MM-DD.md（每日摘要）+ MEMORY.md（持久记忆） |

---

## 五、改动范围汇总

| 文件 | 类型 | 改什么 |
|------|------|--------|
| `nova-core/src/preflight_types.rs` | **删除** | Preflight 整个去掉 |
| `nova-agent/src/preflight.rs` | **删除** | Preflight 整个去掉 |
| `nova-agent/src/stages/classify.rs` | **删除** | 不再需要 ClassifyStage |
| `nova-agent/src/stages/gate.rs` | **删除** | 不再过滤工具 |
| `nova-agent/src/stages/execute_config.rs` | **删除** | 不再根据 complexity 设置 terminate_after_tool |
| `nova-core/src/pipeline.rs` | 简化 | TurnPipeline 去掉 Classify/Gate/ExecuteConfig Stage |
| `nova-core/src/task_executor.rs` | **新增** | TaskExecutor + TaskStatus + TaskRequest + TaskNotification + ChannelContext |
| `nova-tools/src/memory_tool.rs` | **新增** | MemoryTool（review/save/load） |
| `nova-core/src/lib.rs` | 修改 | 导出 task_executor 模块 |
| `nova-agent/src/delegate/delegate_task.rs` | 重写 | 工具描述改为 Claude Code 风格，通过 TaskExecutor.submit() 派发 |
| `nova-agent/src/delegate/delegate_complex_project.rs` | 重写 | 同上 |
| `nova-tools/src/delegate_base.rs` | 重写 | 去掉 RUNNING_PROJECTS，改用 TaskExecutor |
| `nova-agent/src/agent_loop.rs` | 修改 | 加 notify_rx 消息注入 + /stop 命令处理 |
| `nova-daemon/src/dispatcher.rs` | 修改 | 持有 TaskExecutor，去掉 TopicArchived 路由 |
| `nova-daemon/src/tool_factory.rs` | 修改 | 传 TaskExecutor 给 delegate 工具，注册 MemoryTool |
| `nova-daemon/src/discord.rs` | 修改 | 处理 /stop 和 /tasks 命令 + topic_shift 标签解析 |
| `nova-daemon/src/main.rs` | 修改 | 同上（TUI 路径） |
| `nova-memory/src/sidequery/` | **删除** | MemoryKeeper + SideQuery 不再需要 |
| `nova-core/src/models/events.rs` | 简化 | 去掉 ShadowEvent::TopicArchived |

---

## 六、实施顺序

| 步骤 | 内容 | 风险 |
|------|------|------|
| 1 | 删除 Preflight 相关（preflight_types.rs, preflight.rs, classify.rs, gate.rs, execute_config.rs） | 低 |
| 2 | 主 Agent 加 topic_shift 标签输出 | 低 |
| 3 | 新增 MemoryTool（review/save/load） | 低，独立新文件 |
| 4 | discord.rs / main.rs 解析 topic_shift 标签 → 调用 memory tool review | 低 |
| 5 | 删除 MemoryKeeper + SideQuery | 低 |
| 6 | 新增 TaskExecutor 核心模块 | 低，独立新文件 |
| 7 | 重写 delegate_task 工具描述 + 接入 TaskExecutor | 中 |
| 8 | AgentLoop 加 notify_rx 消息注入 | 中 |
| 9 | 加 /stop 命令 | 低 |
| 10 | Dispatcher 集成 | 中 |

每步可独立编译验证。

---

## 七、/stop 命令交互示例

### TUI
```
User: 帮我重构整个鉴权模块
Nova:  [调用 delegate_task]
       任务 a3f8b2c1 已提交（后台执行中）。完成后会通知你。
       你可以用 /stop 取消，用 /tasks 查看状态。

User: /tasks
Nova:  活跃任务：
       - a3f8b2c1 "重构鉴权模块" — Running (已运行 2m30s)

User: /stop a3f8b2c1
Nova:  任务 a3f8b2c1 已停止。

User: /stop
Nova:  所有任务已停止。
```

### Discord
```
用户: 帮我写一份碳中和报告
Bot:   任务 7e2d1f09 已提交（后台执行中）。完成后会通知你。
       [自动回复到 Discord channel]

       ... 5 分钟后 ...

       <task-notification>
       <task-id>7e2d1f09</task-id>
       <name>写碳中和报告</name>
       <status>completed</status>
       <duration>312.5s</duration>
       <result>## 碳中和行业报告\n\n### 一、定义...</result>
       </task-notification>
       [自动回复到同一个 Discord channel]
```

---

## 八、关键设计决策

### 8.1 为什么去掉 Preflight 的决策职能？

实践证明：
- Preflight 判断错误 → 工具被收回 → 任务丢失（比不判断更糟）
- Claude Code 验证了：LLM 通过工具描述自主决策是可行的
- 减少一次 API 调用，降低延迟

### 8.2 为什么通知注入而不是 channel 回调？

参考 Claude Code 的设计：
- 注入为 user message → LLM 自然看到结果，灵活决定回复方式
- channel 回调 → 程序硬编码处理，不灵活
- 注入方式还能携带渠道信息，LLM 知道回复到哪

### 8.3 为什么 maxTurns 而不是时间超时？

参考 Claude Code：
- LLM 任务时间不可预测（网络慢 vs 本地快）
- maxTurns 限制工具调用轮数，是更精确的控制
- 时间超时作为安全网兜底

### 8.4 为什么 /stop 而不是自动中断？

- 用户随时可能改变主意
- /stop 语义清晰，和 Claude Code 的 ESC 对应
- 同时支持停止单个任务和停止所有任务

### 8.5 为什么记忆封装为 tool 而不是事件链路？

参考 Hermes 的 memory tool 设计：
- 1 个 tool 封装所有操作（review/save/load），比 4 个组件的事件链路清晰得多
- LLM 自己决定什么时候 review，不需要自动触发机制
- tool 内部封装：加载文件 → 调 LLM → 写入 → 返回结果，调用方无需关心细节
- 可组合：LLM 可以 review + save 组合使用

### 8.6 为什么话题转移写 memories/ 而不是 MEMORY.md？

- `memories/YYYY-MM-DD.md` 是每日对话摘要，和 /new、压缩的归档逻辑一致
- `MEMORY.md` 是精炼的持久记忆（用户偏好、避坑指南），应由特定条件或手动维护
- 话题转移触发的是"记录刚才聊了什么"，不是"提炼持久知识"
- 两层分离：daily log vs curated knowledge
