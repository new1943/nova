# **Nova V4 验收标准 (Acceptance Criteria)**

本文档为所有核心功能定义了量化的测试标准和通过条件，用于在开发完成后自验证。

## **1. 性能与延迟指标 (Performance AC)**

* **AC-P1 (Pre-flight Check 延迟):**
  * `pre_flight_check` 作为一个 JSON-Mode 轻量请求，P90 响应时间必须 `< 1000ms`。
  * 降级策略生效验证：主动配置错误或模拟断网导致请求耗时超过 `3000ms` 时，主 Loop 必须丝滑降级并继续走日常响应流程，绝不能阻断终端用户对话。
* **AC-P2 (极速 CRUD):**
  * TaskManager 接收到事件后覆写 `Tasks.md` 的耗时（包含物理 I/O）必须 `< 10ms`。
  * 验证主 Agent 侧完全感受不到 `Tasks.md` 的读写卡顿。

## **2. 架构纪律指标 (Architecture AC)**

* **AC-A1 (动态权限控制精准度):**
  * 向主 Agent 下发复杂指令（如：“请帮我重新规划整个项目的鉴权模块”），`pre_flight_check` 必须识别为 `complexity: High`。
  * 拦截器介入后，打印注入主 Agent 的 `tools` 列表，该列表必须**仅包含** `delegate_complex_project`，**绝对不能出现** `bash`, `file_edit` 等底层工具。
* **AC-A2 (提示词隔离与防注入):**
  * 在工作区塞入恶意的 `AGENTS.md` 或包含破解指令的随机 `.md` 文件。
  * 启动主 Agent 后验证发往模型的最终 Prompt，必须证明该恶意文件**完全没有被读取和加载**。
  * 在白名单文件（如 `USER.md`）中塞入超过 50,000 字符的大文本，Agent 启动不应该抛出 Context Length Exceeded，并且 Prompt 中必须能看到截断标记 `[... 内容已截断 ...]`。

## **3. 后台总线稳定性指标 (State Bus AC)**

* **AC-B1 (阅后即焚清理能力):**
  * 手动在 `Tasks.md` 中构造 5 条包含 `- [x]` 的已完成任务。
  * 手动触发（或等待时间触发） Heartbeat 的 `SystemIdle` 事件。
  * 验证 5 条 `- [x]` 任务记录被物理移除出文件，且未完成的任务 `- [ ]` 保留。
* **AC-B2 (异步 Memory 提炼不抢占):**
  * 主 Loop 密集对话时（10轮/分钟），MemoryKeeper 的 SideQuery 不能干扰主 Loop 频次的速率限制（Rate Limits）。
  * 若产生竞争，应优先保障主 Loop，SideQuery 具备在 HTTP 429 时的退避重试能力。

## **4. 闭环反馈指标 (Feedback Loop AC)**

* **AC-F1 (异步完工主动推送):**
  * 触发复杂任务委派，主 Agent 回复：“交给我后台去弄了”后，主程序处于等待输入状态。
  * 后台 Coordinator 完成 4 阶段流水线，触发 `ProjectCompleted` 事件。
  * 验证 Discord 对应的频道中，能够不依赖用户输入，由机器人主动发来带格式的竣工战报消息。
