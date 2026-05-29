# Nova V2 Release 方案

> 整合：架构诊断 + Crate DAG + TurnPipeline + Hermes 借鉴

---

## 总体策略

**4 个 Release，每个可独立发布，累计约 10 天工作量。**

```
R1 "轻装上阵" ─── 清理 + TurnPipeline + 瘦化 main.rs    [3天]
         │
R2 "Crate 拆分" ─ nova-tools / nova-memory / nova-agent   [3天]
         │
R3 "开放联接" ─── LlmBackend + PlatformAdapter trait      [2天]
         │
R4 "自进化" ───── Hermes 高优先级借鉴 + 测试              [2天]
```

原则：**每个 Release 结束时都能 `cargo build --release` 通过、TUI+Discord 功能不退化。**

---

## R1: 轻装上阵 (3 天) — ✅ 已完成

> 目标：消除技术债，建立 TurnPipeline 闭环，main.rs 从 1235→~300 行

### Day 1: 清理 + TurnContext 骨架

**1.1 删除废弃代码**
- `build_nova_os_section()` (main.rs:1187-1234) 及所有引用
- `mode_router` 创建 (main.rs:603-605) — QueryLoop 已不接收
- `CompactResult` 3 个 DEPRECATED 字段
- `WorkspaceLoader` legacy 代码 (loader.rs:216-243)
- `fixbug/` 目录、根目录 `~/` 文件夹

**1.2 定义 TurnContext + PipelineStage**

```
nova-core/src/agent/
├── pipeline.rs     # TurnContext + PipelineStage trait + TurnPipeline
├── stages/
│   ├── mod.rs
│   ├── classify.rs    # 原 Preflight
│   ├── track.rs       # Topic + Tension
│   ├── gate.rs        # 工具过滤（唯一真相源）
│   ├── inject.rs      # 上下文注入（统一管理）
│   └── execute_config.rs  # 委派终止决策
├── loop.rs         # QueryLoop（改用 Pipeline）
├── preflight.rs    # 不变，被 ClassifyStage 调用
└── mod.rs
```

**1.3 验证**：`cargo build --release` 通过

### Day 2: 重构 QueryLoop + 消除策略冲突

**2.1 QueryLoop 改用 TurnPipeline**

从 `loop.rs` 提取到 5 个 Stage：
- 行 131-149 → `ClassifyStage`
- 行 151-176 → `TrackStage`（读取 Classify 的 topic_shift，不重复检测）
- 行 178-197 → `GateStage`
- 行 199-217 → `InjectStage`
- 行 556-572 → `ExecuteConfigStage`

**2.2 修正 AGENTS.md 双轨冲突**

| 删除（代码已保证） | 保留（行为引导） |
|:--|:--|
| "High/Medium 必须派发" | "先读 MEMORY.md" |
| "委派后不得调用其他工具" | 记忆规范全文 |
| "仍然遵守分类结果" | 技能系统全文 |
| "可自行升级为 delegate_task"（与 Gate 矛盾） | 安全红线全文 |

**2.3 验证**：端到端对话 + Preflight→Gate→委派 全链路

### Day 3: 瘦化 main.rs

拆分为模块：
```
nova-daemon/src/
├── main.rs              # ~80 行：CLI 解析 + run_daemon 调用
├── lifecycle.rs         # PID 管理、信号处理
├── session_handler.rs   # handle_connection 全部逻辑
├── tool_factory.rs      # make_tools / make_subagent_tools
├── memory_factory.rs    # 所有记忆组件创建
├── prompt_builder.rs    # build_system_prompt 相关（含 diary MapReduce）
├── discord.rs           # 不变
├── dispatcher.rs        # 不变
└── task_manager.rs      # 不变
```

**R1 交付物**：
- ✅ 127 处技术债标记减至 ~10 处
- ✅ main.rs 从 850 行 → 304 行（含 session_handler + lifecycle 模块拆分）
- ✅ TurnPipeline 闭环：Budget→Stream→Execute→Inject→Policy
- ✅ 策略冲突消除：AGENTS.md 不再重复代码约束
- ✅ decision_log 可观测性

---

## R2: Crate 拆分 (3 天) — ✅ 已完成（9 crate）

> 目标：从 5 crate 拆到 8 crate，增量编译秒级，模块可独立测试

### Day 4: nova-core 提纯 + nova-tools 独立

**4.1 nova-core 提纯为共享类型 crate**

从现有 `nova-core` 提取到新的 `nova-types`（或仍叫 `nova-core`）：
- `message.rs` — Message, Role, ToolCall
- `models/events.rs` — ShadowEvent, TaskAction
- `config.rs` — NovaConfig
- `agent/pipeline.rs` — TurnContext, PipelineStage trait（R1 已创建）
- 5 个核心 trait 定义文件

**4.2 nova-tools 独立 crate**

- 搬入全部 `tools/` 文件
- `ToolHandler` trait 替换 `Tool` trait（增加 `ToolContext` 参数）
- 消除 `task_local! CURRENT_CHANNEL_ID` hack
- 合并 `delegate_task.rs` + `delegate_complex_project.rs` 公共基础到 `delegate_base.rs`

