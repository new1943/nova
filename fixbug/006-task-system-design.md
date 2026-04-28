# Nova 任务系统改进方案 (V2 - 极简架构版)

> 设计原则：遵循 KISS 原则，拒绝过度设计。采用“轻量内存控制 + Markdown 状态持久化”的双层架构。
> 日期：2026-04-28

---

## 一、架构理念：状态流与控制流分离

在早期的设计思路（V1）中，试图模仿复杂的状态机机制，这在本地 Agent 场景下属于过度设计（Over-engineered）。
V2 版本回到第一性原理，系统核心诉求仅为：**进程控制** + **数据持久化** + **上下文感知**。

我们将任务系统拆分为双层结构：
1. **物理控制层 (Control Layer - 内存)**：复用现有的 `RUNNING_PROJECTS (HashMap<String, AbortHandle>)`，仅负责物理线程的终止与中断控制。
2. **状态持久层 (State Layer - 磁盘)**：使用工作空间根目录下的 `tasks.md` 作为**单一真实数据源 (SSOT)**，负责持久化记录和 Agent 的上下文感知。

---

## 二、核心数据结构 (`tasks.md`)

在工作空间根目录自动生成并维护 `tasks.md` 文件。采用极简 Markdown 列表语法，既方便 Rust 正则解析，又完美支持人类直接阅读和编辑。

### 2.1 文件格式规范

```markdown
# Nova Tasks
> 本文件由 Nova 系统自动维护，您也可以直接修改它。

* `task-1234`: [Running] 正在重构 dispatcher.rs
* `task-5678`: [Completed] 修复 SSL 握手问题
* `task-9999`: [Failed] 编译错误: 找不到 serde 包
* `task-0000`: [Cancelled] 被用户中断
```

### 2.2 支持的状态类型
* `[Pending]`：进入队列，尚未开始执行
* `[Running]`：正在后台运行中
* `[Completed]`：执行成功
* `[Failed]`：执行报错或超时
* `[Cancelled]`：被用户主动终止

---

## 三、核心模块设计 (Rust)

新增轻量级纯文本文件操作模块：`nova-core/src/task/logger.rs`。
由于仅操作文本，无需复杂的数据库引擎。需使用 `tokio::sync::Mutex` 或文件锁确保并发写入安全。

### 3.1 核心 API
```rust
pub struct TaskLogger;

impl TaskLogger {
    /// 新任务直接在文件末尾追加一行
    pub async fn append_task(id: &str, desc: &str) -> Result<()>;
    
    /// 按行读取 tasks.md，正则替换对应 ID 的状态标签，写回文件
    pub async fn update_status(id: &str, new_status: &str, error_msg: Option<&str>) -> Result<()>;
    
    /// 读取任务列表，格式化为 XML 标签供 Prompt 使用
    pub async fn read_context() -> Result<String>;
    
    /// 守护进程重启时，将残留的 [Running] 修正为 [Failed]
    pub async fn sweep_orphans() -> Result<()>;
}
```

---

## 四、生命周期控制流 (Control Flow)

系统通过拦截现有的关键节点，自动触发状态更新：

| 生命周期 | 触发点 | 内存操作 (Control) | 磁盘操作 (State) |
|---------|--------|------------------|-----------------|
| **创建 (Start)** | `delegate_task` / `delegate_complex_project` 被调用 | 存入 `RUNNING_PROJECTS` | `append_task(id, desc)` 状态为 `[Running]` |
| **成功 (Complete)** | 后台 tokio 线程执行结束 (Ok) | 从 `RUNNING_PROJECTS` 移除 | `update_status(id, "Completed")` |
| **失败 (Fail)** | 后台线程 Panic 或返回 Err | 从 `RUNNING_PROJECTS` 移除 | `update_status(id, "Failed", Some(err))` |
| **取消 (Cancel)** | 用户使用 `/cancel` 或 Agent 调用取消工具 | 取出并调用 `AbortHandle.abort()` | `update_status(id, "Cancelled")` |
| **异常中断 (Crash)** | Nova Daemon 崩溃重启 | 内存清空 | 启动时调用 `sweep_orphans()` 将运行中标记为失败 |

---

## 五、上下文闭环 (Context Loop)

这是 Agent “无所不知”的魔法所在。

在 `nova-core/src/agent/loop.rs` 的主对话循环中，在组装 System Prompt 或 User Prompt 时，**自动注入**当前的任务状态：

```rust
// 伪代码示例
let tasks_context = TaskLogger::read_context().await?;
let prompt = format!("{}\n<current_tasks>\n{}\n</current_tasks>", base_prompt, tasks_context);
```

**效果：**
- 用户问：“现在都有哪些任务？”
- Agent 无需调用任何工具，由于上下文中已经有最新的 `<current_tasks>` 数据，直接脱口而出当前的任务清单。
- 人类可以直接打开 `tasks.md` 划掉任务或修改状态，Agent 下次对话瞬间感知。实现了最完美的“人机协同”。
