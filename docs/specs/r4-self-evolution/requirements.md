# 需求文档：R4 自进化

## 简介

R4「自进化」是 Nova V2 Release 计划的第四阶段（最终阶段），目标是落地 Hermes 中优先级借鉴、强化记忆与工具使用纪律、实现渐进式 Skill 披露、保障文件写入原子性，并补充关键路径测试覆盖。R1 已建立 TurnPipeline 闭环，R2 已完成 8 crate 拆分，R3 已完成 trait 化接口与安全防护。R4 在此基础上完善 Agent 行为规范、优化 Skill 系统的上下文效率、确保数据写入可靠性，并通过测试验证核心路径的正确性。

## 术语表

- **AGENTS.md**：Nova Agent 的内置行为宪法文件，通过 `include_str!` 编译嵌入，定义操作规范
- **MEMORY.md**：用户工作记忆文件（Layer 1），路径为 `~/.nova/MEMORY.md`，存储跨会话持久化的关键信息
- **MemoryKeeper**：`nova-memory` crate 中的记忆提取组件，处理 TopicArchived 事件并将有价值信息写入 MEMORY.md
- **SkillsLoader**：`nova-tools/src/skills/loader.rs` 中的技能加载器，负责从磁盘读取所有 Skill 定义
- **SkillsListTool**：`skills_list()` 工具实现，当前返回所有 Skill 的 name + description 元数据
- **SkillViewTool**：`skill_view(name)` 工具实现，返回指定 Skill 的完整 SKILL.md 内容
- **SharedSkillsLoader**：`Arc<Mutex<SkillsLoader>>` 类型别名，用于跨线程共享 Skill 缓存
- **原子写入**：使用 temp 文件写入 + rename 的模式，确保文件写入操作的原子性，避免写入中断导致数据损坏
- **TurnPipeline**：Agent 每轮对话的处理流水线，包含 Classify→Track→Gate→Inject→PlatformHint→ExecuteConfig 各 Stage
- **TurnContext**：Pipeline 各 Stage 共享的单次对话数据载体
- **Dispatcher**：`nova-daemon` 中的中央事件路由器，接收 ShadowEvent 并分发给对应处理器
- **ShadowEvent**：系统内部事件总线的事件类型，包含 TaskProgress、TopicArchived、SystemIdle、ProjectCompleted
- **TaskManager**：`nova-daemon` 中的任务管理器，处理 TaskProgress 事件并维护 tasks.md 文件
- **SessionManager**：`nova-memory` 中的会话管理器，负责 session JSONL 文件的读写
- **InjectStage**：TurnPipeline 中负责上下文注入的 Stage
- **渐进式披露**：信息分层展示策略，仅在需要时加载完整内容，减少不必要的上下文占用

## 需求

### 需求 1：记忆内容策略明确化

**用户故事：** 作为开发者，我希望 AGENTS.md 中的记忆规范明确定义"存什么"和"不存什么"，以便 Agent 能做出正确的记忆写入决策，减少无效记忆积累。

#### 验收标准

1. THE AGENTS.md 记忆系统章节 SHALL 明确列出应存储的内容类型：用户偏好、环境细节、工具使用技巧、稳定惯例、用户纠正过的行为
2. THE AGENTS.md 记忆系统章节 SHALL 明确列出不应存储的内容类型：任务进度、Session 结果、已完成工作日志、临时 TODO 状态
3. THE AGENTS.md 记忆系统章节 SHALL 包含核心原则声明："最有价值的记忆是不需要用户再次纠正你的记忆"
4. THE AGENTS.md 记忆系统章节 SHALL 指导 Agent 在发现新方法或解决可复用问题时，使用 skill 工具而非 MEMORY.md 保存
5. THE AGENTS.md 记忆系统章节 SHALL 指导 Agent 在需要回忆过去对话内容时，使用 session_search 而非将对话结果存入 MEMORY.md

