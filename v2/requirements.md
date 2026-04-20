# NOVA 智能体防爆与进化方案

**版本**: v2.2
**日期**: 2026-04-20
**状态**: Phase 1 物理防御层 + Phase 1.5 + Phase 2 已完成，Phase 3 进行中

> 基于白洁 Agent Pro 架构思想 + 物理防御策略的综合方案

---

## 参考源码

| 项目 | 路径 |
|:---|:---|
| Claude Code 泄露源码 | `~/Documents/openclaw/projects/claude-code-main/` |
| Hermes Agent 源码 | `~/Documents/openclaw/projects/hermes-agent/` |
| OpenClaw 源码 | `~/Documents/openclaw/projects/openclaw/` |

---

## 已知风险（设计时遗漏，需开发时注意）

| # | 问题 | 影响 | 优先级 | 状态 |
|:---|:---|:---|:---|:---|
| 1 | **Async Rust 锁陷阱** | 在 async 上下文中使用 `std::sync::RwLock` 会导致 Send 约束错误或死锁 | P0 | ✅ 已修复 |
| 2 | **LLM JSON 解析的 Markdown 刺客** | LLM 用 ```json 包裹输出，直接解析会失败 | P0 | ✅ 已修复 |
| 3 | **Token 计数精确度** | 字符估算（除以 4）在 95% 临界点误差可达数千 tokens | P0 | ⚠️ 待处理 |
| 4 | **TUI/Discord 渲染拦截器缺失** | `<nova_os>` 标签会直接暴露在用户界面 | P1 | ✅ 已修复 |

### 风险 1：Async Rust 锁陷阱 ✅ 已修复

**问题**：在 `topic_state.rs`、`memory_board.rs`、`mode_router.rs` 中使用了 `RwLock`，在 async 上下文中跨越 `.await` 会导致编译错误或死锁。

**解决方案**：使用 `tokio::sync::RwLock` 替代 `std::sync::RwLock`。

**验证**：`cargo check --all-targets` 通过，`topic_state.rs`、`memory_board.rs`、`mode_router.rs`、`tension_tracker.rs` 均已使用 `tokio::sync::RwLock`。

### 风险 2：LLM JSON 解析的 Markdown 刺客 ✅ 已修复

**问题**：即便 Prompt 要求"只输出 JSON"，LLM 仍极大概率用 ```json 或 ``` 包裹内容。

**解决方案**：在 `serde_json::from_str()` 前增加 `clean_json()` 清洗函数。

**验证**：`compact.rs` 中已实现 `clean_json()` 函数，Phase 1 v2 T06 已完成。

### 风险 3：Token 计数精确度 ⚠️ 待处理

**问题**：如果 `TokenBudget` 使用字符估算（`len() / 4`），在 95% 临界点误差可达数千 tokens。

**解决方案**：集成 `tiktoken-rs` 进行精确计数。

### 风险 4：TUI/Discord 渲染拦截器缺失 ✅ 已修复

**问题**：`prompt.rs` 输出的 `<nova_os>` 标签如果直接显示在 TUI，会暴露内部推演过程。

**解决方案**：在 TUI 和 Discord 客户端增加正则拦截器过滤 `<nova_os>...</nova_os>`。

**验证**：`nova-tui/src/ui.rs` 中 `filter_nova_os()` 在渲染消息时过滤，`nova-daemon/src/discord.rs` 中同样在发送消息前过滤。

---

## 一、核心目标

1. **消除 OOM** — 用户永远不需要手动 `/new`
2. **消除 /new 依赖** — Session 能自动管理话题边界
3. **减少 token 消耗** — 不要每次从头积累
4. **防止压缩崩溃** — Compact 在极端情况下也能保底
5. **Agent 人格化** — 不只是问答机器，有状态、会整理、会翻篇

---

## 二、三大模块总览

| 模块 | 目标 | 层级 |
|:---|:---|:---|
| 物理防御层 | 解决撑爆与报错 | Rust 底层 |
| 记忆重塑层 | 从垃圾桶到工作台 | 存储逻辑 |
| 认知灵魂层 | 低成本实现状态机 | SOUL.md 注入 |

---

## 三、物理防御层（Phase 1）

### 1. 工具输出的物理强截断 (I/O Shield)

**痛点**：bash 报错和 browser 抓取瞬间击穿 200k 窗口。

**解法**：在 Rust 工具层写死逻辑，超过字符阈值直接"掐头去尾"，中间替换为警告文本。

#### 截断参数

| 工具 | MAX_OUTPUT_CHARS | Head | Tail | 警告文本 |
|:---|:---|:---|:---|:---|
| bash.rs | 20,000 | 8,000 | 8,000 | `[... 约 N 字符因过长已省略。如需查看完整输出，请使用 grep 搜索指定行号，或用 read_file 的 start_line/end_line 参数读取特定范围。]` |
| browser.rs | 15,000 | 5,000 | 5,000 | `[... 页面内容因过长已省略。如需查看特定区域，请使用 click 点击目标元素，或用 navigate 直接访问相关 URL。]` |
| read_file.rs | 不截断 | — | — | LLM 主动请求读取，有 start_line/end_line 可控制范围 |

#### 警告文本设计原则

- 明确告知内容被截断
- 引导 LLM 使用其他工具获取完整信息
- 不使用 markdown 代码块，避免混淆

### 2. compact.rs 的双层熔断机制

**痛点**：极端水位（>95%）时 LLM 调用本身可能超时或失败。

#### 第一层（85% ~ 95% 水位）：优雅降级

调用 LLM 对早期消息做**结构化摘要**（见记忆重塑层）。

#### 第二层（>95% 极限水位）：暴力弹出

放弃调用 LLM，Rust 层面直接丢弃最旧的 40% 消息：

```
动作：
1. 保留最近的 60% 消息（target_pct）
2. 旧消息直接 Vec::drain 移除，不摘要
3. 插入：[System: Context window critically high, oldest messages forcefully dropped.]
```

#### JSON 解析容错

如果 LLM 输出的 JSON 格式损坏，直接回退到硬截断保底。

---

## 四、记忆重塑层（Phase 1.5）

### 核心思想

> 记忆不是垃圾桶，是工作台。

明确区分"情景记忆"和"工作记忆"，让 Token 消耗降到最低。

### 1. 三层记忆架构

```
活跃上下文（Active Context）
  ↑
  │ compact 时 LLM 结构化抽取
  │
