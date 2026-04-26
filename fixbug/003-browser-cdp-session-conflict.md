# FIXBUG-003: BrowserTool CDP Session 并发冲突

> 创建日期：2026-04-24
> 优先级：P1（导致 WebSocket 消息解析失败 + 功能性故障）
> 状态：方案已确认，待实施

---

## 现象

当 SubAgent（通过 `delegate_complex_project` 或 `delegate_task` 派发）和主循环 **同时** 使用 BrowserTool 时，`daemon.log` 出现大量 WebSocket 解析错误，浏览器自动化功能完全失效：

```
WS Invalid message: data did not match any variant of untagged enum Message
WS Invalid message: data did not match any variant of untagged enum Message
WS Invalid message: data did not match any variant of untagged enum Message
... (数十条连续错误)
```

## 日志证据

```
2026-04-24T07:52:59.549002Z  INFO Chrome already running, connecting to browser...
2026-04-24T07:53:00.728347Z  WARN WS Invalid message: data did not match any variant of untagged enum Message
2026-04-24T07:53:00.805041Z  WARN WS Invalid message: ...
2026-04-24T07:53:00.816150Z  WARN WS Invalid message: ...
... (在 SubAgent 启动后集中爆发，持续 ~20 秒)
```

关键时间线：
1. `07:52:59` — Chrome 检测到已在运行，尝试连接
2. `07:53:00` — WebSocket 错误集中爆发
3. 错误持续到 `07:53:17`，期间 SubAgent 的 browser 操作全部失败

## 根因分析

### 当前架构

```
主循环 (QueryLoop)
  └─ tools: Arc<ToolRegistry>
       └─ BrowserTool (实例 A) ← 持有 CDP Session

SubAgent (via delegate_complex_project)
  └─ tools: Arc<ToolRegistry>  ← make_subagent_tools() 创建的独立 ToolRegistry
       └─ BrowserTool (实例 B) ← 独立 CDP Session
```

虽然 `make_subagent_tools()` 确实创建了**独立的 `BrowserTool` 实例**（`main.rs:159`），但问题出在 **Chrome 进程层面**：

### 根本原因：共享 Chrome 进程

`BrowserTool` 内部的连接逻辑是：

```rust
// browser.rs（简化）
async fn ensure_session(&self) -> Result<...> {
    // 1. 检查是否已有 Chrome 进程
    if chrome_already_running() {
        // 2. 连接到已有进程的 DevTools WebSocket
        return connect_to_existing();
    }
    // 3. 启动新 Chrome 进程
    launch_new_chrome()
}
```

当**两个 BrowserTool 实例**（主循环的 A 和 SubAgent 的 B）同时启动时：

1. **实例 A**（主循环）先启动 Chrome，获取 CDP WebSocket 连接
2. **实例 B**（SubAgent）检测到 Chrome 已运行，**连接到同一个 Chrome 进程的同一个 WebSocket endpoint**
3. 两个实例通过各自的 WebSocket 连接向**同一个 Chrome target** 发送 CDP 命令
4. Chrome 的 CDP 协议 **不支持多个客户端连接到同一个 target**（debug page）
5. 消息交错 → WebSocket frame 解析失败 → `data did not match any variant of untagged enum Message`

### 问题链路

```
BrowserTool A (主循环) ──┐
                          ├──→ Chrome Process (同一个) ──→ WebSocket target
BrowserTool B (SubAgent) ─┘         ↑
                                    ├── 两个客户端竞争同一 target
                                    └── CDP 消息交错 → 解析失败
```

## 解决方案

### 方案 A：Profile 隔离（推荐）

让 SubAgent 的 BrowserTool 使用**独立的 Chrome profile 目录**，这样会启动一个**全新的 Chrome 进程**，彻底避免 target 共享。

#### 改动

