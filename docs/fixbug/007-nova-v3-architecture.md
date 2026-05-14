# Nova v3 架构方案

**日期：** 2026-05-08
**参考：** Claude Code 源码、Hermes Agent、微信公众号 Agent 执行模式文章

---

## 一、核心思路

整个系统就是一个 **Main Loop + Tool Registry**。

```
loop {
    msg = recv();                        // 用户消息 或 子任务通知
    response = llm.call(msg, &tools);    // LLM 看到所有工具，自己决定调哪个
    for call in response.tool_calls {
        result = tools.execute(call);    // 执行工具，结果回到下一轮 LLM
    }
}
```

没有 Preflight。没有 Pipeline Stage。没有 ShadowEvent。LLM 就是路由器。

---

## 二、工具集

```
┌─────────────────────────────────────────────────────────┐
│                     Tool Registry                        │
│                                                         │
│  ┌─ 同步调用 ─────────────────────────────────────────┐ │
│  │  bash  read_file  write_file  grep  glob  browser  │ │
│  └────────────────────────────────────────────────────┘ │
│                                                         │
│  ┌─ 封装工具 ─────────────────────────────────────────┐ │
│  │  memory { review, save, load }                     │ │
│  │  skills { list, run }                              │ │
│  └────────────────────────────────────────────────────┘ │
│                                                         │
│  ┌─ 执行模式工具（spawn subagent）────────────────────┐ │
│  │  execute_react         边查边想                     │ │
│  │  execute_chain         固定步骤                     │ │
│  │  execute_parallel      并行子任务                   │ │
│  │  execute_with_review   执行 + 自检                  │ │
│  │  execute_project       规划→执行→验收→循环          │ │
│  └────────────────────────────────────────────────────┘ │
│                                                         │
│  ┌─ 系统命令 ─────────────────────────────────────────┐ │
│  │  /stop  /tasks                                     │ │
│  └────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────┘
```

### 2.1 同步调用

直接执行，返回结果。LLM 看到结果后决定下一步。

```
bash          — 执行 shell 命令
read_file     — 读取文件
write_file    — 写入文件
edit_file     — 编辑文件
grep          — 搜索文本
glob          — 搜索文件名
browser       — 浏览器操作
```

### 2.2 封装工具

内部可能调 LLM、读写文件，但对外就是一个 tool call。

#### memory tool

```json
{
  "action": "review" | "save" | "load",
  "content": "...",
  "date": "2026-05-08"
}
```

| action | 触发时机 | 内部行为 | 写入目标 |
|--------|---------|---------|---------|
| review | 话题转移 | 加载对话 → 调 LLM 提炼摘要 | memories/YYYY-MM-DD.md |
| save | 用户说"记住这个" | 直接写入 | MEMORY.md |
| load | 新会话启动 / 需要回忆 | 读取文件返回 | 无（只读） |

工具描述：
```
记忆工具，用于管理对话记忆和持久知识。

- review: 对当前对话做摘要，写入今日记忆文件。话题转移时使用。
- save: 保存持久记忆（用户偏好、避坑指南、技术决策）到 MEMORY.md。
- load: 读取指定日期的记忆文件。默认今天。

When to use review:
- 话题明显转移（换了一个完全不同的主题）
- 一段对话自然结束，想记录要点

When to use save:
- 用户说"记住这个"、"以后注意"
- 发现重要的用户偏好、项目决策、避坑经验

When to use load:
- 新会话开始，需要了解今天的上下文
- 需要回忆之前讨论过什么
```

#### skills tool

```json
{
  "action": "list" | "run",
  "skill_name": "...",
  "args": "..."
}
```

### 2.3 执行模式工具

都是 spawn subagent，区别在于编排方式。

#### execute_react — 边查边想

```
适用：调试、调研、探索性任务。步骤少（<5步），不确定下一步。
行为：单 subagent，带工具循环，每步推理后决定下一步。
内部：max_iterations=100，有完整工具集。
```

工具描述：
```
边查边想，每步结果决定下一步。适合调试、调研、探索性任务。

When to use:
- 步骤少（<5步），需要边查边想
- 不确定下一步是什么，要看中间结果
- 调试报错、调研问答、探索性任务

When NOT to use:
- 步骤固定无分支 → execute_chain
- 多个独立任务可同时做 → execute_parallel
- 精度要求极高需要自检 → execute_with_review
- 步骤多（>5步）需要规划 → execute_project
```

