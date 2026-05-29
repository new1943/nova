# NOVA 代码-文档对齐补全计划

**目标**: 让代码实现与设计文档（`nova_v2_architecture.md`、`nova_strategy_coordination.md`、`nova_v2_release_plan.md`）对齐。
**前提**: 已确认全量对齐 — TurnPipeline + main.rs 拆分 + 两个 trait 都实现。

---

## 当前状态

| 指标 | 设计目标 | 实际 |
|:-----|:---------|:-----|
| main.rs 行数 | ~80 行 | 304 行 ✅ |
| agent_loop 架构 | TurnPipeline 5 Stage | TurnPipeline 5 Stage ✅ |
| 核心 trait | 5 个 | 5 个已定义（AgentPolicy, MemoryLayer, LlmBackend, PlatformAdapter, PipelineStage） |
| Crate 数 | 10（含 nova-gateway） | 9（含 discord） |

---

## Phase 1: 核心 Trait 定义（nova-core） — ✅ 已完成

> 先在 nova-core 定义好接口，不改现有代码，全部可编译。

### 1.1 AgentPolicy trait

**文件**: `nova-core/src/policy.rs`（新建）

```rust
#[async_trait]
pub trait AgentPolicy: Send + Sync {
    /// 每轮 API 调用前，决定工具可见性和行为
    async fn decide(&self, ctx: &PolicyContext) -> PolicyDecision;
}

pub struct PolicyContext {
    pub user_input: String,
    pub recent_messages: Vec<Message>,
    pub all_tool_names: Vec<String>,
}

pub struct PolicyDecision {
    pub allowed_tools: Vec<String>,
    pub terminate_after_tool: bool,
    pub prompt_injection: String,
}
```

**现有对应**: agent_loop.rs:401 的 `is_allowed` 检查 + executor_tools 的 `terminate_after_tool` 逻辑。

### 1.2 MemoryLayer trait

**文件**: `nova-core/src/memory_layer.rs`（新建）

```rust
#[async_trait]
pub trait MemoryLayer: Send + Sync {
    fn name(&self) -> &str;

    /// 写入记忆（如 compact 时提取）
    async fn store(&self, content: &str) -> Result<()>;

    /// 召回相关记忆（注入 system prompt）
    async fn recall(&self, query: &str, limit: usize) -> Result<Vec<String>>;

    /// 空闲时整合（可选）
    async fn consolidate(&self) -> Result<()> { Ok(()) }
}
```

**现有对应**: `DailyNotes`、`MemoryConsolidator`、`MemoryBoard`、`DualWriteMemory` 各自独立，无统一接口。

### 1.3 注册到 lib.rs

**修改**: `nova-core/src/lib.rs` 添加 `pub mod policy;` 和 `pub mod memory_layer;`

**验证**: `cargo build -p nova-core`

---

## Phase 2: TurnPipeline 实现（nova-agent） — ✅ 已完成

> 将 agent_loop.rs 的 662 行单体循环拆成 5 个 Stage。

### 2.1 TurnContext 数据结构

**文件**: `nova-agent/src/pipeline/mod.rs`（新建）

```rust
pub struct TurnContext {
    // 输入（不可变）
    pub user_input: String,
    pub session_id: String,

    // Stage 1: Inject 写入
    pub system_prompt: String,
    pub prompt_injections: Vec<PromptInjection>,

    // Stage 2: Policy 写入
    pub allowed_tools: Vec<String>,
    pub should_terminate_after_tool: bool,

    // Stage 3: Budget 写入
    pub needs_compact: bool,
    pub budget_pct: f32,

    // 运行时状态
    pub turn_count: usize,
    pub messages: Vec<Message>,
    pub newly_added: Vec<Message>,
    pub tool_calls: Vec<AccumulatedToolCall>,
    pub text_content: String,
    pub token_usage: TokenUsage,
    pub should_break: bool,
    pub error: Option<String>,

    // 可观测性
    pub decision_log: Vec<DecisionEntry>,
}
```

