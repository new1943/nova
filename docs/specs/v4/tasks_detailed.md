# **Nova 重构详细任务执行表 (Detailed Tasks)**

本文档对原本的粗粒度任务进行了方法级的细化，并明确了任务依赖（Dependencies）和执行优先级（Priority: P0-P3）。

## **Phase 1: 减负与清创 (Clean Up & Deprecation)**
**目标：** 剥离陈旧代偿机制，还主 Agent 一个干净的执行环境。
* [ ] **Task 1.1 (P0):** 删除 `nova-daemon/src/main.rs` 及系统 Prompt 组装环节中的 `<nova_os>` 注入逻辑。（依赖：无）
* [ ] **Task 1.2 (P0):** 重构 `compact.rs`。将 `compact` 方法中的 LLM API 调用移除，只保留字符串和 Token 裁剪的纯函数逻辑。（依赖：无）
* [ ] **Task 1.3 (P0):** 搜索 `nova-core/src/session.rs` 和 `loop.rs`，彻底删除主 Query Loop 阻塞等待写入 `Tasks.md` 和 `MEMORY.md` 的同步逻辑。（依赖：无）

## **Phase 2: 神经中枢搭建 (The State Bus Foundation)**
**目标：** 建立后台全异步事件流。
* [ ] **Task 2.1 (P0):** 在 `nova-core/src/models/` 创建 `events.rs`，定义 `ShadowEvent` 和 `TaskAction` 枚举。（依赖：无）
* [ ] **Task 2.2 (P0):** 在 `nova-daemon` 中初始化 `tokio::mpsc::channel(100)`。编写 `dispatcher_loop(mut rx: Receiver<ShadowEvent>)` 并在应用启动时 `tokio::spawn` 挂载。（依赖：2.1）
* [ ] **Task 2.3 (P1):** 创建 `TaskManager` 结构体。编写 `handle_task_progress` 方法，实现对 `Tasks.md` 行级别的 Markdown Regex 匹配与覆写，保证极速原子化 IO。（依赖：2.2）
* [ ] **Task 2.4 (P1):** 创建 `MemoryKeeper` 结构体。维护内部状态 `Vec<Message>`。编写 `handle_archived_topic` 接收数据，以及 `handle_idle` 触发 `SideQuery` 进行异步摘要逻辑。（依赖：2.2）

## **Phase 3: 统一前置感知 (Pre-flight Check & Context Routing)**
**目标：** 整合状态机拦截与话题感知。
* [ ] **Task 3.1 (P0):** 编写 `pre_flight_check(input: &str)` 方法，构造 JSON-Mode 约束请求，使用主模型极速返回 `PreFlightCheckResult`。（依赖：无）
* [ ] **Task 3.2 (P1):** 在主 Loop 收到用户输入的第一时间，调用 `pre_flight_check`。若检测到 `topic_shift == true`，通过 `mpsc_tx` 向后台发射 `ShadowEvent::TopicArchived`。（依赖：2.2, 3.1）
* [ ] **Task 3.3 (P2):** 完善 `AgenticSessionSearch`，当 3.2 判定切换时，扫描历史 `.jsonl` 并通过特定的 Prompt Context 注入给下一轮对话。（依赖：3.2）

## **Phase 4: 宏观编排层落地 (Macro-Orchestration & Coordinator)**
**目标：** 激活 Team 和 Coordinator 策略，实现主 Agent 减负。
* [ ] **Task 4.1 (P0):** 修改 `ToolRegistry` 或主 Agent 的工具下发逻辑：根据 `pre_flight_check` 产出的 `complexity`，若为 `High`，仅注册 `delegate_complex_project` 工具，隐藏其他系统工具。（依赖：3.1）
* [ ] **Task 4.2 (P1):** 编写 `delegate_complex_project` 工具的 JSON Schema 并在 Rust 中定义 Handler。（依赖：无）
* [ ] **Task 4.3 (P1):** 编写 Coordinator 引擎核心。接收委托目标后，生成针对性的 Prompt（Research -> Implementation），使用 `tokio::spawn` 挂起独立 Worker 线程执行。（依赖：4.2）
* [ ] **Task 4.4 (P2):** 在 Worker (SubAgent) 的 `QueryLoop` 工具调用 hook 处（`before_tool` / `after_tool`），发送 `ShadowEvent::TaskProgress` 探针。（依赖：2.1, 4.3）

## **Phase 5: 中央提示词构建管线 (Central PromptBuilder)**
**目标：** 严格物理隔离系统指令与用户上下文。
* [ ] **Task 5.1 (P0):** 编写 `PromptBuilder` 模块，废弃原有的无脑读取逻辑。硬编码系统级 Guidance (`MEMORY_GUIDANCE` 等)。（依赖：1.1）
* [ ] **Task 5.2 (P0):** 编写白名单读取函数，仅从磁盘加载 `SOUL.md`, `IDENTITY.md`, `USER.md`, `HEARTBEAT.md`。
* [ ] **Task 5.3 (P1):** 实现 I/O 截断工具函数：`truncate_safe(text: &str, max_len: usize) -> String`（保留 70% 头部，20% 尾部），应用于用户文件的读取内容中。

## **Phase 6: 闭环反馈与心跳引擎 (Feedback Loop & Heartbeat)**
**目标：** 主动闭环与后台巡检。
* [ ] **Task 6.1 (P1):** 编写 `HeartbeatScheduler`。使用 `tokio::time::interval(Duration::from_secs(60))` 轮询 `SessionManager` 的 `last_active_time`。若闲置超阈值，发射 `SystemIdle` 事件。（依赖：2.2）
* [ ] **Task 6.2 (P2):** 在 Dispatcher 的处理循环中，增加对 `ProjectCompleted` 的路由处理。
* [ ] **Task 6.3 (P2):** 获取 Discord Serenity HTTP Client 的 Arc 引用，在收到 `ProjectCompleted` 后调用 API 推送竣工战报消息。（依赖：6.2）
