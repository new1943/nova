# **Nova V4 接口契约规范 (Interface Contract)**

本文档定义了 V4 架构中各个模块之间的调用边界、消息协议与错误处理策略。

## **1. 模块边界与通信协议**

### **1.1 主 Loop <-> PreFlight Check (拦截器)**
* **协议：** 异步函数调用（直接 `await`）。
* **接口：** `async fn pre_flight_check(input: &str, history: &[Message]) -> Result<PreFlightCheckResult>`
* **背压与降级：**
  * 若前置请求超时（> 3秒）或模型崩溃，直接**降级为默认安全状态**：`topic_shift: false, complexity: Low`。保证主流程不被阻塞。

### **1.2 主 Loop / Heartbeat -> Dispatcher (mpsc 总线)**
* **协议：** 无界或大容量有界 `tokio::mpsc` Channel。
* **接口：** `let tx = mpsc::Sender<ShadowEvent>::clone(); tx.send(event).await;`
* **背压与错误处理：**
  * 主 Loop 使用 `try_send` 或极短超时的 `send`，**绝不允许阻塞主 Loop**。
  * 如果 Channel 满载，丢弃 `TaskProgress`（仅影响状态展示），保留 `ProjectCompleted`（放入死信队列重试）。

### **1.3 Dispatcher -> TaskManager / MemoryKeeper**
* **协议：** Dispatcher 循环接收事件，通过 Rust 函数调用对应管理器的处理逻辑。
* **接口边界：**
  * `TaskManager` 是一个封装了对 `Tasks.md` 互斥文件锁写的单例模块。
  * `MemoryKeeper` 是一个封装了缓冲池 (`Vec<Message>`) 和异步 SideQuery 调用的单例模块。
* **错误处理：**
  * `TaskManager` 写入失败：静默重试 3 次，若失败则打 error log 放弃。不影响主流程。
  * `SideQuery` (MemoryKeeper 提炼) 失败：数据退回缓冲池，等待下一次 Idle 触发。

### **1.4 Coordinator <-> SubAgent (Worker)**
* **协议：** `tokio::spawn` 衍生的后台异步任务。
* **交互：**
  * Coordinator (状态机) 根据任务目标，组装 SubAgent 的 Prompt（包含全量 bash, edit 工具）。
  * Coordinator 使用独立的 `QueryLoop` 实例唤醒 SubAgent。
  * SubAgent 执行完成后，返回 `Result<String>` 作为 Research/Synthesis 的交付物。
* **通知：** SubAgent 内部可通过克隆的 `mpsc::Sender` 直接向总线发射 `TaskProgress`。

## **2. 控制流时序简图 (TopicShift 触发链)**

```text
[User Input] 
    │
    ▼
(1) Pre-flight Check (LLM API) -> 返回 {topic_shift: true, complexity: Low}
    │
    ├─► (2a) 若 topic_shift == true -> 触发 AgenticSearch 扫描历史
    │        -> 扫描结果暂存于当前 SessionContext
    │
    ├─► (2b) 若 topic_shift == true -> mpsc_tx.send(TopicArchived { transcript })
    │        -> MemoryKeeper 收到后将其放入归档缓冲池
    │
    ▼
(3) 主 Loop 组装 Prompt (此时 Context 包含 2a 召回的历史)
    │
    ▼
(4) 调用主 LLM 返回结果，呈现给用户
```