#### execute_chain — 固定步骤

```
适用：流水线任务。步骤固定，无分支，无"如果...那么..."。
行为：单 subagent，顺序执行，无循环。
内部：按步骤列表依次执行，每步只执行一次。
```

工具描述：
```
固定步骤，顺序执行，无分支。适合流水线任务。

When to use:
- 步骤完全固定，无任何分支
- "先查天气，再推荐衣服"这种直线任务
- 每天定时执行的数据处理流程

When NOT to use:
- 需要根据中间结果调整 → execute_react
- 步骤间有依赖但可规划 → execute_project
```

#### execute_parallel — 并行子任务

```
适用：多个独立子任务，互不依赖，可以同时做。
行为：同时 spawn N 个 subagent，各自独立执行，汇总结果。
内部：tokio::join! 或 FuturesUnordered。
```

工具描述：
```
多个独立子任务同时执行。适合同时查询多个数据源。

When to use:
- 子任务之间完全无依赖
- 同时查 iPhone 和小米的参数
- 同时查北京和上海的天气

When NOT to use:
- 步骤间有依赖（先查A再用A的结果查B）→ execute_react
```

#### execute_with_review — 执行 + 自检

```
适用：精度要求极高。生成结果后换视角审查，发现错误则修正。
行为：执行 → review → 修正，最多 N 轮。
内部：先 spawn executor subagent，再 spawn reviewer subagent，
     reviewer 发现问题则重新执行，直到通过或达到轮次上限。
```

工具描述：
```
执行 + 自我审查。生成结果后换一个视角审视，发现错误则修正。

When to use:
- 精度要求极高（代码生成、合同条款、技术方案）
- 结果正确性比速度重要

When NOT to use:
- 实时聊天，用户等不了
- 简单任务不需要自检 → execute_chain 或 execute_react
```

#### execute_project — 规划→执行→验收→循环

```
适用：复杂项目（>10步），跨文件，需要多轮协调。
行为：自动编排循环，直到所有步骤完成。
内部：一个协调器 subagent 管理整个流程。
```

工具描述：
```
复杂项目协调器。自动编排：规划→执行→验收→循环，直到所有步骤完成。

When to use:
- 步骤多（>10步），需要提前规划
- 跨文件重构、完整功能实现、行业报告
- 需要多轮执行+验收

行为：
1. 规划：拆分任务为步骤列表
2. 执行：逐条执行每个步骤
3. 验收：检查步骤是否完成
4. 检查：还有没有剩余步骤？
   → 有：回到 2
   → 没有：返回最终结果

When NOT to use:
- 步骤少（<5步）且不确定 → execute_react
- 步骤固定无分支 → execute_chain
```

### 2.4 注意事项：防止递归调用

**子 Agent 的工具集不包含执行模式工具。** 这是在工具注册层面切断的，不是靠 LLM 自觉。

```
主 Agent 的工具：bash, read_file, ..., execute_react, execute_parallel, execute_project, ...
子 Agent 的工具：bash, read_file, ..., memory
子 Agent 看不到：execute_react, execute_parallel, execute_project, ...
```

```
主 Agent → 调用 execute_project → spawn 子 Agent
                                      ├─ 只能用 bash/read_file/grep...
                                      ├─ 不能调 execute_react 等（工具列表里没有）
                                      └─ 干完 → 通知回到主 Agent
```

这是 Claude Code 的做法——`AgentTool` 的 subagent 不会拿到 `AgentTool` 自己。
死循环在工具注册层面就切断了，不需要运行时检查。

---

## 三、子 Agent 生命周期

### 3.1 统一底层

所有执行模式工具共享同一个 Executor：

```rust
struct Executor {
    thread_pool: ThreadPool,
    llm: Arc<dyn LlmBackend>,
    tools: ToolRegistry,        // 子 agent 可用的工具（不含执行模式工具，防止递归）
    notify_tx: mpsc::Sender<Notification>,
}

impl Executor {
    async fn react(&self, task: &str, ctx: &ChannelContext) -> TaskId;
    async fn chain(&self, task: &str, ctx: &ChannelContext) -> TaskId;
    async fn parallel(&self, tasks: Vec<String>, ctx: &ChannelContext) -> TaskId;
    async fn with_review(&self, task: &str, ctx: &ChannelContext) -> TaskId;
    async fn project(&self, goal: &str, ctx: &ChannelContext) -> TaskId;
}
```

