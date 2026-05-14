# Nova V5 架构设计文档

> 版本: 0.1-draft
> 日期: 2026-04-24

---

## 1. 架构演进概览

V4 架构的核心矛盾：**Rust 代码在替 LLM 做决策**（删工具、过滤 schema），但 LLM 不知道也不配合。

V5 的核心转变：**Rust 提供信息，LLM 做决策，Rust 兜底**。

```
V4:  Preflight ──→ Rust 删工具 ──→ LLM 看到残缺工具 ──→ 碰壁 ──→ 幻觉

V5:  Preflight ──→ 注入 <preflight> 上下文 ──→ LLM 看到全部工具 + 决策依据
                                                    ↓
                                            按 AGENTS.md 规则自主派发
                                                    ↓
                                         Rust hard gate（安全网，正常不触发）
```

---

## 2. AGENTS.md 设计

### 2.1 物理位置

```
nova-core/
  prompts/
    AGENTS.md          ← 主 Agent 行为宪法
    SUBAGENT.md        ← SubAgent 精简版行为指令（可选）
  src/
    workspace/
      loader.rs        ← include_str!("../prompts/AGENTS.md")
```

使用 `include_str!` 编译嵌入：

```rust
// nova-core/src/workspace/loader.rs

/// 内置 Agent 行为宪法 — 不可被用户篡改
const BUILTIN_AGENTS: &str = include_str!("../prompts/AGENTS.md");
```

### 2.2 内容结构

```markdown
# Nova Agent 行为准则

## 一、任务派发

每条用户消息到达前，系统会进行预分析，结果以 <preflight> 标签注入。
你必须根据预分析结果决定处理方式：

### 派发规则
- **High 复杂度**（≥3 轮工具调用，跨文件/多步骤/浏览器多页）：
  必须调用 `delegate_complex_project` 委托给后台架构团队
- **Medium 复杂度**（1-2 轮工具调用，单文件/单次搜索）：
  必须调用 `delegate_task` 委托给后台助手
- **Low 复杂度**（无需工具或纯知识问答）：
  直接回答或使用工具处理

### <preflight> 标签格式
```xml
<preflight>
  complexity: High|Medium|Low
  topic_shift: true|false
  reason: 分类原因简述
</preflight>
```

### 重要约束
- 收到 High/Medium 时，**不得**直接使用 browser、bash 等执行工具
- 委托后立即回复用户，告知任务已派发
- 如果你认为 Preflight 分类不准确，仍然遵守分类结果

## 二、记忆系统

### 层1：MEMORY.md（工作记忆）
...（从当前 MEMORY_GUIDANCE 迁移）

### 层2：情景记忆（自动管理）
...

### 层3：历史 Session
...

## 三、技能系统

...（从当前 SKILLS_GUIDANCE 迁移）

## 四、安全红线

- 未经许可不执行破坏性操作
- `trash` 优于 `rm`
- 不泄露 MEMORY.md 中的私人信息到群聊
- 遇到不确定的操作先请示用户
```

### 2.3 Token 预算分析

| 段落 | 估计字符数 | 估计 Tokens |
|------|-----------|-------------|
| 任务派发规则 | ~800 | ~300 |
| 记忆系统规范 | ~1200 | ~500 |
| 技能系统规范 | ~800 | ~300 |
| 安全红线 | ~400 | ~150 |
| **合计** | **~3200** | **~1250** |

占 200K 上下文的 ~0.6%，完全可控。

---

## 3. System Prompt 组装管线（修订版）

### 3.1 五层架构（V5 修订）

