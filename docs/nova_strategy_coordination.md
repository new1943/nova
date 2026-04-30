# Nova 策略协调机制设计

> 解决"各自为战"问题的核心：TurnPipeline 流水线

---

## 一、当前的策略冲突地图

先把问题完全摊开——Nova 目前有 **7 个独立的策略/机制**，分布在 **4 层不同的执行介质**上，彼此之间没有显式的协调协议：

```
┌──────────────────────────────────────────────────────────┐
│ 层1: Rust 硬编码 (loop.rs)                               │
│   ① Preflight 分类器 → complexity=High/Medium/Low        │
│   ② Hard Gate 工具过滤 → 物理隔离不可见工具               │
│   ③ 委派后终止 → if delegated { break }                  │
│   ④ TopicTracker → 话题状态机（Archive/NewTopic）         │
│   ⑤ TensionTracker → 情绪/疲劳检测                       │
├──────────────────────────────────────────────────────────┤
│ 层2: 提示词软约束 (AGENTS.md, include_str!)               │
│   ⑥ 派发规则 → "High/Medium 必须派发"                     │
│   ⑥ 记忆规范 → "先读 MEMORY.md"                          │
│   ⑥ 安全红线 → "不泄露隐私"                               │
├──────────────────────────────────────────────────────────┤
│ 层3: 上下文注入 (system prompt 拼接)                      │
│   ⑦ <preflight> 标签注入                                  │
│   ⑦ <current_tasks> 标签注入                              │
│   ⑦ <relevant_history> 注入                               │
│   ⑦ MEMORY.md 注入                                        │
├──────────────────────────────────────────────────────────┤
│ 层4: 事件总线 (ShadowEvent dispatcher)                    │
│   TaskProgress / TopicArchived / SystemIdle / ProjectCompleted │
└──────────────────────────────────────────────────────────┘
```

### 已发生的冲突案例

| 冲突 | 原因 | 后果 |
|:--|:--|:--|
| AGENTS.md 说"High 必须派发"，Hard Gate 也强制只给 delegate 工具 | **双重执行**：提示词和代码各管各 | LLM 收到自相矛盾的约束，有时先回复再派发 |
| Preflight 说 `complexity=Low`，但 AGENTS.md 说"可自行升级为 delegate_task" | **矛盾指令**：代码锁死了工具，提示词又说可以升级 | Low 模式下 LLM 想派发但工具不可见，报错 |
| TopicTracker 检测到 topic_shift，Preflight 也检测到 topic_shift | **重复检测**：两个独立机制做同一件事 | 两次触发 TopicArchived，浪费 token |
| TensionTracker 更新情绪状态，但 ModeRouter 已禁用 | **断裂链路**：TensionTracker 的输出没有消费者 | 无效计算，增加延迟 |
| 委派后终止 break，但 StopHooks 可能还需要执行 | **生命周期冲突**：提前退出跳过了本该执行的逻辑 | 部分 hook 未触发 |

### 根因：没有"策略总线"

每个策略直接读写 QueryLoop 的内部状态，彼此之间没有数据契约。就像一个公司的 7 个部门都直接修改同一份 Excel 表，没有人知道谁改了什么、改了之后谁需要知道。

---

## 二、核心设计：TurnPipeline 流水线

### 思路

借鉴**编译器 Pass Pipeline** 和 **Web 中间件链**的模型：

- 每个策略是流水线上的一个 **Stage**
- 所有 Stage 共享同一个 **TurnContext** 数据结构
- Stage 有**严格的执行顺序**
- 每个 Stage **只写自己的字段**，读别人的字段
- 最终的决策由 TurnContext 的**终态**决定，而不是任何单个 Stage

```
用户消息
   │
   ▼
┌─────────┐   ┌─────────┐   ┌──────────┐   ┌─────────┐   ┌─────────┐
│ Classify │──▶│ Track   │──▶│ Gate     │──▶│ Inject  │──▶│ Execute │
│ (Preflight)  │ (Topic/ │   │ (工具过滤) │   │ (上下文) │   │ (API调用)│
│          │   │ Tension)│   │          │   │          │   │          │
└─────────┘   └─────────┘   └──────────┘   └─────────┘   └─────────┘
   │               │              │              │              │
   └───────────────┴──────────────┴──────────────┴──────────────┘
                          共享 TurnContext
```

