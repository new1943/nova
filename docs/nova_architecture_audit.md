# Nova 项目架构诊断报告

## 一、总体判断

**你的感觉是对的。** 项目确实处于"补丁驱动开发"阶段——从 git 历史来看，30 次提交中有 12 次是 `fix bug`，3 次 `fix browser`，commit message 几乎没有语义。代码中散布着 **127 处** 技术债标记（`[V2]` `[V4]` `[V6]` `FIXBUG` `DEPRECATED`），`main.rs` 膨胀到了 **1235 行**。

但项目**远没有废**。核心骨架（QueryLoop + IPC + ShadowEvent bus）的设计思路是正确的，16 个 Claude Code 策略的实现是扎实的。问题出在"拿着锤子找钉子"——每次遇到问题直接在现有代码上打补丁，缺少一次**从问题域到解决方案的系统性推演**。

---

## 二、核心问题诊断

### 🔴 问题 1：God Object — `main.rs` (1235 行)

`nova-daemon/src/main.rs` 是整个系统最严重的坏味道。它同时承担了：

| 职责 | 行数 | 本该属于 |
|:--|:--|:--|
| Daemon 启动 / PID 管理 | ~60 行 | 独立模块 `daemon/lifecycle.rs` |
| 工具注册 (`make_tools` + `make_subagent_tools`) | ~130 行 | `nova-core/tools/factory.rs` |
| IPC 连接处理 (`handle_connection`) | ~520 行 | `daemon/session_handler.rs` |
| Session diary / Map-Reduce 总结 | ~100 行 | `nova-core/memory/diary.rs` |
| Discord push 监听 | ~30 行 | `daemon/discord.rs` (已有但只包含 Gateway) |
| `nova_os` section 构建 | ~50 行 | **已废弃但未删除** |
| Heartbeat 启动 | ~15 行 | `daemon/heartbeat.rs` |

> **后果**：每次改任何功能都要改 `main.rs`，极易引入回归 bug。

### 🔴 问题 2：没有明确的"策略层" (Policy Layer)

系统有三种关键决策逻辑散落在不同位置：

1. **Preflight 分类** → `agent/preflight.rs`（分类器）+ `agent/loop.rs:178-197`（Hard Gate 执行）
2. **工具过滤** → `loop.rs` 行内 match + `tools/registry.rs` 的 `as_api_schemas_filtered`
3. **委派后终止** → `loop.rs:556-572`（行内 if-else）

这三者本质上是同一个策略："**什么时候用什么工具、做完后怎么办**"。但它们散布在 3 个文件的行内代码中，用注释 `[V6]` 标记代替了架构设计。

### 🟡 问题 3：模块边界模糊 — "伪模块化"

`nova-core/src/` 有 21 个子目录，但很多只是把文件放进了文件夹，并没有真正的接口隔离：

| 模块 | 文件数 | 问题 |
|:--|:--|:--|
| `memory/` | 11 文件 | TopicTracker、TensionTracker、ModeRouter、MemoryBoard 之间没有统一 trait，都直接被 QueryLoop 持有 |
| `tools/` | 19 文件 | 每个 Tool 独立但 `delegate_task.rs` 和 `delegate_complex_project.rs` 80% 代码重复 |
| `coordinator/` | 2 文件 | 仅 78+90 行，几乎就是个 SubAgent 的包装 |
| `dream/`, `heartbeat/` | 各 1-2 文件 | 功能正交但初始化逻辑全塞在 `main.rs` |

### 🟡 问题 4：废弃代码未清理

```
// [V4] ModeRouter disabled — nova_os deprecated
// [V4 DEPRECATED] Memory extraction moved to MemoryKeeper
// [V4 DEPRECATED] LLM summarization removed
// [V4 spec] AGENTS.md, STATE.md, TASKS.md are deprecated
```

`build_nova_os_section()`（50 行）已废弃但仍保留在代码中。`mode_router` 在 `handle_connection` 里仍被创建（行 603），只是不传给 QueryLoop。`CompactResult` 有 3 个 DEPRECATED 字段仍在编译。

### 🟡 问题 5：缺少测试覆盖

整个项目几乎没有集成测试，`cargo test` 只有极少的单元测试。这导致每次"修 bug"实际上是"引入新 bug"的循环。

---

## 三、架构的"好骨头" — 值得保留的部分

在讨论怎么重构之前，先确认哪些设计是好的，不需要推翻：

1. ✅ **Crate 分层** — `nova-api` → `nova-core` → `nova-daemon` → `nova-tui` 的依赖关系是合理的
2. ✅ **ShadowEvent Bus** — `events.rs` + `dispatcher.rs` 的事件总线模式是正确的
3. ✅ **IPC 协议** — JSON Lines over UDS，简单可靠
4. ✅ **QueryLoop 核心循环** — Stream → Parse → Tool Execute → Hooks 的流程清晰
5. ✅ **Token Budget + Compact** — 预算管理的双层熔断机制是有效的
6. ✅ **SideQuery** — 轻量级并行 LLM 调用的抽象干净

