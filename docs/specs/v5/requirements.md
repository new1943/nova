# Nova V5 需求规格说明书

> 版本: 0.1-draft
> 日期: 2026-04-24

---

## 1. 背景

V4 重构实现了 ShadowEvent 总线、Preflight 分类器、Coordinator 4 阶段流水线等核心机制，但在实际运行中暴露了新的架构问题：

### 1.1 行为逻辑碎片化

Agent 的行为规范散落在 **5 个不同位置**：

| 位置 | 内容 | 形态 |
|------|------|------|
| `loader.rs` 的 `MEMORY_GUIDANCE` 常量 | 记忆系统使用规范 | Rust `const &str` |
| `loader.rs` 的 `SKILLS_GUIDANCE` 常量 | 技能系统使用规范 | Rust `const &str` |
| `loop.rs` L184-192 的 if-else | 任务派发规则 | Rust 代码逻辑 |
| `preflight.rs` 的 `PREFLIGHT_SYSTEM` | 复杂度分类标准 | Rust `const &str` |
| `~/.nova/AGENTS.md`（已废弃） | OpenClaw 通用 Agent 行为模板 | 用户可编辑文件 |

这导致：
- 修改行为规则需要改 Rust 代码 → 编译 → 重启 daemon
- LLM 看到的 system prompt 是碎片拼装的，缺乏统一的"自我认知"
- V4 的 AGENTS.md 被标记为 deprecated 但没有替代方案

### 1.2 Preflight 控制权错位

V4 设计中 Preflight 的角色是**控制器**——它的分类结果直接驱动 Rust 代码删除工具（`as_api_schemas_filtered`）。这造成了：

1. **LLM 信息不对称**：system prompt 描述了所有工具，但 tool_schemas 只有 2 个 → LLM 从训练记忆中调用被删的工具 → 被 hard gate 拦截 → 浪费 2-3 轮 API 调用
2. **MiniMax API 不遵守工具限制**：即使只传 2 个 tool schema，LLM 仍会调用不在列表里的工具
3. **Medium 被过度委派**：Medium 和 High 走同一条路径（Coordinator 4 阶段），简单的"搜一下"被强制走 Research → Synthesis → Implementation → Verification

### 1.3 SubAgent 工具隔离缺失

SubAgent 和主 Agent 共享同一个 `BrowserTool` 实例的底层 Chrome 进程，CDP WebSocket session 不支持并发 → 消息交错 → 大量 `WS Invalid message` 错误。

---

## 2. 核心目标

### 目标 A：统一 Agent 行为宪法

引入内置 `AGENTS.md`，作为主 Agent 不可篡改的最高行为准则。将所有散落的行为指令（记忆规范、技能规范、任务派发规则、安全红线）统一收归此文件。

### 目标 B：Preflight 角色重定义

将 Preflight 从"控制器"降级为"顾问"。Preflight 的分类结果以结构化上下文 `<preflight>` 注入对话流，LLM 根据 AGENTS.md 中的规则自主决策，Rust 侧 hard gate 降级为最后防线。

### 目标 C：Medium 轻量级委派

新增 `delegate_task` 工具，Medium 复杂度任务走单次 SubAgent（不走 Coordinator 4 阶段），减少不必要的 API 调用消耗。

### 目标 D：SubAgent 资源隔离

SubAgent 的 BrowserTool 使用独立的 Chrome Profile，避免与主 Agent 的 CDP session 冲突。

---

## 3. 功能需求

### FR-1: 内置 AGENTS.md（Agent 行为宪法）

- **FR-1.1 文件位置**：`nova-core/prompts/AGENTS.md`，通过 `include_str!` 编译嵌入二进制。用户不可修改。
- **FR-1.2 内容涵盖**：
  - 记忆系统操作规范（替代 `MEMORY_GUIDANCE` 常量）
  - 技能系统操作规范（替代 `SKILLS_GUIDANCE` 常量）
  - 任务派发规则（替代 `loop.rs` 的 if-else 工具过滤）
  - 工具使用指南与约束
  - 安全红线与禁止事项
  - `<preflight>` 标签解读规则
