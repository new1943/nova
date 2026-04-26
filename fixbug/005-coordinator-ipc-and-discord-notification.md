# Bug Fix #005: Coordinator IPC 通知与 Discord 推送问题

**日期**: 2026-04-26
**严重程度**: 🔴 中
**影响范围**: Coordinator 完成后的事件通知、Docker/TUI 接收者

---

## 问题概述

Coordinator 完成 4 阶段（Research → Synthesis → Implementation → Verification）后，尝试通过 IPC 将 `ProjectCompleted` 事件推送给订阅者，但存在以下问题：

1. **IPC 没有活跃接收者** — 事件被直接丢弃
2. **Discord Channel ID 无效** — 尝试发送到 "coordinator" 频道但映射不存在
3. **TaskManager 任务状态更新 race condition** — 任务已完成但更新时找不到

---

## 问题 1: IPC Push 没有活跃接收者

### 现象

```
INFO Dispatcher: ProjectCompleted (project=3f3c2924-2919-41a9-8d65-e80c4fef4d09) → Discord/IPC
WARN IPC push has no active receivers, dropping ProjectCompleted for project 3f3c2924-2919-41a9-8d65-e80c4fef4d09
```

### 根因分析

Dispatcher 在项目完成时调用 `push_project_completed()`，但此时没有任何活跃的 IPC 接收者订阅该项目的事件。

可能原因：
1. TUI 客户端从未连接 `/tmp/nova.sock`
2. 接收者订阅了但已在项目完成前断开
3. 订阅机制本身有问题

### 修复方案

**方案 A: 确保 IPC 连接在项目完成前建立**

在 `nova-ipc/src/server.rs` 中检查 IPC 订阅机制：

```rust
// nova-ipc/src/server.rs
// 确保 ProjectCompleted 事件不会因无接收者而静默丢弃
// 应该在 push_project_completed 前检查是否有接收者
// 如果没有，尝试通过其他渠道（如 Discord）通知
```

**方案 B: 添加 fallback 机制**

当 IPC push 失败（无接收者）时，自动切换到 Discord 推送：

```rust
// nova-daemon/src/dispatcher.rs
async fn push_project_completed(&self, project: ProjectCompleted) {
    // 先尝试 IPC
    if let Err(e) = self.ipc_push(project.clone()).await {
        // IPC 失败，检查是否有 Discord 配置
        if let Some(channel_id) = self.discord_fallback_channel() {
            self.push_to_discord(project, channel_id).await;
        }
    }
}
```

**方案 C: 记录详细日志**

添加更详细的日志，区分"真的没有接收者"和"发送失败"：

```rust
debug!("IPC receivers: {:?}", self.active_receivers());
if self.active_receivers().is_empty() {
    warn!("No active IPC receivers for ProjectCompleted, checking Discord fallback");
}
```

---

## 问题 2: Discord Channel ID 无效

### 现象

```
WARN [V4] Invalid Discord channel ID: coordinator (tried 'coordinator' mapping) discord不会推送
```

### 根因分析

项目完成后的通知尝试发送到名为 "coordinator" 的 Discord 频道，但：
1. 配置中没有 `coordinator` 这个频道的映射
2. Discord channel ID 是一个数字 ID，不接受字符串名称

### 修复方案

**方案 A: 添加 coordinator 频道映射**

在 `nova-daemon/src/discord.rs` 或配置文件中添加：

```toml
# ~/.nova/config
[discord.channels]
coordinator = "1494576747872649236"  # 或实际的 Discord channel ID
```

**方案 B: 改进 channel ID 解析逻辑**

当前的 `get_discord_channel_id` 函数在找不到映射时直接返回错误，应该提供更好的 fallback：

```rust
// nova-daemon/src/discord.rs
fn resolve_channel_id(channel: &str) -> Option<ChannelId> {
    // 1. 先查配置文件映射
    if let Some(id) = config::get_discord_channel(channel) {
        return Some(id);
    }
    // 2. 如果是数字字符串，尝试直接解析
    if let Ok(id) = channel.parse::<u64>() {
        return Some(ChannelId(id));
    }
    // 3. 尝试从已知的频道列表中查找
    warn!("Unknown Discord channel: {}", channel);
    None
}
```

**方案 C: 使用默认频道而不是 coordinator**

当特定频道不可用时，使用配置中指定的默认 Discord 频道：

```rust
let target_channel = channel.unwrap_or_else(|| config::default_discord_channel());
```

---

## 问题 3: TaskManager Race Condition

### 现象

```
DEBUG TaskManager: task [coordinator-research] not found for update
DEBUG TaskManager: task [coordinator-implementation] not found for update
```

### 根因分析

在 coordinator 流程中：
1. `coordinator-research` 完成 → 进入 `coordinator-synthesis`
2. `coordinator-synthesis` 尝试更新 `coordinator-research` 状态，但任务可能已被清理

这表明任务状态更新和任务清理之间存在时序问题。

### 修复方案

**方案 A: 延迟任务清理**

任务完成后不立即删除，而是标记为 `Completed` 状态，保留一段时间供后续阶段更新：

```rust
// nova-daemon/src/task_manager.rs
pub enum TaskState {
    Running,
    Completed,  // 保留而非立即删除
    Failed,
}

// 清理 Completed 任务的时间改为延迟 5 分钟
```

**方案 B: 使用 Result 处理任务不存在的情况**

将 `update_task` 的行为从 panic/informational 改为 Result：

```rust
pub fn update_task(&self, id: &str, state: TaskState) -> Result<(), TaskNotFoundError> {
    let mut tasks = self.tasks.write();
    if let Some(task) = tasks.get_mut(id) {
        task.state = state;
        Ok(())
    } else {
        Err(TaskNotFoundError(id.to_string()))
    }
}
```

**方案 C: 在日志中降级为 debug 级别**

这个问题不影响核心功能，只是日志噪音。将 `not found` 从 `debug` 改为 `trace`，并添加上下文说明这是预期行为：

```rust
// 如果任务已完成被清理，忽略更新请求
trace!("Task {} already removed, skipping update", id);
```

---

## 问题 4: Chrome CDP WebSocket 消息解析（次要）

### 现象

```
WARN WS Invalid message: data did not match any variant of untagged enum Message
```

### 根因分析

`chrome-devtools` MCP 工具返回的 WebSocket 消息包含新的消息类型，代码中的 `Message` 枚举定义不完整。

### 修复方案

扩展 `Message` 枚举定义，添加所有可能的 CDP 事件类型：

```rust
// 需要检查 chrome-devtools MCP 工具的 Message 定义
// 或在调用处添加 #[serde(flatten)] 来捕获未知变体
```

---

## 修复优先级

| 问题 | 优先级 | 理由 |
|------|--------|------|
| Discord Channel 映射 | P0 | 当前 Discord 完全不工作 |
| IPC 无接收者 fallback | P1 | 导致通知丢失 |
| TaskManager race | P2 | 不影响功能，只是日志噪音 |
| CDP 消息解析 | P3 | 功能正常，只是日志噪音 |

---

## 涉及文件

- `nova-daemon/src/dispatcher.rs` — IPC push 和 Discord fallback 逻辑
- `nova-daemon/src/discord.rs` — Discord channel ID 解析
- `nova-daemon/src/task_manager.rs` — 任务状态管理
- `nova-ipc/src/server.rs` — IPC 服务器和订阅机制
- `~/.nova/config` — Discord 频道配置