```rust
// nova-daemon/src/main.rs — make_subagent_tools()

fn make_subagent_tools(
    browser_chrome_path: Option<String>,
    browser_profile_dir: Option<String>,  // 主循环的 profile
    browser_headless: bool,
    file_tracker: nova_core::tools::SharedFileReadTracker,
) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    // ... 其他工具注册 ...

    // SubAgent 使用独立的 profile 目录，避免与主循环共享 Chrome 进程
    let subagent_profile = browser_profile_dir
        .as_ref()
        .map(|p| format!("{}-subagent", p))
        .or_else(|| {
            dirs::home_dir().map(|h| {
                h.join(".nova").join("chrome-subagent").to_string_lossy().to_string()
            })
        });

    tools.register_builtin(Box::new(BrowserTool::new(
        browser_chrome_path,
        subagent_profile,       // 独立 profile → 独立 Chrome 进程
        browser_headless,
    )));
    tools
}
```

**优点**：改动最小（~5 行），彻底避免进程级冲突。
**缺点**：每个 SubAgent 启动时可能产生一个新的 Chrome 进程（~200MB 内存开销）。

#### 补充：BrowserTool 清理逻辑

SubAgent 完成后，其 `BrowserTool` 实例被 drop，但 Chrome 进程可能残留。需要确保 `BrowserTool` 的 `Drop` 实现能正确关闭独立 Chrome 进程：

```rust
// browser.rs — 确认 Drop 实现
impl Drop for BrowserTool {
    fn drop(&mut self) {
        // 如果是我启动的 Chrome 进程，kill 掉
        if let Some(ref mut child) = self.chrome_process {
            let _ = child.kill();
        }
    }
}
```

### 方案 B：Mutex 互斥（备选）

在 BrowserTool 外层加 `Arc<Mutex<BrowserTool>>`，确保同一时刻只有一个调用方使用 browser。

**优点**：不启动额外 Chrome 进程。
**缺点**：SubAgent 的 browser 操作会被主循环的 browser 操作阻塞（反之亦然），降低并发性。对于后台任务来说，这个限制可以接受。

```rust
// 在 ToolRegistry 或工具执行层面加 Mutex
// 只对 browser 工具生效
async fn execute(&self, name: &str, input: Value, timeout: Duration) -> Result<String> {
    if name == "browser" {
        let _guard = self.browser_mutex.lock().await;
        self.tools.get(name)?.execute(input).await
    } else {
        self.tools.get(name)?.execute(input).await
    }
}
```

**不推荐**：过于 hack，且阻塞时间不可控。

### 推荐：方案 A（Profile 隔离）

## 修改文件清单

| 文件 | 改动 |
|------|------|
| `nova-daemon/src/main.rs` | `make_subagent_tools()`：为 SubAgent 的 BrowserTool 传入独立 profile 目录 |
| `nova-core/src/tools/browser.rs` | 确认 `Drop` 实现正确关闭子进程；或增加显式 `shutdown()` 方法 |

## 验证方法

1. 重启 daemon
2. 在 TUI 中触发一个需要 browser 的 Medium 任务（通过 delegate_task）
3. **同时**在主循环中也使用 browser（如直接输入"打开百度"）
4. 观察 `daemon.log`：
   - 不应出现 `WS Invalid message` 错误
   - 两个 browser 操作应独立完成
5. SubAgent 完成后，检查 SubAgent 的 Chrome 进程是否被正确清理（`ps aux | grep chrome`）

## 风险点

1. **Chrome 进程残留**：如果 SubAgent panic 或被 abort，`Drop` 可能不会执行，Chrome 进程泄漏。
   - **缓解**：daemon 启动时清理 `chrome-subagent` profile 目录下的锁文件；或定期检测孤儿进程。

2. **磁盘占用**：独立 profile 目录会占用额外磁盘空间（~50MB）。
   - **缓解**：SubAgent 完成后删除 profile 目录（在 `Drop` 或 task completion handler 中）。

3. **并发 SubAgent**：如果同时有多个 SubAgent 使用 browser（通过多个 delegate_task），它们仍可能冲突（如果共享同一个 subagent profile）。
   - **缓解**：每个 SubAgent 使用 `format!("{}-{}", base_profile, uuid)` 作为独立 profile。但这需要在 SubagentSpawner 层面改造 ToolRegistry 的构建方式，复杂度较高。当前阶段先假设同一时刻只有一个后台 browser 任务。