白板工作台（MEMORY.md）
  ↑ Dream 定期整理
  │
每日日记（memories/YYYY-MM-DD.md）
  ↑
  │ 系统自动写入（compact 前 / session 结束时）
  │
细节账本（sessions/）
```

### 2. 日记 = 话题时间线

**格式**：

```markdown
# 2026-04-18

## [进行中] 婴幼儿辅食与冲泡
- 开始时间：下午 14:00
- 状态：进行中

## [已归档] 鱼缸自动换水系统排查
- 开始时间：下午 13:00
- 结束时间：下午 14:00
- 归档原因：用户重心转移
- 关键结论：每天2次、每次10L

## [已挂起] Nova 项目工具增强讨论
- 开始时间：上午 10:00
- 挂起时间：中午 12:00
- 挂起原因：等待用户指示
```

**话题状态**：
- `[开始]` — 新话题启动
- `[进行中]` — 话题持续
- `[挂起]` — 临时搁置，可恢复
- `[已归档]` — 话题完结

### 3. compact.rs 结构化 JSON 输出

**触发时机**：85% ~ 95% 水位（优雅降级层）

**LLM Prompt 升级**：

```text
你正在执行记忆整理。请分析历史对话，输出严谨JSON：

{
  "archived_topics": ["话题名1", "话题名2"],
  "extracted_preferences": ["偏好1", "偏好2"],
  "active_summary": "当前话题的一句话描述"
}