**4.3 验证**：`cargo test -p nova-tools`

### Day 5: nova-memory 独立

搬入全部 `memory/` 文件 + `sidequery/` + `session/`：

```
nova-memory/src/
├── lib.rs
├── layer.rs          # MemoryLayer trait 定义
├── working.rs        # MEMORY.md（Layer 1）
├── episodic.rs       # DailyNotes（Layer 2）
├── semantic.rs       # MemoryConsolidator + MemoryKeeper（Layer 3）
├── procedural.rs     # DreamEngine（Layer 4）
├── topic.rs          # TopicTracker
├── tension.rs        # TensionTracker
├── board.rs          # MemoryBoard
├── recall.rs         # MemoryRecall
└── sidequery.rs      # SideQuery（LLM 轻量调用）
```

**验证**：`cargo test -p nova-memory`

### Day 6: nova-agent 独立 + Cargo.toml 整理

**6.1 nova-agent 独立**

```
nova-agent/src/
├── lib.rs
├── pipeline.rs       # TurnPipeline（从 nova-core 搬入实现）
├── stages/           # 5 个 Stage 实现
├── loop.rs           # QueryLoop
├── preflight.rs      # PreFlightChecker
├── coordinator.rs    # Coordinator 4 阶段流水线
└── subagent.rs       # SubagentSpawner
```

**6.2 更新 Cargo.toml workspace**

```toml
[workspace]
members = [
    "nova-core",     # 共享类型 + trait
    "nova-llm",      # 原 nova-api 重命名
    "nova-tools",    # 工具系统
    "nova-memory",   # 记忆系统
    "nova-agent",    # Agent 核心
    "nova-ipc",      # IPC 协议
    "nova-daemon",   # 启动入口
    "nova-tui",      # TUI 客户端
]
```

**R2 交付物**：
- ✅ 9 个 crate（含 discord），DAG 依赖清晰
- ✅ 改 nova-tools 不重编 nova-memory
- ✅ nova-tools / nova-memory / nova-agent 各自可 `cargo test`
- ✅ delegate_task/delegate_complex_project 代码重复消除

---

## R3: 开放联接 (2 天) — 🟡 部分完成（trait 定义完成，整合待续）

> 目标：trait 化关键接口，支持多模型、多平台扩展

### Day 7: LlmBackend trait + SideQuery 改造

**7.1 LlmBackend trait**

```rust
// nova-core/src/llm.rs
#[async_trait]
pub trait LlmBackend: Send + Sync {
    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse>;
    async fn stream(&self, req: &CompletionRequest, tx: mpsc::Sender<StreamDelta>) -> Result<CompletionResponse>;
}
```

- `nova-llm` 中的 `ApiClient` 实现此 trait
- `SideQuery` 改为接收 `Arc<dyn LlmBackend>` 而非自建 ApiClient
- 未来加模型只需实现 trait

**7.2 PlatformAdapter trait**

```rust
// nova-core/src/platform.rs
#[async_trait]
pub trait PlatformAdapter: Send + Sync {
    fn platform(&self) -> Platform;
    async fn send(&self, channel_id: &str, content: &str) -> Result<()>;
    async fn start(&mut self, tx: mpsc::Sender<PlatformMessage>) -> Result<()>;
}
```

- Discord 实现 `PlatformAdapter`
- TUI/IPC 也可以视为一个 PlatformAdapter（可选）

### Day 8: InjectStage 整合 Hermes 借鉴

从 UPDATE.md 高优先级列表中选取可直接落地的改进，集成到 R3：