### TurnContext 数据结构

```rust
/// 贯穿整个 Turn 生命周期的上下文——所有策略的唯一数据通道
pub struct TurnContext {
    // ── 输入（不可变）──────────────────────────
    pub user_input: String,
    pub recent_messages: Vec<Message>,
    pub session_id: String,

    // ── Stage 1: Classify 写入 ─────────────────
    pub complexity: Complexity,        // High / Medium / Low
    pub topic_shift: bool,
    pub classify_reason: String,

    // ── Stage 2: Track 写入 ────────────────────
    pub topic_transition: TopicTransition,  // None / Archive / NewTopic
    pub tension_level: u8,                  // 0-100
    pub topic_name: Option<String>,

    // ── Stage 3: Gate 写入 ─────────────────────
    pub allowed_tools: Vec<String>,    // 最终可见工具列表
    pub gate_reason: String,           // 解释为什么这样过滤

    // ── Stage 4: Inject 写入 ───────────────────
    pub prompt_injections: Vec<PromptInjection>,  // 按优先级排序

    // ── Stage 5: Execute 写入 ──────────────────
    pub should_terminate: bool,        // 委派后是否终止
    pub post_events: Vec<ShadowEvent>, // 执行后要发送的事件

    // ── 可观测性 ───────────────────────────────
    pub decision_log: Vec<DecisionEntry>,  // 每个 Stage 的决策记录
}

pub struct DecisionEntry {
    pub stage: String,
    pub decision: String,
    pub reason: String,
    pub timestamp: Instant,
}

pub struct PromptInjection {
    pub tag: String,       // 如 "preflight", "current_tasks"
    pub content: String,
    pub priority: u8,      // 数字越小优先级越高
}
```

### Stage Trait

```rust
#[async_trait]
pub trait PipelineStage: Send + Sync {
    /// Stage 名称，用于日志和 decision_log
    fn name(&self) -> &str;

    /// 执行阶段逻辑，修改 TurnContext
    async fn execute(&self, ctx: &mut TurnContext) -> Result<()>;
}
```

### TurnPipeline 组装

```rust
pub struct TurnPipeline {
    stages: Vec<Box<dyn PipelineStage>>,
}

impl TurnPipeline {
    pub fn new() -> Self {
        Self { stages: vec![] }
    }

    pub fn add(mut self, stage: Box<dyn PipelineStage>) -> Self {
        self.stages.push(stage);
        self
    }

    /// 按序执行所有 Stage
    pub async fn run(&self, ctx: &mut TurnContext) -> Result<()> {
        for stage in &self.stages {
            stage.execute(ctx).await?;
            // 每个 Stage 执行后自动记录
            tracing::debug!("[Pipeline] {} completed", stage.name());
        }
        Ok(())
    }
}
```

---

## 三、5 个 Stage 的具体职责

### Stage 1: ClassifyStage（原 Preflight）

```rust
impl PipelineStage for ClassifyStage {
    fn name(&self) -> &str { "classify" }

    async fn execute(&self, ctx: &mut TurnContext) -> Result<()> {
        let result = self.checker.check(&ctx.user_input, &ctx.recent_messages).await;

        ctx.complexity = result.complexity;
        ctx.topic_shift = result.topic_shift;
        ctx.classify_reason = result.reason;

        ctx.decision_log.push(DecisionEntry {
            stage: "classify".into(),
            decision: format!("complexity={:?}, topic_shift={}", ctx.complexity, ctx.topic_shift),
            reason: ctx.classify_reason.clone(),
            timestamp: Instant::now(),
        });
        Ok(())
    }
}
```

**只负责**：分类。**不负责**：工具过滤、上下文注入。

