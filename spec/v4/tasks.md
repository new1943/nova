# **Nova 重构实施任务与排期 (Tasks)**

本项目分为四个递进的阶段，采用“先破后立，由底向上”的重构策略。

## **Phase 1: 减负与清创 (Clean Up & Deprecation)**

**目标：** 剥离陈旧代偿机制，还主 Agent 一个干净的执行环境。

* [x] **Task 1.1:** 删除或禁用 nova-daemon/src/main.rs 及相关文件中的 <nova_os> 注入逻辑。
* [x] **Task 1.2:** 重构 loop.rs 和 compact.rs。将 Compact 动作精简为纯粹的上下文裁剪，移除在 Compact 期间调用 LLM 生成摘要的同步阻塞代码。
* [x] **Task 1.3:** 从主 Query Loop 中移除任何对 Tasks.md 和 MEMORY.md 的同步写操作。

## **Phase 2: 神经中枢搭建 (The State Bus Foundation)**

**目标：** 建立后台全异步事件流。

* [x] **Task 2.1:** 在 nova-core 中定义 ShadowEvent Enum。
* [x] **Task 2.2:** 初始化全局 tokio::mpsc Channel，并在 nova-daemon 启动时挂载 Dispatcher 守护循环。
* [x] **Task 2.3:** 实现 TaskManager (Rust 原生服务)：接收 TaskProgress 事件，支持对 Tasks.md 进行增删改（阅后即焚）。
* [x] **Task 2.4:** 实现 MemoryKeeper：建立内存缓冲池。实现闲时/积压触发逻辑。
* [x] **Task 2.5:** 将原有的 SideQuery 逻辑接入 MemoryKeeper，针对缓冲池数据执行 MEMORY_GUIDANCE 提炼，原子化覆写 MEMORY.md。

## **Phase 3: 感知层重构 (Context Routing & Tracking)**

**目标：** 实现无阻塞的话题流转和记忆召回。

* [x] **Task 3.1:** 统一前置感知管道。在主 Loop 处理用户输入前，调用主模型发起一次轻量级判定请求，同时输出”话题流转 (TopicShift)”与”任务复杂度”状态。
* [x] **Task 3.2:** 实现滑动窗口逻辑（截取最后 10 条 Message），配合极简的 JSON-Mode Prompt 确保前置判定极速返回。
* [x] **Task 3.3:** 若前置判定包含 `TopicShift`，发射 `TopicArchived` 事件至 mpsc 通道。
* [x] **Task 3.4:** 串联 AgenticSessionSearch。当分类器返回 `TopicShift` 时，触发历史扫描并将结果挂载到内存缓存中，供主 Loop 下一轮消费。

## **Phase 4: 宏观编排层落地 (Macro-Orchestration & Coordinator)**

**目标：** 激活 Team 和 Coordinator 策略，实现主 Agent 减负。

* [x] **Task 4.1:** 实现”状态机拦截”机制。根据 Task 3.1 中的前置判定结果，若判定为”复杂多步骤任务”，在 ToolRegistry 层面动态过滤底层工具包（bash, read 等）。
* [x] **Task 4.2:** 编写高级工具 delegate_complex_project 及其专属 Schema，注册至主 Agent。Schema 定义放宽，不仅限于重构/开发，还包括复杂的多步骤网页检索等任何需统筹规划的任务。
* [x] **Task 4.3:** 完善 Rust Coordinator 引擎，串联 4 阶段流水线（Research -> Synthesis -> Implementation -> Verification）。
* [x] **Task 4.4:** 实现 SubAgent (Worker) 在执行工具调用前后，向 mpsc 通道发射 TaskProgress 事件的探针埋点。
* [ ] **Task 4.5:** 端到端集成测试：下发一个复杂重构任务，观察主界面（不卡顿）、Tasks.md（动态增删）与后台子线程的协同运作情况。

## **Phase 5: 中央提示词构建管线 (Central PromptBuilder)**

**目标：** 实现系统硬编码纪律与用户态动态上下文的物理隔离。

* [x] **Task 5.1:** 建立白名单加载机制，仅允许加载 `SOUL.md`, `IDENTITY.md`, `USER.md`, `HEARTBEAT.md`。
* [x] **Task 5.2:** 实现五层 Prompt 组装逻辑，将 `MEMORY_GUIDANCE` 等系统核心纪律提取为 Rust 常量并硬编码至第二层。
* [x] **Task 5.3:** 实现 I/O 截断防护，采用固定字符限制（如 20000 字符，保留头部 70% + 尾部 20% + 中间截断标记）防止 Context 溢出与恶意注入。

## **Phase 6: 闭环反馈与心跳引擎 (Feedback Loop & Heartbeat)**

**目标：** 建立后台任务完工后的主动通知机制与系统主动性脉搏。

* [x] **Task 6.1:** 完善 `ShadowEvent` 枚举，增加 `ProjectCompleted { report: String, channel_id: String }` 事件以携带路由上下文。
* [x] **Task 6.2:** 实现 Discord 渠道的主动消息推送机制。当 Dispatcher 收到 `ProjectCompleted` 事件时，触发守护进程向 Discord 频道主动发送战报。
* [x] **Task 6.3:** 搭建 Heartbeat 定时引擎（tokio::time），实现基于系统静默状态的闲时管家与夜间批处理（Dream）机制。