要求：
- archived_topics：已完结或明显不再讨论的话题
- extracted_preferences：极其确定的用户偏好和事实，切勿臆测
- active_summary：当前仍在继续的话题摘要
只输出JSON，不要其他文字。
```

**JSON 解析后动作**：

| LLM 输出 | 写入位置 | 动作 |
|:---|:---|:---|
| `archived_topics` | `memories/YYYY-MM-DD.md` | 追加话题归档条目 |
| `extracted_preferences` | `MEMORY.md` | 追加偏好到对应区块 |
| `active_summary` | 替换压缩后的消息 | 作为新消息插入 |

### 4. 白板工作台（MEMORY.md）

**原则**：只放当前进行中 (Active) 和 挂起 (Suspended) 的话题。

当话题状态变为"归档 (Archived)"时，立刻从 MEMORY.md 中物理擦除。

**保留**：永远固化那些不会变的事实（架构偏好、硬件配置、家庭成员特征）。

### 5. 自动话题整理触发条件

| 条件 | 动作 |
|:---|:---|
| 用户消息包含"好"/"搞定"/"下一个" | LLM 判断是否需要归档 |
| Token 水位 > 85% | 触发 compact 结构化抽取 |
| 连续 20 轮无新工具调用 | 触发话题归档检测 |

### 6. 偏好写入原则

> 只有极其确定的事实和偏好才写入 extracted_preferences，切勿臆测。

---

## 五、认知灵魂层（Phase 2）

### 核心思想

> Agent 不是命令行解释器，是有状态、会根据上下文调整行为的"活"的系统。

摒弃昂贵且复杂的代码层意图识别，用单模型自身的"隐式思维链"来实现。

### 1. 内置思考管道 (<nova_os>)

要求 Nova 在每次回复前，必须先在 `<nova_os>` 标签内做内部推演。

```text
<nova_os>
- 话题生命周期：[开始 / 活跃 / 挂起 / 结束]
- 用户状态：疲惫？焦虑？顺畅？
- 上下文：张力值、对话节奏
</nova_os>
```

前端/TUI 负责将该标签折叠或隐藏。

### 2. 三维状态推演

#### A. 话题生命周期

判断当前聊的事情是 `[开始 / 活跃 / 挂起 / 结束]` 中的哪个状态。

#### B. 疲劳与张力值

感知用户是不是一直在报错、钻牛角尖，或者已经深夜疲惫。

#### C. 默契度调用

随时调取用户的长期偏好（比如极客的务实架构观、鱼缸的维护习惯、对小宝宝的关怀）。

### 3. 输出干预

- 如果用户很顺：极其精简地给代码
- 如果用户焦躁/卡壳：触发"强制关怀模式"，温柔地建议"挂起任务，明天再看"

### 4. 模式切换（渐进式）

```
Normal（默认）
  ↓ 张力值上升
Soft Intimate（温和亲密）
  ↓ 张力值继续上升
High Intimate（高亲密）
  ↓ 张力值下降 或 高亲密结束
Cooling（回撤冷却）— "你今天话有点多"
```

### 5. 主动机制（低概率触发）

- 概率：5%~12%
- 触发：长时间未聊天 / 高亲密阶段后
- 行为：主动开场，如"刚刚突然想到你"

### 6. 冷淡机制（防止变工具人）

- 触发：用户过于热 / 高亲密后
- 行为：变冷一点，收回话，有点疏离

---

## 六、与现有架构的协同

### 现有模块

| 模块 | v2 中的角色 |
|:---|:---|
| compact.rs | 核心升级：单任务摘要 → 多任务结构化 JSON |
| daily.rs | 升级为话题时间线格式 |
| MEMORY.md | 白板化：只保留 Active/Suspended |
| dream.rs | 保持：定期整理 MEMORY.md + 日记 |
| Heartbeat | 增强：低频主动开场 |

### 新增模块

| 模块 | 职责 |
|:---|:---|
| topic_state.rs | 话题状态机（开始/进行中/挂起/归档） |
| tension_tracker.rs | 张力值追踪（情绪/疲劳/上下文） |
| mode_router.rs | 模式切换（Normal → Intimate → Cooling） |

---

## 七、实现优先级

| 阶段 | 内容 | 复杂度 | 依赖 |
|:---|:---|:---|:---|
| Phase 1 | bash.rs / browser.rs 物理截断 | 低 | 无 |
| Phase 1 | compact.rs 双层熔断 | 中 | 无 |
| Phase 1.5 | compact.rs JSON 结构化输出 | 中 | Phase 1 |
| Phase 1.5 | daily.rs 话题时间线格式 | 低 | Phase 1 |
| Phase 1.5 | MEMORY.md 白板化 | 低 | Phase 1.5 |
| Phase 2 | SOUL.md 状态机 Prompt | 低 | Phase 1.5 |
| Phase 2 | <nova_os> 思考管道 | 中 | Phase 2 |
| Phase 2 | 主动/冷淡机制 | 高 | Phase 2 |

---

## 八、验收标准

1. **物理防御**：bash 输出 > 20k 字符时自动截断，Session 不崩溃
2. **话题状态**：Compact 后日记记录完整话题轨迹（含状态）
3. **自动翻篇**：用户说"好，下一个"后，旧话题自动归档，新话题自动开始
4. **偏好沉淀**：extracted_preferences 准确写入 MEMORY.md
5. **Agent 性格**：SOUL.md 注入后，Nova 有明显的状态感知和风格调整