### Stage 2: TrackStage（Topic + Tension 合并）

```rust
impl PipelineStage for TrackStage {
    fn name(&self) -> &str { "track" }

    async fn execute(&self, ctx: &mut TurnContext) -> Result<()> {
        // Topic 追踪 — 使用 Classify 的 topic_shift 结果，避免重复检测
        let transition = if ctx.topic_shift {
            // Preflight 已确认话题转移，直接 Archive
            self.topic_tracker.write().await.force_archive().await
        } else {
            self.topic_tracker.write().await.on_user_message(&ctx.user_input).await
        };
        ctx.topic_transition = transition;

        // Tension 追踪
        self.tension_tracker.update_from_message(&ctx.user_input).await;
        ctx.tension_level = self.tension_tracker.current_tension().await;

        // 如果 topic 需要归档，记录到 post_events（在 Execute 之后统一发送）
        if matches!(ctx.topic_transition, TopicTransition::Archive) {
            ctx.post_events.push(ShadowEvent::TopicArchived {
                transcript: ctx.recent_messages.clone(),
            });
        }

        ctx.decision_log.push(DecisionEntry {
            stage: "track".into(),
            decision: format!("topic={:?}, tension={}", ctx.topic_transition, ctx.tension_level),
            reason: format!("topic_shift from classify: {}", ctx.topic_shift),
            timestamp: Instant::now(),
        });
        Ok(())
    }
}
```

**关键改进**：TopicTracker **读取** Classify 的 `topic_shift` 结果，而不是自己再做一次检测。消除重复。

### Stage 3: GateStage（工具过滤 — 唯一真相源）

```rust
impl PipelineStage for GateStage {
    fn name(&self) -> &str { "gate" }

    async fn execute(&self, ctx: &mut TurnContext) -> Result<()> {
        // ┌─────────────────────────────────────────┐
        // │ 工具可见性的唯一决策点 — 没有第二个地方  │
        // └─────────────────────────────────────────┘
        let (tools, reason) = match ctx.complexity {
            Complexity::High => (
                vec!["delegate_complex_project", "cancel_delegated_project"],
                "High complexity → delegate only",
            ),
            Complexity::Medium => (
                vec!["delegate_task", "cancel_delegated_project"],
                "Medium complexity → delegate_task only",
            ),
            Complexity::Low => (
                self.registry.all_tool_names(),
                "Low complexity → all tools visible",
            ),
        };

        ctx.allowed_tools = tools.into_iter().map(String::from).collect();
        ctx.gate_reason = reason.into();

        ctx.decision_log.push(DecisionEntry {
            stage: "gate".into(),
            decision: format!("{} tools allowed", ctx.allowed_tools.len()),
            reason: ctx.gate_reason.clone(),
            timestamp: Instant::now(),
        });
        Ok(())
    }
}
```

### Stage 4: InjectStage（上下文注入 — 统一管理）

```rust
impl PipelineStage for InjectStage {
    fn name(&self) -> &str { "inject" }

    async fn execute(&self, ctx: &mut TurnContext) -> Result<()> {
        // 统一收集所有注入项，按优先级排序
        // 不再散落在 main.rs 的各个角落

        // 1. Preflight 标签
        ctx.prompt_injections.push(PromptInjection {
            tag: "preflight".into(),
            content: format!(
                "<preflight>\ncomplexity: {:?}\ntopic_shift: {}\nreason: {}\n</preflight>",
                ctx.complexity, ctx.topic_shift, ctx.classify_reason
            ),
            priority: 10,
        });

        // 2. 活跃任务
        if let Ok(tasks) = TaskLogger::read_context(&self.workspace_dir).await {
            if !tasks.is_empty() {
                ctx.prompt_injections.push(PromptInjection {
                    tag: "current_tasks".into(),
                    content: format!("<current_tasks>\n{}\n</current_tasks>", tasks),
                    priority: 20,
                });
            }
        }

        // 3. 历史召回（有预算上限）
        if let Ok(history) = self.recall_history(ctx).await {
            ctx.prompt_injections.push(PromptInjection {
                tag: "relevant_history".into(),
                content: history,
                priority: 30,
            });
        }

        // 按 priority 排序
        ctx.prompt_injections.sort_by_key(|i| i.priority);
        Ok(())
    }
}
```