### 需求 2：工具使用纪律

**用户故事：** 作为开发者，我希望 AGENTS.md 中嵌入工具使用纪律规范，以便 Agent 在承诺执行操作后必须实际调用工具完成，避免"光说不练"的行为。

#### 验收标准

1. THE AGENTS.md SHALL 新增"工具使用纪律"章节，位于安全红线章节之前
2. THE 工具使用纪律章节 SHALL 规定：当 Agent 声明将执行某操作时（如"我来检查文件"、"让我运行测试"），必须在同一回复中发起对应的工具调用
3. THE 工具使用纪律章节 SHALL 规定：Agent 必须持续调用工具直到任务完成且结果经过验证，不得以"下次继续"结束回复
4. THE 工具使用纪律章节 SHALL 列出必须使用工具的场景：数学计算、系统状态查询、文件内容读取、Git 历史查询、当前时间获取
5. THE 工具使用纪律章节 SHALL 规定：每条回复要么包含推进任务的工具调用，要么向用户交付最终结果；仅描述意图而不行动的回复不可接受
6. THE 工具使用纪律章节 SHALL 规定：工具返回空结果或部分结果时，Agent 应尝试不同查询策略重试，而非直接放弃

### 需求 3：渐进式 Skill 披露 — Tier 1 列表

**用户故事：** 作为开发者，我希望 `skills_list()` 工具仅返回 Skill 的名称和描述摘要（不包含完整内容），以便减少不必要的上下文注入量，提高 token 使用效率。

#### 验收标准

1. THE SkillsListTool SHALL 仅返回每个 Skill 的 name、description、category 元数据字段
2. THE SkillsListTool SHALL 不在返回结果中包含 Skill 的完整 prompt 内容
3. WHEN SkillsListTool 被调用时，THE 返回结果 SHALL 包含所有已加载 Skill 的元数据列表，按 name 字母序排列
4. THE SkillsListTool 返回结果 SHALL 包含可用的 category 列表，供 Agent 按类别筛选
5. WHEN 指定 category 参数时，THE SkillsListTool SHALL 仅返回匹配该类别的 Skill 元数据

### 需求 4：渐进式 Skill 披露 — Tier 2 详情

**用户故事：** 作为开发者，我希望 `skill_view(name)` 工具返回指定 Skill 的完整内容，以便 Agent 在确定需要某个 Skill 后才加载其完整定义。

#### 验收标准

1. THE SkillViewTool SHALL 接收 name 参数，返回对应 Skill 的完整 SKILL.md 内容
2. WHEN 指定的 Skill 不存在时，THE SkillViewTool SHALL 返回包含错误信息的结果，标明 Skill 未找到
3. THE SkillViewTool SHALL 支持可选的 file_path 参数，用于查看 Skill 目录下的关联文件（Tier 3 预留）
4. WHEN file_path 参数包含路径遍历字符（如 `..`）时，THE SkillViewTool SHALL 拒绝请求并返回安全错误

### 需求 5：渐进式 Skill 披露 — 注入策略变更

**用户故事：** 作为开发者，我希望系统不再在 system prompt 中一次性注入所有 Skill 完整内容，而是仅注入 Skill 列表摘要，以便节省 token 预算并减少上下文噪音。

#### 验收标准

1. THE InjectStage SHALL 不再将所有 Skill 的完整 prompt 内容注入到 system prompt 中
2. THE InjectStage SHALL 仅注入 Skill 名称和描述的摘要列表，提示 Agent 可通过 `skill_view(name)` 获取详情
3. WHEN auto_trigger 匹配当前用户消息时，THE InjectStage SHALL 自动注入匹配 Skill 的完整内容（保留自动触发机制）
4. THE 注入的 Skill 摘要列表 SHALL 使用 `<available-skills>` 标签包裹，与其他注入内容区分

### 需求 6：原子写入 — MEMORY.md