```
┌─────────────────────────────────────────────┐
│ 第一层：Base Identity                        │
│   SOUL.md + IDENTITY.md（用户可配置）         │
├─────────────────────────────────────────────┤
│ 第二层：Agent Constitution                   │  ← V5 新增
│   AGENTS.md（内置，include_str! 编译嵌入）     │
│   包含：记忆规范、技能规范、派发规则、安全红线   │
├─────────────────────────────────────────────┤
│ 第三层：Tools & Capabilities                 │
│   所有工具描述（不再过滤）                     │  ← V5 变更
├─────────────────────────────────────────────┤
│ 第四层：User Context                         │
│   USER.md + MEMORY.md + HEARTBEAT.md         │
│   （带截断防护，I/O Shield）                   │
├─────────────────────────────────────────────┤
│ 第五层：Dynamic Injection                    │
│   <preflight> + <relevant_history>           │  ← V5 变更
│   + Memory Recall 结果                       │
└─────────────────────────────────────────────┘
```

### 3.2 与 V4 的关键差异

1. **第二层从 Rust 常量改为 Markdown 文件**：内容和形态分离。行为规则用 Markdown 编写（可读性强），通过 `include_str!` 保证编译时嵌入（安全性不变）。

2. **第三层不再动态过滤**：所有注册工具的 schema 始终完整注入。行为约束由第二层的 AGENTS.md 声明式定义，而非代码层的 if-else 删减。

3. **第五层新增 `<preflight>` 注入**：Preflight 结果从"代码内部变量"变为"对话上下文"，LLM 可感知并据此决策。

### 3.3 loader.rs 修改要点

```rust
const BUILTIN_AGENTS: &str = include_str!("../prompts/AGENTS.md");

// BOOTSTRAP_FILES 恢复 AGENTS 的位置，但来源从文件系统改为编译嵌入
pub fn build_system_prompt(&mut self, tool_descriptions: &str) -> String {
    let mut parts: Vec<String> = Vec::new();

    // 第一层：用户可配置的身份文件
    for &name in &["SOUL.md", "IDENTITY.md"] {
        let content = self.load_with_cache(name);
        if !content.is_empty() {
            parts.push(truncate_bootstrap(&content, MAX_PER_FILE_CHARS));
        }
    }

    // 第二层：内置 Agent 行为宪法（不从文件系统加载）
    parts.push(BUILTIN_AGENTS.to_string());

    // 第三层：工具描述（始终完整，不过滤）
    if !tool_descriptions.is_empty() {
        parts.push(tool_descriptions.to_string());
    }

    // 第四层：用户上下文
    for &name in &["USER.md", "HEARTBEAT.md"] {
        let content = self.load_with_cache(name);
        if !content.is_empty() {
            parts.push(truncate_bootstrap(&content, MAX_PER_FILE_CHARS));
        }
    }
    // MEMORY.md 单独注入（带专用截断）
    let memory = self.load_memory();
    if !memory.is_empty() {
        parts.push(format!("## MEMORY.md\n\n{}", truncate_bootstrap(&memory, 10_000)));
    }

    // 第五层：动态注入（由 main.rs 在 build 之后追加）
    // <preflight>, <relevant_history>, Memory Recall 在外部拼接

    parts.join("\n\n---\n\n")
}
```

---

## 4. Preflight 上下文注入流程

### 4.1 注入位置

Preflight 结果注入到 **system prompt 的末尾**（第五层），在用户消息之前：

```
System Prompt:
  [第一层] SOUL.md ...
  [第二层] AGENTS.md（含派发规则）
  [第三层] 工具描述
  [第四层] USER.md / MEMORY.md
  [第五层] <preflight>complexity: Medium ...</preflight>
           <relevant_history>...</relevant_history>

User: 我记得google也有？
```

### 4.2 loop.rs 修改

```rust
// 不再过滤 tool_schemas — 所有工具始终可见
let tool_schemas = self.tools.as_api_schemas();

// Preflight 结果注入到 system prompt
let preflight_injection = if let Some(ref result) = preflight_result {
    format!(
        "\n\n<preflight>\ncomplexity: {:?}\ntopic_shift: {}\nreason: {}\n</preflight>",
        result.complexity, result.topic_shift, result.reason
    )
} else {
    String::new()
};

let effective_system = format!("{}{}", system_prompt, preflight_injection);
```

### 4.3 Hard Gate 降级