### Stage 5: ExecuteConfig（不是真的执行，而是配置执行行为）

```rust
impl PipelineStage for ExecuteConfigStage {
    fn name(&self) -> &str { "execute_config" }

    async fn execute(&self, ctx: &mut TurnContext) -> Result<()> {
        // 委派后是否终止 — 唯一决策点
        ctx.should_terminate = matches!(
            ctx.complexity,
            Complexity::High | Complexity::Medium
        );

        ctx.decision_log.push(DecisionEntry {
            stage: "execute_config".into(),
            decision: format!("terminate_after_delegation={}", ctx.should_terminate),
            reason: format!("complexity={:?}", ctx.complexity),
            timestamp: Instant::now(),
        });
        Ok(())
    }
}
```

---

## 四、解决"代码硬编码 vs 提示词软约束"的双轨冲突

这是最容易出问题的地方。当前的状态：

```
AGENTS.md（提示词）说: "High/Medium 必须派发"
Hard Gate（代码）做: 物理隔离工具，只给 delegate

→ LLM 收到的信号: "你必须派发" + "你只能看到 delegate 工具"
→ 两个约束做同一件事，但如果一个改了另一个没改 → 矛盾
```

### 原则：**代码做执行，提示词做解释**

```
┌──────────────────────────────────────────────────────┐
│ 代码层 (Gate) — "你能做什么"（不可绕过）              │
│   → 物理过滤工具可见性                                │
│   → 委派后强制终止循环                                │
│   → 这些是 LLM 无法违反的硬约束                      │
│                                                      │
│ 提示词层 (Inject) — "为什么这样、怎么做"（引导行为）  │
│   → 告诉 LLM 当前的 complexity 分类                  │
│   → 解释可用工具的用途和调用方式                      │
│   → 引导回复风格和语气                                │
│   → 这些是让 LLM 做出更好决策的软引导                │
└──────────────────────────────────────────────────────┘
```

### 具体执行

AGENTS.md 的内容重新划分：

| 当前 AGENTS.md 内容 | 归属 | 理由 |
|:--|:--|:--|
| "High/Medium **必须**调用 delegate" | **删除** | 代码已硬性保证，提示词重复约束只会引起混淆 |
| "委派后不得调用其他工具" | **删除** | 代码已 `break`，提示词多余 |
| "仍然遵守分类结果" | **删除** | 工具已被物理隔离，遵不遵守都一样 |
| "Low 可自行升级为 delegate_task" | **保留但改为**: "Low 模式下你有所有工具" | 不再矛盾 |
| "先读 MEMORY.md" | **保留** | 纯行为引导 |
| 记忆规范、技能系统、安全红线 | **保留** | 纯行为引导 |
| SubAgent 行为指南 | **保留** | 纯行为引导 |

**核心规则**：如果一个约束已经被代码强制执行了，提示词**不要重复说一遍**，否则就是制造矛盾的温床。提示词只说**代码做不到的事**（风格、语气、判断）。

---

## 五、Pipeline 在 QueryLoop 中的使用

```rust
// 重构后的 QueryLoop::run_turn

pub async fn run_turn(&self, session: &mut Session, ...) -> Result<TurnResult> {
    // 1. 创建 TurnContext
    let mut ctx = TurnContext::new(&session);

    // 2. 执行 Pipeline — 所有策略按序执行，写入 TurnContext
    self.pipeline.run(&mut ctx).await?;

    // 3. 使用 TurnContext 的终态构建 API 请求
    let tool_schemas = self.tools.filter_by_names(&ctx.allowed_tools);
    let injections: String = ctx.prompt_injections.iter()
        .map(|i| i.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let effective_prompt = format!("{}\n{}", system_prompt, injections);

    // 4. 核心循环（已经很干净了）
    loop {
        let resp = self.llm.stream(&req, tx).await?;
        // ... 收集响应、执行工具 ...

        if tool_calls.is_empty() { break; }
        if ctx.should_terminate && has_delegation(&tool_calls) { break; }
    }

    // 5. 发送 post_events（Topic 归档等）
    for event in ctx.post_events {
        self.event_bus.emit(event);
    }

    // 6. 返回结果 + decision_log（可选输出到日志）
    for entry in &ctx.decision_log {
        debug!("[Pipeline] {}: {} ({})", entry.stage, entry.decision, entry.reason);
    }

    Ok(TurnResult { session, new_messages, ctx })
}
```