---

## 四、建议路线图

> [!IMPORTANT]
> 以下是渐进式重构方案，不需要推翻重来。核心原则：**每次重构只动一个模块，保证编译通过、行为不变**。

### Phase 0：清理 — 删废弃代码 (0.5 天)

- [ ] 删除 `build_nova_os_section()` 及其所有引用
- [ ] 删除 `main.rs` 中 `mode_router` 的创建和传递（QueryLoop 已不接收）
- [ ] 清理 `CompactResult` 的 3 个 DEPRECATED 字段
- [ ] 删除 `fixbug/` 目录（历史记录保留在 git 中即可）
- [ ] 清理根目录的 `~` 文件夹（不应存在）

### Phase 1：拆分 main.rs (1 天)

```
nova-daemon/src/
├── main.rs              # 仅 ~80 行：解析 CLI + 调 run_daemon
├── lifecycle.rs         # PID 管理、信号处理
├── session_handler.rs   # handle_connection 的全部逻辑
├── tool_factory.rs      # make_tools / make_subagent_tools
├── discord.rs           # Gateway + push listener (合并)
├── dispatcher.rs        # 不变
└── task_manager.rs      # 不变
```

### Phase 2：提取策略层 (1 天)

```rust
// nova-core/src/agent/policy.rs

pub struct AgentPolicy {
    preflight: PreFlightChecker,
}

impl AgentPolicy {
    /// 根据 preflight 结果，返回当前 turn 可用的工具集
    pub fn filter_tools(&self, result: &PreFlightCheckResult, registry: &ToolRegistry) -> Vec<ToolSchema> { ... }

    /// 判断 tool call 执行后是否需要终止循环
    pub fn should_terminate_after(&self, tool_calls: &[ToolCall], result: &PreFlightCheckResult) -> bool { ... }
}
```

`loop.rs` 中 `[V6] Hard Gate` 和委派终止逻辑全部委托给 `AgentPolicy`，QueryLoop 只调接口。

### Phase 3：统一 Memory 接口 (1 天)

```rust
// nova-core/src/memory/tracker.rs

/// 所有 tracker 的统一接口
pub trait ContextTracker: Send + Sync {
    async fn on_user_message(&self, content: &str);
    async fn on_tool_call(&self);
    async fn snapshot(&self) -> TrackerSnapshot; // 用于诊断/日志
}
```

TopicTracker、TensionTracker 实现此 trait。QueryLoop 持有 `Vec<Box<dyn ContextTracker>>` 而不是一堆 `Option<Arc<RwLock<...>>>`。

### Phase 4：消除 delegate 代码重复 (0.5 天)

`delegate_task.rs` 和 `delegate_complex_project.rs` 提取公共基础：

```rust
// nova-core/src/tools/delegate_base.rs
struct DelegateBase { dispatcher_tx, shadow_tx, api_key, api_base_url, model, workspace_dir, tools }

impl DelegateBase {
    async fn spawn_background(&self, task: &str, project_id: &str) -> String { ... }
}
```

### Phase 5：补关键测试 (持续)

- `policy.rs` 的单元测试（Low/Medium/High 各 2 个 case）
- `dispatcher.rs` 的集成测试（mock ShadowEvent → 验证路由）
- `QueryLoop::run_turn` 的端到端 mock 测试

---

## 五、成本与收益的经济学分析

| 方案 | 时间成本 | 风险 | 收益 |
|:--|:--|:--|:--|
| **继续打补丁** | 每个 bug 0.5-2 天 | 递增（熵增，每次改动影响面扩大） | 短期可交付 |
| **Phase 0-2 渐进重构** | 集中 2.5 天 | 低（每步可编译验证） | main.rs 从 1235→80 行，策略可测试 |
| **推翻重写** | 2-4 周 | 极高（已验证的逻辑丢失） | 理论上最优但实际上不划算 |

> [!TIP]
> **建议 Phase 0 + 1 + 2 一起做**，投入 2.5 天换一个可维护的代码基线。Phase 3-5 可以在后续功能迭代中逐步完成。

---

## 六、总结

Nova 不是一个"废了"的项目。它是一个**功能完整但架构债务累积到拐点**的项目。17449 行 Rust 代码、16 个策略实现、完整的 daemon-TUI-Discord 三端——这些都是真实的资产。

当前的核心矛盾是：**`main.rs` 承担了太多职责，策略逻辑散落在行内代码中，没有测试兜底**。结果就是"改一个 bug 引出两个 bug"的恶性循环。

解决方案不是推翻重来，而是**结构性重构**：拆文件、提接口、删废弃、补测试。用 2.5 天集中清理，换取后续每次改动的安全和效率。