### 2.2 PipelineStage trait

**文件**: `nova-agent/src/pipeline/stage.rs`（新建）

```rust
#[async_trait]
pub trait PipelineStage: Send + Sync {
    fn name(&self) -> &str;
    async fn execute(&self, ctx: &mut TurnContext, session: &mut Session) -> Result<()>;
}
```

### 2.3 5 个 Stage 实现

| Stage | 文件 | 从 agent_loop.rs 提取 | 行号 |
|:------|:-----|:----------------------|:-----|
| **InjectStage** | `stages/inject.rs` | system_prompt 拼接 + prompt_injections | 新增（整合 auto-search、skill injection） |
| **PolicyStage** | `stages/policy.rs` | tool_schemas 过滤 + terminate 判断 | 138, 401, 设计中的 GateStage |
| **BudgetStage** | `stages/budget.rs` | pre-flight + post-flight budget check | 160-175, 314-348 |
| **StreamStage** | `stages/stream.rs` | API 调用 + 流式收集 | 198-263 |
| **ExecuteStage** | `stages/execute.rs` | 工具执行 + 截断 + approval | 378-466 |

### 2.4 TurnPipeline 组装

**文件**: `nova-agent/src/pipeline/mod.rs`

```rust
pub struct TurnPipeline {
    stages: Vec<Box<dyn PipelineStage>>,
}

impl TurnPipeline {
    pub async fn run(&self, ctx: &mut TurnContext, session: &mut Session) -> Result<()> {
        for stage in &self.stages {
            stage.execute(ctx, session).await?;
            ctx.decision_log.push(DecisionEntry {
                stage: stage.name().into(),
                timestamp: Instant::now(),
            });
        }
        Ok(())
    }
}
```

### 2.5 QueryLoop 重构

**修改**: `nova-agent/src/agent_loop.rs`

- `QueryLoop` 保留，但 `run_turn` 内部改为：
  1. 创建 `TurnContext`
  2. 调用 `pipeline.run(&mut ctx, &mut session)`
  3. 发送 `LoopEvent`（从 ctx 读取）
  4. 触发 hooks
- 辅助函数（`build_completion_messages`、`estimate_message_tokens`、`AccumulatedToolCall`）保留在原文件或提取到 `pipeline/util.rs`
- `write_compact_diary` 和 `run_consolidation` 保留

**验证**: `cargo build -p nova-agent`

---

## Phase 3: main.rs 拆分（nova-daemon） — ✅ 已完成

> 将 850 行的 main.rs 拆成模块。

### 3.1 拆分方案

| 原代码 | 目标文件 | 行数(估) |
|:-------|:---------|:---------|
| `HandleConfig` + `budget_pct_calc` | `main.rs` 保留 | ~30 |
| `main()` + `run_daemon()` 入口 | `main.rs` 保留 | ~120 |
| PID 管理 (write/remove/check) | `lifecycle.rs`（新建） | ~30 |
| `handle_connection()` | `session_handler.rs`（新建） | ~430 |
| `tool_factory::make_tools` 调用 + 注册 | `tool_factory.rs` 已有 | 不变 |
| executor notification setup | `session_handler.rs` 内 | 随 handle_connection 移动 |
| Discord push listener | `main.rs` 保留（与 daemon 启动绑定） | ~50 |

### 3.2 新增文件

**`nova-daemon/src/lifecycle.rs`**:
```rust
pub fn write_pid_file() { ... }
pub fn remove_pid_file() { ... }
pub fn check_pid_file() -> bool { ... }
pub struct PidGuard;
```

**`nova-daemon/src/session_handler.rs`**:
```rust
pub async fn handle_connection(
    mut conn: IpcConnection,
    cfg: HandleConfig,
) -> Result<()> { ... }
```

所有 handle_connection 内部的辅助逻辑（forwarder、heartbeat、inject channel）随函数一起移动。