**用户故事：** 作为开发者，我希望所有对 MEMORY.md 的写入操作使用原子写入模式（temp + rename），以便在写入过程中断电或崩溃时不会损坏已有的记忆数据。

#### 验收标准

1. THE MemoryKeeper 的 `merge_memories` 写入操作 SHALL 使用 temp 文件写入 + rename 模式替代直接 `fs::write`
2. THE MemoryConsolidator 的 MEMORY.md 更新操作 SHALL 使用 temp 文件写入 + rename 模式
3. THE DreamEngine 的 MEMORY.md 更新操作 SHALL 使用 temp 文件写入 + rename 模式
4. WHEN temp 文件写入成功后，THE 系统 SHALL 通过 `fs::rename` 原子替换目标文件
5. IF rename 操作失败，THEN THE 系统 SHALL 清理 temp 文件并返回错误，不损坏原文件

### 需求 7：原子写入 — tasks.md

**用户故事：** 作为开发者，我希望 TaskManager 对 tasks.md 的写入操作使用原子写入模式，以便在并发写入或崩溃时不会损坏任务状态文件。

#### 验收标准

1. THE TaskManager 的 `write_file()` 方法 SHALL 使用 temp 文件写入 + rename 模式替代直接 `std::fs::write`
2. THE TaskLogger 的全量重写操作（truncate + write）SHALL 使用 temp 文件写入 + rename 模式
3. THE TaskLogger 的追加写入操作（append）SHALL 保持当前行为（追加操作本身是安全的）
4. WHEN temp 文件写入成功后，THE 系统 SHALL 通过 rename 原子替换目标文件
5. IF rename 操作失败，THEN THE 系统 SHALL 清理 temp 文件并返回错误

### 需求 8：原子写入 — Session 元数据

**用户故事：** 作为开发者，我希望 SessionManager 对 session meta.json 文件的写入使用原子写入模式，以便会话元数据不会因写入中断而损坏。

#### 验收标准

1. THE SessionManager 的 `save_meta()` 方法 SHALL 使用 temp 文件写入 + rename 模式替代直接 `std::fs::write`
2. WHEN temp 文件写入成功后，THE 系统 SHALL 通过 rename 原子替换目标文件
3. IF rename 操作失败，THEN THE 系统 SHALL 清理 temp 文件并返回错误

### 需求 9：原子写入工具函数

**用户故事：** 作为开发者，我希望有一个统一的原子写入工具函数可供各模块复用，以便避免每个写入点重复实现 temp + rename 逻辑。

#### 验收标准

1. THE nova-core crate SHALL 提供一个公共的 `atomic_write(path: &Path, content: &[u8]) -> Result<()>` 函数
2. THE atomic_write 函数 SHALL 在目标文件同目录下创建以 `.` 开头、`.tmp` 结尾的临时文件
3. THE atomic_write 函数 SHALL 先将内容完整写入临时文件，再通过 `fs::rename` 原子替换目标文件
4. IF 写入临时文件失败，THEN THE atomic_write 函数 SHALL 清理临时文件并返回错误
5. IF rename 失败，THEN THE atomic_write 函数 SHALL 清理临时文件并返回错误
6. THE atomic_write 函数 SHALL 支持异步版本 `atomic_write_async`，使用 `tokio::fs` 实现

### 需求 10：Pipeline 单元测试 — 高复杂度场景

**用户故事：** 作为开发者，我希望有单元测试验证 TurnPipeline 在高复杂度输入下的行为，以便确保 Classify→Gate→ExecuteConfig 路径正确限制工具集并触发委派终止。

#### 验收标准

1. THE 测试 SHALL 构造一个包含高复杂度用户消息的 TurnContext（如"帮我重构整个项目"）
2. WHEN Pipeline 执行完成后，THE TurnContext 的 complexity 字段 SHALL 为 High
3. WHEN Pipeline 执行完成后，THE TurnContext 的 allowed_tools 字段 SHALL 仅包含委派相关工具（delegate_complex_project、cancel_delegated_project）
4. WHEN Pipeline 执行完成后，THE TurnContext 的 should_terminate 字段 SHALL 为 true
5. THE 测试 SHALL 使用 mock LlmBackend 避免真实 API 调用