每个方法返回 TaskId（立即返回，不阻塞），结果通过通知回传。

### 3.2 状态机

```
Pending → Running → Completed(output)
                  → Failed(error)
                  → TimedOut(duration)
                  → Cancelled(by /stop)
```

### 3.3 渠道上下文透传

spawn subagent 时从当前对话携带渠道信息，通知时原样带回：

```rust
struct ChannelContext {
    platform: Platform,       // Tui | Discord
    channel_id: String,       // Discord channel_id 或 TUI session_id
    reply_to: Option<String>, // 原始消息 ID
}
```

```
用户消息 (channel_id) → 工具调用 → TaskRequest.channel → 通知.channel → 回复到原渠道
```

---

## 四、通知机制

### 4.1 消息注入（不是 channel 回调）

参考 Claude Code 的全局消息队列。子任务完成后的通知**注入为主 Agent 对话中的 user message**。

```
subagent 完成
  → 格式化为 <task-notification> XML
    → 入队（priority: later）
      → 主循环 turn 边界 drain
        → 注入为 user message
          → LLM 下一轮自然看到
```

### 4.2 通知格式

```xml
<task-notification>
<tool>execute_react</tool>
<task-id>a3f8b2c1</task-id>
<name>调试空指针错误</name>
<status>completed</status>
<duration>45.2s</duration>
<result>问题出在第42行，未检查 null 返回值。已修复。</result>
</task-notification>
```

### 4.3 优先级

```
now (0)   — 系统级
next (1)  — 用户输入  ← always drain
later (2) — 任务通知  ← 只在用户输入间隙 drain
```

用户输入永远优先于子任务通知。

---

## 五、/stop 命令

### 5.1 命令

| 命令 | 行为 |
|------|------|
| `/stop` | 停止所有活跃任务 + 中断当前 LLM 调用 |
| `/stop <task_id>` | 停止指定任务 |
| `/tasks` | 列出所有活跃任务 |

### 5.2 实现

```rust
match parse_command(&user_input) {
    "/stop" => {
        executor.stop_all().await;
        abort_controller.abort();
        reply("所有任务已停止。");
    }
    "/stop <id>" => {
        executor.stop(&id).await;
        reply("任务已停止。");
    }
    "/tasks" => {
        let tasks = executor.active_tasks().await;
        reply(&format_tasks(&tasks));
    }
}
```

### 5.3 AbortController

- 主 Agent 有自己的 AbortController
- sync subagent 共享父级 AbortController（父停子停）
- async subagent 有独立 AbortController（需要 /stop 显式 kill）

---

## 六、Main Loop 完整结构

```rust
struct AgentLoop {
    llm: Arc<dyn LlmBackend>,
    tools: ToolRegistry,
    executor: Executor,
    notify_rx: mpsc::Receiver<Notification>,
    abort: AbortController,
    channel: ChannelContext,
}

impl AgentLoop {
    async fn run(&mut self) {
        loop {
            tokio::select! {
                // 用户输入
                msg = self.platform_rx.recv() => {
                    // 系统命令处理
                    if let Some(cmd) = parse_command(&msg) {
                        self.handle_command(cmd).await;
                        continue;
                    }

                    // LLM 调用
                    let response = self.llm.call(
                        &self.build_messages(&msg),
                        &self.tools.schemas(),
                    ).await;

                    // 执行工具调用
                    for call in response.tool_calls {
                        let result = self.tools.execute(&call).await;
                        // 结果追加到消息历史
                        self.messages.push(tool_result(call.id, result));
                    }

                    // 回复用户
                    self.send(&response.text).await;
                }

                // 子任务通知
                Some(notification) = self.notify_rx.recv() => {
                    let text = format_notification(&notification);
                    // 注入为 user message，LLM 下一轮看到
                    self.messages.push(user_message(text));
                    // 同时通知用户（可选）
                    self.send_to_channel(&notification.channel, &text).await;
                }
            }
        }
    }
}
```

---

## 七、消息注入优先级

用户输入和子任务通知可能同时到达。用 `biased tokio::select!` 保证用户输入优先：

```rust
tokio::select! {
    biased;  // 按顺序检查，第一个 ready 的 wins

    msg = self.platform_rx.recv() => { /* 处理用户输入 */ }
    Some(n) = self.notify_rx.recv() => { /* 处理子任务通知 */ }
}
```

---

## 八、与 Claude Code 的对比