### 3.3 main.rs 最终结构

```rust
mod agentic_search;
mod discord;
mod discord_adapter;
mod lifecycle;
mod session_handler;  // 新增
mod tool_factory;
mod session_diary;

// ~120 行: main() + run_daemon() + Discord push listener
```

**验证**: `cargo build -p nova-daemon`

---

## Phase 4: trait 整合 — ✅ 已完成

> 把 Phase 1 定义的 trait 接入 Phase 2 的 Pipeline。

### 4.1 AgentPolicy 整合

**创建**: `nova-agent/src/policies/mod.rs`

- `AllToolsPolicy` — 默认策略，所有工具可见（当前行为）
- `FilteredPolicy` — 基于 AgentPolicy trait 的过滤策略

**修改**: `PolicyStage` 调用 `policy.decide()` 替代硬编码的工具过滤

### 4.2 MemoryLayer 整合

**创建**: `nova-agent/src/memory/mod.rs`

实现 MemoryLayer 的具体类型：
- `WorkingMemory` — 包装 MEMORY.md 读写
- `EpisodicMemory` — 包装 DailyNotes
- `ConsolidationMemory` — 包装 MemoryConsolidator

**修改**: `QueryLoop` 持有 `Vec<Arc<dyn MemoryLayer>>`，InjectStage 调用 `recall()`，BudgetStage 调用 `consolidate()`

### 4.3 清理

- 删除 `nova-agent/src/context.rs`（已无用，CallerContext 已删）— **已完成**
- 删除 `nova-core/src/injection_scanner.rs` 测试中的 `#[ignore]`（如果之前有的话）
- 更新 `CLAUDE.md` 标注 RetryPolicy/InjectionScanner/PlatformAdapter 为"已定义，计划整合"

**验证**: `cargo build && cargo test`

---

## Phase 5: 文档同步 — ✅ 已完成

> 更新所有文档与代码一致。

### 5.1 更新 docs/STRATEGIES.md
- 工具清单补全（executor_tools、memory_tool、skills、worktree、paste）
- 策略数量改为 13

### 5.2 更新 docs/CONFIG.md
- DreamEngine 路径修正（consolidate.rs + daily.rs）
- 文件路径更新（nova-agent/src/ 而非 agent/）

### 5.3 更新 docs/nova_v2_release_plan.md
- R1 状态更新：TurnPipeline ✅、main.rs 拆分 ✅
- R2 状态：已完成（9 crate）
- R3 状态：LlmBackend ✅、PlatformAdapter ✅（未整合）
- R4 状态：部分完成

### 5.4 更新 CLAUDE.md
- 移除 "TurnPipeline architecture" 描述（改为 "Pipeline architecture"）
- 更新策略表
- 标注未整合模块

**验证**: 文档 review

---

## 实施顺序

```
Phase 1 (nova-core trait 定义)        ← 无风险，纯新增
    ↓
Phase 2 (TurnPipeline 实现)          ← 核心改动，需仔细验证
    ↓
Phase 3 (main.rs 拆分)               ← 结构调整，逻辑不变
    ↓
Phase 4 (trait 整合)                 ← Phase 2 的 trait 接入
    ↓
Phase 5 (文档同步)                   ← 最后统一更新
```

每 Phase 结束后 `cargo build && cargo test` 通过再进入下一 Phase。

---

## 风险点

| 风险 | 缓解 |
|:-----|:-----|
| TurnPipeline 重构破坏现有对话功能 | 保留 agent_loop.rs 原始版本，Pipeline 实现后 A/B 切换 |
| MemoryLayer trait 过度抽象 | 先实现最简版本（store + recall），consolidate 可选 |
| main.rs 拆分引入编译错误 | 逐函数移动，每步编译验证 |
| Pipeline 改动影响 Discord 通知 | 确保 notify_tx 通道在 Pipeline 中正确传递 |