```rust
// 保留 hard gate 作为安全网
// 但只在反复违规时才真正拦截
if let Some(ref allowed) = allowed_tool_names {
    if !allowed.contains(&tc.name) {
        // V5: 降级为 debug 日志，第一次只发 warning 给 LLM
        debug!("[V5] Tool soft-blocked: '{}' (preflight suggested delegation)", tc.name);
        // 返回建议性错误而非阻断性错误
        let err_msg = format!(
            "提示：当前 Preflight 分析建议将此任务委托执行。\
            请参考 <preflight> 标签中的分类结果，使用 delegate_task 或 delegate_complex_project。\
            如果你确认需要直接执行，请再次调用此工具。"
        );
        // ... 记录到 session，让 LLM 自行决定
    }
}
```

> **注意**：是否完全移除 hard gate 或保留为"软提示"，需要根据实际运行效果迭代。初期建议保留为"软提示 + 计数器"模式：连续 3 次违规后升级为硬拦截。

---

## 5. delegate_task 工具设计

### 5.1 与 delegate_complex_project 对比

| 维度 | delegate_task | delegate_complex_project |
|------|--------------|-------------------------|
| 目标复杂度 | Medium（1-2 轮工具调用） | High（≥3 轮，跨文件） |
| 执行方式 | 单次 Full SubAgent + 工具 | Coordinator 4 阶段流水线 |
| API 调用数 | 1-3 次 | 4+ 次 |
| 适用场景 | 单次搜索、单文件修改、代码解释 | 跨文件重构、完整功能实现 |
| 通知机制 | ShadowEvent::ProjectCompleted | ShadowEvent::ProjectCompleted |
| 取消机制 | 共享 RUNNING_PROJECTS | 共享 RUNNING_PROJECTS |

### 5.2 SubAgent 行为指令

SubAgent 需要自己的行为边界。两种选择：

**选项 A**：在 AGENTS.md 中增加 `## SubAgent 行为指南` 段落，SubAgent 启动时从 AGENTS.md 中提取此段落作为 system prompt 的一部分。

**选项 B**：独立的 `prompts/SUBAGENT.md`，同样 `include_str!` 编译嵌入。

推荐**选项 A**，KISS 原则——一个文件管所有。

---

## 6. SubAgent 资源隔离

### 6.1 Chrome Profile 隔离

```rust
// make_subagent_tools() 中为 BrowserTool 生成独立 profile
let subagent_profile = format!(
    "{}/.nova/chrome-subagent-{}",
    dirs::home_dir().unwrap().display(),
    uuid::Uuid::new_v4().to_string()[..8]
);
```

### 6.2 生命周期管理

```
SubAgent 启动
  → 创建独立 Chrome Profile 目录
  → 启动独立 Chrome 进程（headless）
  → 执行任务（1-N 轮工具调用）
  → 任务完成
  → Drop BrowserTool → kill Chrome 进程
  → 清理 Profile 目录
```

需要确保 `BrowserTool::Drop` 正确实现进程清理和目录删除。

---

## 7. 弃用清单

| 被弃用项 | 替代方案 | 清理方式 |
|---------|---------|---------|
| `loader.rs` 的 `MEMORY_GUIDANCE` 常量 | AGENTS.md §记忆系统 | 删除常量，移除 `parts.push(MEMORY_GUIDANCE)` |
| `loader.rs` 的 `SKILLS_GUIDANCE` 常量 | AGENTS.md §技能系统 | 删除常量，移除 `parts.push(SKILLS_GUIDANCE)` |
| `loop.rs` 的 `as_api_schemas_filtered` 调用 | AGENTS.md 派发规则 + `<preflight>` 注入 | 改为始终 `as_api_schemas()` |
| `~/.nova/AGENTS.md`（OpenClaw 模板） | 内置 `prompts/AGENTS.md` | 启动时 `info!` 提示迁移 |
| `<nova_os>` 管道（已在 V4 弃用） | 无（维持弃用状态） | 无需额外操作 |
