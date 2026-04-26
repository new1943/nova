# **Nova 宏观编排架构设计文档 (Design)**

## **1\. 架构概览 (Architecture Overview)**

重构后的 Nova 系统采用 **五层宏观编排架构 (Macro-Orchestration)**，实现前台接待、后台执行、系统纪律与主动感知的物理隔离。

### **1.1 分层拓扑**

1. **唤醒与感知层 (The Pulse):** 依托 Heartbeat 引擎，提供系统的心跳脉搏与主动性（如夜间批处理、定时任务）。  
2. **交互与路由层 (The Front Desk):** 主 Agent / Query Loop。极简 Context，依托中央 PromptBuilder 维持纪律，负责意图理解与宏观委派。  
3. **宏观编排层 (The Orchestrator):** Rust 原生 Coordinator。接管后台任务，驱动 Research \-\> Synthesis \-\> Implementation \-\> Verification 四阶段流水线。  
4. **异步执行层 (The Workers):** tokio::spawn 隔离的 SubAgents。拥有独立预算，疯狂调用底层基础工具干活。  
5. **状态总线层 (The State Bus):** mpsc 驱动的后台守护进程（TaskManager & MemoryKeeper），管理物理看板 (Tasks.md, MEMORY.md)。

## **2\. 核心机制设计 (Key Mechanisms)**

### **2.1 状态机拦截 (State Machine Interception)**

* **机制：** 统一前置嗅探（Pre-flight Check）。在构建 LLM 正式回复请求前，拦截器发起一次独立的 API 请求（使用主模型），一次性评估**用户意图复杂度**与**话题流转状态 (TopicShift)**。
* **动作：** 若判定为宏观架构/多步骤任务，从 ToolRegistry 中动态剔除 bash, read_file, file_edit。仅注入 delegate_complex_project 工具。强制 LLM 走委派流程，防止“微操狂热”。

### **2.2 统一影子中枢 (Unified Shadow Engine)**

抛弃分散的状态同步，采用单一 mpsc 通道接收所有后台事件 ShadowEvent：

enum ShadowEvent {  
    TaskProgress { task_id: String, status: TaskStatus },  
    TopicArchived { transcript: Vec<Message> },  
    SystemIdle { duration_mins: u32, transcript: Vec<Message> },
    ProjectCompleted { report: String, channel_id: String },
}

* **Dispatcher 路由：**  
  * TaskProgress -> 路由至 TaskManager -> 毫秒级修改 Tasks.md (物理擦除以销账)。  
  * TopicArchived / SystemIdle -> 路由至 MemoryKeeper -> 缓冲池等待 -> 闲时唤醒 SideQuery 提炼 MEMORY.md。
  * ProjectCompleted -> 触发闭环反馈机制 -> 通过 Discord 渠道主动向用户推送竣工战报消息。

### **2.3 动态上下文路由 (Dynamic Context Router)**

解决 \<nova\_os\> 废除后的状态感知问题：

1. **统一前置嗅探 (Pre-flight Check):** 直接复用拦截器（2.1）的独立 API 请求结果。该请求不仅输出任务复杂度，还同时输出 {"status": "TopicShift"}。
2. **远场召回 (Agentic Search):** 检测到 TopicShift 时，主流程不阻塞。后台静默调用 AgenticSessionSearch 扫描 .jsonl 历史。若匹配到前置悬停话题，自动将背景 Context 注入下一轮主 Agent 对话中。

### **2.4 中央提示词构建管线 (Central PromptBuilder)**

为了彻底解决提示词碎片化和系统纪律被外部文件污染的问题，引入严格的 **五层组装管线**，实现“系统硬编码”与“用户动态上下文”的物理隔离。

* **严格的白名单加载策略：** 废除原有无脑加载 Workspace 目录下所有 Markdown 的逻辑。**仅允许加载 SOUL.md (灵魂人设), IDENTITY.md (能力边界), USER.md (用户偏好), HEARTBEAT.md (心跳配置)**。彻底阻断 AGENTS.md 等非标文件对模型底层逻辑的污染。  
* **组装层级：**  
  1. **第一层 (Base Identity):** 基础设定与身份 (加载 SOUL.md, IDENTITY.md)。  
  2. **第二层 (System Enforcements):** 核心执行纪律。此部分**必须硬编码于 Rust 内核**（包含防注入指令、记忆法则、代码编写规范），绝不暴露给用户态。  
  3. **第三层 (Tools & Capabilities):** 工具声明。由 ToolRegistry 根据当前状态机拦截结果，动态生成并注入可用工具列表及强制使用约束。  
  4. **第四层 (User Context & Shield):** 用户级上下文。加载 USER.md 和 HEARTBEAT.md。在此层必须实施 **I/O 截断防护**（例如超长文件只保留头尾），防止恶意注入或 Token 溢出冲刷掉第二层的纪律。  
  5. **第五层 (Dynamic Status):** 由 Phase 2.3 动态召回的前置历史上下文拼接。

### **2.5 主动心跳机制 (Active Heartbeat)**

作为整个系统的“脉搏”与主动性来源，Heartbeat 引擎是一个运行在后台的独立 tokio 定时任务。

* **闲时静默管家：** 监测系统状态，若用户超过 N 小时未交互（System Idle），Heartbeat 将触发系统的后台自理逻辑（例如唤醒 MemoryKeeper 处理挤压的对话缓冲池）。  
* **夜间批处理 (Nightly Dream)：** 在设定的系统静默期（如凌晨 2:00 \- 5:00），Heartbeat 负责唤醒 DreamEngine。不直接读取海量原始日志，而是整合 MEMORY.md 中的零碎经验，进行全局去重、结构化（例如将重复的排错经验固化为系统 Skill）。  
* **主动日程探针：** 依据白名单加载的 HEARTBEAT.md 中的用户自定义规则（如“每天下午 6 点检查鱼缸系统代码状态”），自动组装 prompt 并触发主动提醒或后台巡检任务。

## **3\. 弃用说明 (Deprecations)**

* **彻底废除 \<nova\_os\> 管道：** 不再向主 Agent 注入张力/模式等伪代码标签。张力阈值改为在 Rust 侧触发拦截策略。  
* **解绑 Compact 职责：** Compact 仅负责基于 Token 预算的暴力/优雅截断，不再调用 LLM 生成结构化摘要。总结工作全权移交 MemoryKeeper。  
* **废弃散装策略文件：** 废除原有 Workspace 下的 AGENTS.md, TOOLS.md 等外部控制文件，相关逻辑全量下沉至 Rust 内核硬编码与动态生成。