### 需求 11：Pipeline 单元测试 — 低复杂度 + 话题切换

**用户故事：** 作为开发者，我希望有单元测试验证 TurnPipeline 在低复杂度且话题切换场景下的行为，以便确保 Track Stage 正确检测话题变化且不限制工具集。

#### 验收标准

1. THE 测试 SHALL 构造一个包含低复杂度用户消息和近期对话历史的 TurnContext（如在技术讨论后突然问"你几岁了？"）
2. WHEN Pipeline 执行完成后，THE TurnContext 的 complexity 字段 SHALL 为 Low
3. WHEN Pipeline 执行完成后，THE TurnContext 的 topic_shift 字段 SHALL 为 true
4. WHEN Pipeline 执行完成后，THE TurnContext 的 allowed_tools SHALL 包含多个工具（不被限制为仅委派工具）
5. THE 测试 SHALL 使用 mock LlmBackend 避免真实 API 调用

### 需求 12：Dispatcher 集成测试 — TopicArchived 路由

**用户故事：** 作为开发者，我希望有集成测试验证 Dispatcher 能正确将 TopicArchived 事件路由到 MemoryKeeper，以便确保记忆提取流程的端到端正确性。

#### 验收标准

1. THE 测试 SHALL 构造一个 Dispatcher 实例并配置 mock MemoryKeeper
2. WHEN Dispatcher 收到 TopicArchived 事件时，THE MemoryKeeper 的 `handle_archived_topic` 方法 SHALL 被调用
3. THE 测试 SHALL 验证传递给 MemoryKeeper 的 transcript 内容与发送的事件一致
4. THE 测试 SHALL 验证 Dispatcher 在 MemoryKeeper 未配置时优雅降级（不 panic）

### 需求 13：文档更新 — CLAUDE.md

**用户故事：** 作为开发者，我希望 CLAUDE.md 反映 R1-R4 完成后的最新架构，以便新贡献者能快速理解项目结构和开发规范。

#### 验收标准

1. THE CLAUDE.md SHALL 反映当前 8 crate 的 workspace 结构和各 crate 职责
2. THE CLAUDE.md SHALL 描述 TurnPipeline 的 Stage 组成和执行顺序
3. THE CLAUDE.md SHALL 描述 LlmBackend 和 PlatformAdapter trait 的用途和实现方式
4. THE CLAUDE.md SHALL 包含构建和测试命令（`cargo build --release`、`cargo test`）

### 需求 14：文档更新 — 架构总览

**用户故事：** 作为开发者，我希望 docs/README.md 包含完整的架构总览图和模块说明，以便快速了解系统全貌。

#### 验收标准

1. THE docs/README.md SHALL 包含 8 crate 的依赖关系描述
2. THE docs/README.md SHALL 描述核心数据流：用户消息 → Pipeline → LLM → 工具调用 → 响应
3. THE docs/README.md SHALL 描述事件系统：ShadowEvent → Dispatcher → 各处理器
4. THE docs/README.md SHALL 描述记忆系统的四层架构

### 需求 15：构建验证

**用户故事：** 作为开发者，我希望 R4 的所有改动在 `cargo build --release` 下通过编译，且 TUI 和 Discord 功能不退化。

#### 验收标准

1. WHEN R4 所有改动完成后，THE 项目 SHALL 通过 `cargo build --release` 编译，无错误
2. WHEN R4 所有改动完成后，THE 项目 SHALL 通过 `cargo test` 运行所有测试（含新增测试），无失败
3. THE 新增的 Pipeline 单元测试和 Dispatcher 集成测试 SHALL 在 CI 环境中可重复通过