---

## 六、Pipeline 如何组装（在 nova-daemon 中）

```rust
// nova-daemon/src/main.rs

let pipeline = TurnPipeline::new()
    .add(Box::new(ClassifyStage::new(preflight_checker)))
    .add(Box::new(TrackStage::new(topic_tracker, tension_tracker)))
    .add(Box::new(GateStage::new(tool_registry.clone())))
    .add(Box::new(InjectStage::new(workspace_dir.clone())))
    .add(Box::new(ExecuteConfigStage));
```

如果要加新策略（比如"夜间模式降低工具权限"），只需要插入一个新 Stage：

```rust
let pipeline = TurnPipeline::new()
    .add(Box::new(ClassifyStage::new(preflight_checker)))
    .add(Box::new(TrackStage::new(topic_tracker, tension_tracker)))
    .add(Box::new(NightModeStage::new()))  // ← 新增
    .add(Box::new(GateStage::new(tool_registry.clone())))
    .add(Box::new(InjectStage::new(workspace_dir.clone())))
    .add(Box::new(ExecuteConfigStage));
```

`NightModeStage` 可以在 `GateStage` 之前修改 `ctx.complexity`，或者 `GateStage` 读取 `NightModeStage` 写入的自定义字段。所有交互都通过 TurnContext，**没有隐式依赖**。

---

## 七、与前面架构方案的关系

TurnPipeline 是 V2 架构中 `AgentPolicy` trait 的**具体实现**：

```rust
impl AgentPolicy for TurnPipeline {
    async fn decide(&self, user_input: &str, messages: &[Message], tools: &ToolRegistry) -> PolicyDecision {
        let mut ctx = TurnContext::new(user_input, messages);
        self.run(&mut ctx).await.ok();

        PolicyDecision {
            allowed_tools: ctx.allowed_tools,
            terminate_after_delegation: ctx.should_terminate,
            prompt_injection: ctx.prompt_injections.iter()
                .map(|i| i.content.clone())
                .collect::<Vec<_>>()
                .join("\n\n"),
        }
    }
}
```

也就是说：
- `AgentPolicy` 是 **trait 接口**（定义在 nova-core）
- `TurnPipeline` 是 **默认实现**（定义在 nova-agent）
- 如果需要完全不同的策略模型，实现另一个 `AgentPolicy` 即可

---

## 八、总结

当前的核心病因不是"策略太多"，而是**策略之间没有显式的数据通道和执行顺序**。每个策略直接操作 QueryLoop 的内部状态，就像 7 个人同时编辑同一个文件但没有 git。

TurnPipeline 的解决方案：

| 问题 | 当前 | Pipeline 后 |
|:--|:--|:--|
| 策略执行顺序 | 隐式（代码行号决定） | **显式**（Pipeline 定义顺序） |
| 策略间数据传递 | 直接读写 QueryLoop 字段 | **通过 TurnContext** |
| 重复检测（topic_shift） | Preflight 和 TopicTracker 各做一次 | TrackStage **读取** ClassifyStage 的结果 |
| 代码 vs 提示词冲突 | 两者重复约束 | **代码做执行，提示词做解释**，不重复 |
| 可观测性 | 只有 info! 日志 | **decision_log** 完整记录每步决策 |
| 扩展新策略 | 改 loop.rs 行内代码 | **插入新 Stage**，不改现有代码 |
