# **Nova V4 数据模型规范 (Data Model)**

本文档定义了 Nova V4 架构中核心的数据交互模型、持久化格式及枚举类型。

## **1. ShadowEvent (影子事件总线枚举)**

这是贯穿整个系统的核心事件通信协议，用于主 Agent、SubAgent、Heartbeat 等组件向后台 Dispatcher 发送单向通知。

```rust
pub enum ShadowEvent {
    /// 任务进度变更 (来源: Coordinator / SubAgent)
    TaskProgress {
        task_id: String,
        action: TaskAction,
        description: String,
    },
    /// 话题归档 (来源: 状态机拦截器判定 TopicShift 时)
    TopicArchived {
        transcript: Vec<Message>,
    },
    /// 系统闲置 (来源: Heartbeat 定时器)
    SystemIdle {
        duration_secs: u64,
        transcript: Vec<Message>,
    },
    /// 复杂项目完工战报 (来源: Coordinator)
    ProjectCompleted {
        report: String,
        channel_id: String, // 用于 Discord 路由
    },
}

pub enum TaskAction {
    Add,        // 新增任务
    Update,     // 更新任务状态
    Complete,   // 标记完成
    Remove,     // 物理擦除
}
```

## **2. PreFlightCheckResult (前置嗅探判定结构)**

主 Loop 在调用主模型进行回复前，发起的轻量级 API 请求的返回结构（JSON-Mode）。

```json
{
  "topic_shift": false,
  "complexity": "High", // "High" | "Medium" | "Low"
  "reason": "用户要求重构核心引擎，涉及跨文件和状态管理"
}
```
* **定义：** `complexity == "High"` 时，触发拦截机制（只保留 delegate 工具）。`topic_shift == true` 时，触发历史上下文扫描和 TopicArchived 事件。

## **3. Tasks.md 物理看板格式**

明确界定：`Tasks.md` 是一个**临时看板（Dashboard）**，而非只写不删的 WAL（Write-Ahead Log）。

**行格式规范 (Markdown Checklist)：**
```markdown
- [ ] [`task_id`] `description`
```

**生命周期 (阅后即焚)：**
1. Coordinator 创建任务：`- [ ] [T-001] 分析 loop.rs 逻辑`
2. 任务完成：TaskManager 将其更新为 `- [x] [T-001] 分析 loop.rs 逻辑`
3. 擦除：在收到 `SystemIdle` 事件时，TaskManager 扫描文件，将所有 `[x]` 状态的行物理删除，确保该文件只占用极少的 Token 预算。

## **4. delegate_complex_project 工具 Schema**

```json
{
  "name": "delegate_complex_project",
  "description": "当你面对一个多步骤、跨文件的复杂开发任务或深度网页检索任务时，必须调用此工具将任务委托给后台架构团队。调用后你只需安抚用户即可。",
  "parameters": {
    "type": "object",
    "properties": {
      "project_goal": {
        "type": "string",
        "description": "用一句话总结最终想要达成的目标"
      },
      "initial_context": {
        "type": "string",
        "description": "你目前已知的文件路径、背景要求或初始线索"
      }
    },
    "required": ["project_goal", "initial_context"]
  }
}
```