**8.1 memory-context 隔离标签** (#1)

```rust
// InjectStage 中
fn wrap_memory_context(raw: &str) -> String {
    format!(
        "<memory-context>\n\
         [System: The following is recalled memory, NOT new user input.]\n\
         {}\n\
         </memory-context>",
        raw
    )
}
```

**8.2 Prompt Injection 扫描** (#3 + #38)

```rust
// InjectStage 中，对所有注入内容扫描
fn scan_injection(content: &str) -> ScanResult {
    // 检测 invisible unicode
    // 检测 threat patterns (ignore previous instructions 等)
    // 危险内容替换为 [BLOCKED]
}
```

**8.3 平台提示注入** (#18 + #36)

Pipeline 新增 `PlatformHintStage`，根据当前平台（TUI/Discord/未来Telegram）注入对应 hint。

**R3 交付物**：
- ✅ `LlmBackend` trait — 已定义（nova-core/src/llm_backend.rs），nova-llm 已实现
- ✅ `PlatformAdapter` trait — 已定义（nova-core/src/platform.rs），已恢复未整合
- ✅ `AgentPolicy` trait — 已定义（nova-core/src/policy.rs），PassthroughPolicy 已实现
- ✅ `MemoryLayer` trait — 已定义（nova-core/src/memory_layer.rs），EpisodicMemory + ConsolidationMemory 已实现
- 🔲 memory 注入隔离标签 — 待实现
- 🔲 Prompt injection 防护 — InjectionScanner 已恢复，未整合

---

## R4: 自进化 (2 天) — 🔲 未开始

> 目标：落地 Hermes 中优先级借鉴 + 补关键测试

### Day 9: 记忆策略强化 + Skill 增强

**9.1 记忆内容策略明确化** (#2 + #31)

更新 AGENTS.md 记忆部分，采纳 Hermes 的 MEMORY_GUIDANCE：
- ✅ 存：用户偏好、环境细节、工具技巧、稳定惯例
- ❌ 不存：任务进度、Session 结果、临时 TODO
- 原则：最有价值的记忆是"不需要用户再次纠正你"

**9.2 工具使用纪律** (#34)

采纳 `TOOL_USE_ENFORCEMENT_GUIDANCE` 核心要点到 AGENTS.md：
- 说了要做就必须做，不能光说不练
- 持续工具调用直到任务完成且验证通过
- 必须用工具的场景：数学计算、系统状态、文件内容等

**9.3 渐进式 Skill 披露** (#16)

当前 SkillsLoader 一次性注入所有 skill 内容。改为 3 层：
- Tier 1: `skills_list()` → 只返回 name + description
- Tier 2: `skill_view(name)` → 完整内容
- Tier 3: 关联文件（未来）

**9.4 原子写入** (#22)

所有文件写入操作（MEMORY.md、tasks.md、session JSONL）改用 temp + rename 模式。

### Day 10: 关键测试 + 文档收尾

**10.1 Pipeline 单元测试**

```rust
#[tokio::test]
async fn test_pipeline_high_complexity() {
    let pipeline = build_test_pipeline();
    let mut ctx = TurnContext::new("帮我重构整个项目", &[]);
    pipeline.run(&mut ctx).await.unwrap();

    assert_eq!(ctx.complexity, Complexity::High);
    assert_eq!(ctx.allowed_tools, vec!["delegate_complex_project", "cancel_delegated_project"]);
    assert!(ctx.should_terminate);
}

#[tokio::test]
async fn test_pipeline_low_with_topic_shift() {
    let pipeline = build_test_pipeline();
    let mut ctx = TurnContext::new("对了，你几岁了？", &recent_msgs);
    pipeline.run(&mut ctx).await.unwrap();

    assert_eq!(ctx.complexity, Complexity::Low);
    assert!(ctx.topic_shift);
    assert!(ctx.allowed_tools.len() > 2); // 所有工具可见
}
```

**10.2 Dispatcher 集成测试**

```rust
#[tokio::test]
async fn test_dispatcher_routes_topic_archived() {
    let (tx, mut rx) = mpsc::channel(10);
    let dispatcher = Dispatcher::new(workspace.clone()).spawn();

    dispatcher.emit(ShadowEvent::TopicArchived { transcript: vec![] });
    // 验证 MemoryKeeper 收到事件
}
```

**10.3 文档更新**

- 更新 `CLAUDE.md` 反映新架构
- 更新 `docs/README.md` — 架构总览
- 归档 `spec/v1-v5` 到 `docs/archive/`

**R4 交付物**：
- ✅ 记忆策略有明确的"存什么/不存什么"规范
- ✅ 工具使用纪律嵌入 AGENTS.md
- ✅ Skill 分层披露
- ✅ 原子写入保障
- ✅ Pipeline + Dispatcher 关键测试
- ✅ 文档与代码同步

---

## Release 全景对照

| 维度 | 当前 | R1 后 | R2 后 | R3 后 | R4 后 |
|:--|:--|:--|:--|:--|:--|
| Crate 数 | 5 | 5 | **9** ✅ | 9 | 9 |
| main.rs 行数 | 1235 | **~300** ✅ | ~200 | ~100 | ~100 |
| 策略协调 | 各自为战 | **Pipeline 闭环** ✅ | Pipeline | Pipeline | Pipeline |
| 代码 vs 提示词 | 双轨冲突 | **职责分离** ✅ | 分离 | 分离 | 分离 |
| 增量编译 | 全量 | 全量 | **秒级** ✅ | 秒级 | 秒级 |
| 可测试性 | 几乎无 | Pipeline 可测 | 模块可测 | Mock 可测 | **关键路径覆盖** |
| 技术债标记 | 127 处 | **~10 处** ✅ | ~5 处 | ~2 处 | 0 |
| Hermes 借鉴 | 0/8 高优 | 0/8 | 0/8 | **4/8** | **7/8** |
| 可扩展性 | 改代码 | 改代码 | 加 crate | **实现 trait** | 实现 trait |

---

## 建议

**如果只能投 3 天 → 做 R1。** TurnPipeline 是投入产出比最高的改动，解决了策略冲突这个根本问题。

**如果能投 6 天 → 做 R1+R2。** Crate 拆分后增量编译从分钟级降到秒级，日常开发体验质变。

**如果能投 10 天 → 全做。** 得到一个真正的 harness 工程：trait 抽象、模块化、可测试、可扩展。