- **FR-1.3 注入位置**：在 system prompt 的第二层（System Enforcements），位于 SOUL.md/IDENTITY.md 之后、用户上下文之前。
- **FR-1.4 与用户文件边界**：

  | 层级 | 文件 | 性质 | 用户可改 |
  |------|------|------|---------|
  | 第一层 | SOUL.md, IDENTITY.md | 人格与身份 | ✅ |
  | 第二层 | **AGENTS.md**（内置） | 行为宪法 | ❌ |
  | 第三层 | 工具描述（动态生成） | 能力声明 | ❌ |
  | 第四层 | USER.md, MEMORY.md, HEARTBEAT.md | 用户上下文 | ✅ |
  | 第五层 | `<preflight>`, `<relevant_history>` | 动态注入 | ❌ |

### FR-2: Preflight 上下文注入

- **FR-2.1**：Preflight 分类结果以 `<preflight>` XML 标签注入到**用户消息前方**（或 system prompt 的动态注入层）：
  ```xml
  <preflight>
    complexity: Medium
    topic_shift: false
    reason: 用户追问Google code wiki，预估1轮搜索
  </preflight>
  ```
- **FR-2.2**：AGENTS.md 中定义 `<preflight>` 的解读规则，LLM 根据规则自主选择工具。
- **FR-2.3**：Rust 侧**不再主动过滤** tool_schemas。所有工具始终对 LLM 可见。
- **FR-2.4**：Rust 侧 hard gate 保留为安全网，但日志级别降为 `debug`。仅在 LLM 反复违规时升级为 `warn` 并拦截。

### FR-3: delegate_task 轻量委派工具

- **FR-3.1**：新建 `delegate_task` 工具，启动单次 Full SubAgent（带完整工具集），不走 Coordinator 4 阶段。
- **FR-3.2**：与 `delegate_complex_project` 共享 `RUNNING_PROJECTS` 注册表（统一取消机制）。
- **FR-3.3**：完成后通过 `ShadowEvent::ProjectCompleted` 通知用户。

### FR-4: SubAgent 资源隔离

- **FR-4.1**：SubAgent 的 BrowserTool 使用独立的 Chrome Profile 目录（`~/.nova/chrome-subagent-{uuid}/`）。
- **FR-4.2**：SubAgent 完成后清理 Profile 目录和 Chrome 进程。
- **FR-4.3**：SubAgent 自身也应有一个精简版的行为指令（可从 AGENTS.md 中提取 SubAgent 专属段落，或独立定义）。

### FR-5: 废弃清理

- **FR-5.1**：删除 `loader.rs` 中的 `MEMORY_GUIDANCE` 和 `SKILLS_GUIDANCE` 常量（已迁移到 AGENTS.md）。
- **FR-5.2**：删除 `loop.rs` 中的工具过滤逻辑（L184-192 的 `as_api_schemas_filtered` 调用）。
- **FR-5.3**：清理用户工作区 `~/.nova/AGENTS.md`（OpenClaw 模板），或在 daemon 启动时给出迁移提示。

---

## 4. 非功能需求

- **可维护性**：修改 Agent 行为规则只需编辑 `nova-core/prompts/AGENTS.md` 并重新编译，无需改 Rust 逻辑。
- **Token 预算**：AGENTS.md 总字符数控制在 5000 字以内（约 2000 tokens），占 200K 上下文的 ~1%。
- **向后兼容**：如果用户工作区仍有旧的 `AGENTS.md`，不加载、不报错、首次运行时输出一条 `info` 日志提示迁移。
- **可观测性**：Preflight 注入的 `<preflight>` 标签内容必须出现在 `daemon.log` 中（`debug` 级别），便于排查分类问题。

---

## 5. 与 V4 的演进关系

| V4 设计 | V5 变化 | 原因 |
|---------|---------|------|
| FR-4.1: "核心纪律硬编码于 Rust 内核" | 改为 `include_str!` 编译嵌入 Markdown | Markdown 比 Rust `const` 更易维护，`include_str!` 同样保证不可运行时篡改 |
| 设计 2.1: "从 ToolRegistry 中动态剔除工具" | 改为 Preflight 上下文注入，LLM 自主决策 | MiniMax API 不遵守工具限制，剔除工具造成信息不对称 |
| 设计 2.4 第三层: "根据拦截结果动态生成工具列表" | 所有工具始终可见，行为约束在 AGENTS.md | 工具可见性和使用规则分离，更灵活 |
| AGENTS.md: "废弃，不加载" | 重新启用，但改为**内置**而非用户文件 | AGENTS.md 的概念是对的（统一行为定义），问题在于不应该让用户改 |