| 维度 | Claude Code | Nova v3 |
|------|------------|---------|
| **Main Loop** | query.ts 的 while 循环 | AgentLoop::run() |
| **工具注册** | getAllBaseTools() | ToolRegistry |
| **Agent 派发** | AgentTool（一个入口，多种 agent type） | 5 个独立工具（每个对应一种模式） |
| **通知** | 消息队列注入 user message | 同 |
| **中断** | ESC → AbortController | /stop → AbortController |
| **记忆** | 无内置（通过 hooks） | memory tool（review/save/load） |
| **Plan 模式** | EnterPlanMode 系统状态切换 | execute_project 工具（无系统状态） |
| **验证** | verification agent | execute_with_review 工具 |

### 关键差异

1. **执行模式显式化**：Claude Code 用一个 `AgentTool` + 多种 agent_type；Nova 用 5 个独立工具，LLM 直接选模式
2. **Plan 不是系统行为**：Claude Code 的 Plan Mode 切换权限模式；Nova 的 `execute_project` 就是一个工具
3. **内置记忆**：Claude Code 没有内置记忆工具；Nova 有 memory tool

---

## 九、改动范围

| 文件 | 类型 | 改什么 |
|------|------|--------|
| `nova-core/src/preflight_types.rs` | **删除** | Preflight 整个去掉 |
| `nova-agent/src/preflight.rs` | **删除** | Preflight 整个去掉 |
| `nova-agent/src/stages/` | **删除** | ClassifyStage, GateStage, ExecuteConfigStage 全部去掉 |
| `nova-core/src/pipeline.rs` | **删除或大幅简化** | TurnPipeline 不再需要 |
| `nova-core/src/executor.rs` | **新增** | Executor（线程池 + 5 种执行模式） |
| `nova-core/src/executor/types.rs` | **新增** | TaskStatus, TaskRequest, Notification, ChannelContext |
| `nova-core/src/executor/react.rs` | **新增** | execute_react 实现 |
| `nova-core/src/executor/chain.rs` | **新增** | execute_chain 实现 |
| `nova-core/src/executor/parallel.rs` | **新增** | execute_parallel 实现 |
| `nova-core/src/executor/review.rs` | **新增** | execute_with_review 实现 |
| `nova-core/src/executor/project.rs` | **新增** | execute_project 实现 |
| `nova-tools/src/memory_tool.rs` | **新增** | MemoryTool（review/save/load） |
| `nova-tools/src/registry.rs` | 修改 | 注册新工具 |
| `nova-agent/src/agent_loop.rs` | 重写 | Main Loop + notify_rx + /stop |
| `nova-daemon/src/dispatcher.rs` | 简化 | 去掉 ShadowEvent 路由，持有 Executor |
| `nova-daemon/src/tool_factory.rs` | 修改 | 注册执行模式工具 + memory tool |
| `nova-daemon/src/discord.rs` | 修改 | /stop + /tasks 命令 + 通知注入 |
| `nova-daemon/src/main.rs` | 修改 | 同上（TUI 路径） |
| `nova-memory/src/sidequery/` | **删除** | MemoryKeeper + SideQuery 不再需要 |
| `nova-core/src/models/events.rs` | 简化 | 去掉 ShadowEvent::TopicArchived |
| `nova-agent/src/delegate/` | **删除** | delegate_task + delegate_complex_project 由执行模式工具替代 |
| `nova-tools/src/delegate_base.rs` | **删除** | RUNNING_PROJECTS 由 Executor 替代 |

---

## 十、实施顺序

| 步骤 | 内容 | 依赖 |
|------|------|------|
| 1 | 新增 Executor 核心（types + 线程池 + 状态机） | 无 |
| 2 | 新增 execute_chain（最简单的执行模式） | 步骤 1 |
| 3 | 新增 execute_react | 步骤 1 |
| 4 | 新增 execute_parallel | 步骤 1 |
| 5 | 新增 execute_with_review | 步骤 1 |
| 6 | 新增 execute_project | 步骤 1 |
| 7 | 新增 memory tool | 无 |
| 8 | 重写 AgentLoop（main loop + notify_rx + /stop） | 步骤 1-7 |
| 9 | 简化 Dispatcher | 步骤 8 |
| 10 | 更新 Discord + TUI | 步骤 8 |
| 11 | 删除旧代码（Preflight, Pipeline, delegate, SideQuery） | 步骤 8-10 |

每步可独立编译验证。步骤 1-7 是新增代码，不破坏现有功能。
