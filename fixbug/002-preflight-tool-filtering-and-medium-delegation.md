# FIXBUG-002: Preflight 工具限制泄漏 & Medium 过度委派

> 创建日期：2026-04-24
> 优先级：P1（直接影响用户体验 + 浪费 token）
> 状态：方案已确认，待实施

---

## 问题 1：LLM 无视 tool_schemas 限制，反复碰壁

### 现象

当 Preflight 判定 `complexity=Medium` 或 `complexity=High` 后，主循环将 API 请求的 `tool_schemas` 过滤为仅 `delegate_complex_project` + `cancel_delegated_project`。但 MiniMax LLM **仍然尝试调用 `browser` 等工具**，被 hard gate 拦截后返回错误。LLM 收到错误后不认命，**再次尝试**相同工具 → 再次被拦截。最终浪费 2-3 轮 API 调用后，LLM 放弃工具调用，**直接输出训练数据猜测的答案**（幻觉）。

### 日志证据

```
[V4] High/Medium complexity — restricting tools to delegate_complex_project
Tool call: name=browser, args={"action":"navigate","url":"..."}
[V4] Tool BLOCKED: 'browser' not in allowed set {"delegate_complex_project", "cancel_delegated_project"}
# ... 同一循环内再次出现 ...
Tool call: name=browser, args={"action":"navigate","url":"..."}
[V4] Tool BLOCKED: 'browser' not in allowed set {"delegate_complex_project", "cancel_delegated_project"}
# LLM 最终放弃，输出幻觉文本
Loop exit: no tool_calls, text_content_len=474
```

### 根因分析

1. **MiniMax API 不严格遵守 `tools` schema 约束**：即使 API 请求只携带了 2 个 tool schema，LLM 仍从训练记忆中"回忆"出 browser 工具并尝试调用。这是 MiniMax 的 API 行为问题，Nova 侧无法从 API 层面彻底解决。

2. **BLOCKED 后的错误消息不够明确**：当前错误消息（`loop.rs:483`）只说"工具不可用"，没有告诉 LLM **为什么**不可用以及**应该怎么做**。LLM 缺乏上下文，所以反复重试。

3. **system_prompt 没有工具约束提示**：当 tool_schemas 被过滤时，system_prompt 仍然是完整的（包含所有工具的描述），给 LLM 造成"我可以用 browser"的错觉。

### 解决方案

在 `loop.rs` 构建 API 请求时，如果处于工具限制模式，**往 system_prompt 末尾追加硬约束指令**。这比仅依赖 tool_schemas 过滤多了一层保障。

#### 改动

```rust
// nova-core/src/agent/loop.rs — 在 L241-251 的 API 请求构建前

// 当工具被限制时，注入系统级约束到 system prompt
let effective_system = if allowed_tool_names.is_some() {
    format!(
        "{}\n\n## ⚠️ 当前模式限制\n\
        你当前只能使用以下工具：delegate_complex_project、cancel_delegated_project。\n\
        其他所有工具（browser、bash、read_file 等）均不可用，调用会被系统直接拦截。\n\
        请直接调用 delegate_complex_project 委托任务给后台团队。\n\
        如果任务不需要委托，请直接用自然语言回答。",
        system_prompt
    )
} else {
    system_prompt.to_string()
};

let req = ApiRequest {
    model: self.config.model.clone(),
    max_tokens: self.config.max_tokens,
    system: effective_system,  // 替换 system_prompt.to_string()
    messages: api_messages,
    tools: tool_schemas.clone(),
    stream: true,
};
```

### 修改文件清单

| 文件 | 改动 |
|------|------|
| `nova-core/src/agent/loop.rs` | L241-251 区域：新增 `effective_system` 构建逻辑，替换 API 请求中的 `system` 字段 |

---

## 问题 2：Medium 复杂度被过度委派到 Coordinator 4 阶段流水线

### 现象

用户追问"我记得 Google 也有？"这种 1 轮 web search 就能解决的问题，被 Preflight 判定为 `complexity=Medium` 后，和 `complexity=High` 走完全相同的路径：
- 工具限制为 delegate_complex_project only
- 调用 delegate_complex_project → 启动 Coordinator 4 阶段流水线（Research → Synthesis → Implementation → Verification）
- 4 个阶段 = 至少 4 次 API 调用 + 可能的工具调用，严重过度消耗

### 根因分析

`loop.rs:184` 将 Medium 和 High **一视同仁**：

```rust
if complexity == Complexity::High || complexity == Complexity::Medium {
    // 两者走完全相同的限制路径
    let allowed: HashSet<String> = ["delegate_complex_project", "cancel_delegated_project"]...
}
```

Medium 任务（"搜一下 X"、"解释一段代码"）不需要 4 阶段流水线，但用户明确要求仍需通过 **SubAgent 后台执行**（不在主循环中直接执行），只是不需要 Coordinator 的重量级编排。

### 解决方案

新建 `delegate_task` 工具，专门处理 Medium 复杂度任务。它直接启动一个 **单次 Full SubAgent**（带完整工具集），不走 Coordinator 4 阶段。

#### 复杂度 → 执行方式映射

| 复杂度 | 可用工具 | 执行方式 |
|--------|---------|---------|
| **High** | `delegate_complex_project` + `cancel_delegated_project` | Coordinator 4 阶段流水线 |
| **Medium** | `delegate_task` + `cancel_delegated_project` | 单次 SubAgent（Full + tools） |
| **Low** | 所有工具 | 主循环直接执行 |

#### Step 1：新建 `delegate_task.rs`

```rust
// nova-core/src/tools/delegate_task.rs

//! delegate_task tool — 轻量级后台任务委托。
//!
//! 与 delegate_complex_project（4 阶段 Coordinator）不同，
//! 此工具直接启动单个 Full SubAgent 执行任务，适合 Medium 复杂度任务。

use std::sync::Arc;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::{info, error};

use crate::models::{ShadowEvent, ShadowEventEmitter};
use crate::subagent::{SubagentConfig, SubagentSpawner, SubagentType};
use crate::tools::registry::ToolRegistry;
use super::registry::Tool;
use super::delegate_complex_project::RUNNING_PROJECTS;

pub struct DelegateTaskTool {
    emitter: Arc<dyn ShadowEventEmitter>,
    shadow_tx: tokio::sync::mpsc::Sender<ShadowEvent>,
    api_key: String,
    api_base_url: String,
    model: String,
    system_prompt: String,
    tools: Option<Arc<ToolRegistry>>,
}

impl DelegateTaskTool {
    pub fn new(
        emitter: Arc<dyn ShadowEventEmitter>,
        shadow_tx: tokio::sync::mpsc::Sender<ShadowEvent>,
        api_key: String,
        api_base_url: String,
        model: String,
    ) -> Self {
        Self {
            emitter,
            shadow_tx,
            api_key,
            api_base_url,
            model,
            system_prompt: String::new(),
            tools: None,
        }
    }

    pub fn with_tools(mut self, tools: Arc<ToolRegistry>) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn with_system_prompt(mut self, prompt: String) -> Self {
        self.system_prompt = prompt;
        self
    }
}

#[async_trait]
impl Tool for DelegateTaskTool {
    fn name(&self) -> &str {
        "delegate_task"
    }

    fn description(&self) -> &str {
        "将一个中等复杂度的任务委托给后台助手执行。适用于需要 1-2 轮工具调用的任务（如网页搜索、单文件修改、代码解释等）。后台助手拥有完整的工具访问权限（browser、bash、文件操作等），执行完成后会自动通知你结果。"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "要执行的任务描述，越具体越好"
                },
                "context": {
                    "type": "string",
                    "description": "可选的背景上下文信息"
                }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let task_desc = input.get("task")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let context = input.get("context")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let full_task = if context.is_empty() {
            task_desc.to_string()
        } else {
            format!("{}\n\nContext: {}", task_desc, context)
        };

        let emitter = self.emitter.clone();
        let shadow_tx = self.shadow_tx.clone();
        let api_key = self.api_key.clone();
        let api_base_url = self.api_base_url.clone();
        let model = self.model.clone();
        let system_prompt = self.system_prompt.clone();
        let tools = self.tools.clone();
        let task_desc_clone = task_desc.to_string();

        let project_id = uuid::Uuid::new_v4().to_string();
        let project_id_clone = project_id.clone();

        let join_handle = tokio::spawn(async move {
            let config = SubagentConfig {
                name: format!("task-{}", &project_id_clone[..8]),
                agent_type: SubagentType::Full,
                team_name: None,
                system_prompt,
                api_key,
                api_base_url,
                model,
                max_input_tokens: 32_000,
                shadow_tx: Some(shadow_tx),
                tools,
            };

            let handle = SubagentSpawner::spawn(config, full_task);
            let result = handle.join.await;

            // Clean up registry
            RUNNING_PROJECTS.lock().await.remove(&project_id_clone);

            match result {
                Ok(Ok(output)) => {
                    info!("DelegateTask completed: {} chars", output.len());
                    emitter.emit(ShadowEvent::ProjectCompleted {
                        project_id: project_id_clone.clone(),
                        report: format!(
                            "任务完成：{}\n\n执行结果：\n{}",
                            task_desc_clone, output
                        ),
                        channel_id: "coordinator".to_string(),
                    });
                }
                Ok(Err(e)) => {
                    error!("DelegateTask failed: {}", e);
                    emitter.emit(ShadowEvent::ProjectCompleted {
                        project_id: project_id_clone.clone(),
                        report: format!(
                            "任务失败：{}\n\n错误：{}",
                            task_desc_clone, e
                        ),
                        channel_id: "coordinator".to_string(),
                    });
                }
                Err(e) => {
                    error!("DelegateTask join error: {}", e);
                    emitter.emit(ShadowEvent::ProjectCompleted {
                        project_id: project_id_clone.clone(),
                        report: format!(
                            "任务异常终止：{}\n\n错误：{}",
                            task_desc_clone, e
                        ),
                        channel_id: "coordinator".to_string(),
                    });
                }
            }
        });

        RUNNING_PROJECTS.lock().await.insert(project_id.clone(), join_handle.abort_handle());

        Ok(format!(
            "✅ 任务已委托：{}\n\n后台助手正在处理中...\n\n任务 ID: {}\n\n(如果需要停止该任务，请使用 cancel_delegated_project 工具并提供此任务 ID)\n\n=== 🔴 系统指令 🔴 ===\n请立刻直接用自然语言回复用户，告知用户已经将任务派发给后台助手，请用户耐心等待，完成后会自动通知他们。不要再调用任何工具，必须直接回复！",
            task_desc,
            project_id,
        ))
    }
}
```

#### Step 2：修改 `tools/mod.rs`

```diff
 pub mod delegate_complex_project;
+pub mod delegate_task;

 pub use delegate_complex_project::{DelegateComplexProjectTool, CancelDelegatedProjectTool};
+pub use delegate_task::DelegateTaskTool;
```

#### Step 3：修改 `loop.rs` 工具过滤逻辑

```rust
// nova-core/src/agent/loop.rs L184-192 — 区分 Medium 和 High

let (tool_schemas, allowed_tool_names): (Vec<_>, Option<HashSet<String>>) = match complexity {
    Complexity::High => {
        info!("[V4] High complexity — restricting tools to delegate_complex_project");
        let allowed: HashSet<String> = ["delegate_complex_project", "cancel_delegated_project"]
            .iter().map(|s| s.to_string()).collect();
        let schemas = self.tools.as_api_schemas_filtered(|name| allowed.contains(name));
        (schemas, Some(allowed))
    }
    Complexity::Medium => {
        info!("[V4] Medium complexity — restricting tools to delegate_task");
        let allowed: HashSet<String> = ["delegate_task", "cancel_delegated_project"]
            .iter().map(|s| s.to_string()).collect();
        let schemas = self.tools.as_api_schemas_filtered(|name| allowed.contains(name));
        (schemas, Some(allowed))
    }
    Complexity::Low => {
        (self.tools.as_api_schemas(), None)
    }
};
```

同时，修复1 的 system prompt 约束注入需要感知具体限制的工具名：

```rust
// loop.rs — 在 API 请求构建前
let effective_system = match complexity {
    Complexity::High => {
        format!(
            "{}\n\n## ⚠️ 当前模式限制\n\
            你当前只能使用 delegate_complex_project 和 cancel_delegated_project 工具。\n\
            其他所有工具均不可用，调用会被系统直接拦截。\n\
            请直接调用 delegate_complex_project 委托任务。",
            system_prompt
        )
    }
    Complexity::Medium => {
        format!(
            "{}\n\n## ⚠️ 当前模式限制\n\
            你当前只能使用 delegate_task 和 cancel_delegated_project 工具。\n\
            其他所有工具均不可用，调用会被系统直接拦截。\n\
            请直接调用 delegate_task 委托任务。",
            system_prompt
        )
    }
    Complexity::Low => system_prompt.to_string(),
};
```

#### Step 4：在 `make_tools()` 中注册 `DelegateTaskTool`

```rust
// nova-daemon/src/main.rs — make_tools() 函数内，在 delegate_complex_project 注册之后

let delegate_task_tool = nova_core::tools::DelegateTaskTool::new(
    dispatcher_tx.clone(),
    shadow_tx.clone(),
    api_key.clone(),
    api_base_url.clone(),
    model.clone(),
);
let delegate_task_tool = if let Some(ref st) = subagent_tools {
    delegate_task_tool.with_tools(st.clone())
} else {
    delegate_task_tool
};
tools.register_builtin(Box::new(delegate_task_tool));
```

### 修改文件清单

| 文件 | 改动 |
|------|------|
| `nova-core/src/tools/delegate_task.rs` | **新建**：`DelegateTaskTool` — 单 SubAgent 后台委托工具 |
| `nova-core/src/tools/mod.rs` | 新增 `pub mod delegate_task;` 和 `pub use delegate_task::DelegateTaskTool;` |
| `nova-core/src/agent/loop.rs` | L184-192：区分 Medium/High 工具限制；L241-251 前：注入 `effective_system` |
| `nova-daemon/src/main.rs` | `make_tools()` 内注册 `DelegateTaskTool` |

### 风险点

1. **Preflight 分类准确性**：如果 Preflight 把 Low 错误分类为 Medium，本该主循环直接回答的问题会被不必要地委派。
   - **缓解**：Preflight 的 few-shot 示例已有覆盖；观察日志，必要时调整分类标准。

2. **SubAgent 超时**：单 SubAgent 没有内置超时保护。
   - **缓解**：`run_with_tools_loop()` 已有 `MAX_LOOPS = 20` 安全上限；API 调用自带 60s 超时。

3. **通知路径复用**：`delegate_task` 复用了 `channel_id: "coordinator"` 和 `RUNNING_PROJECTS` 全局注册表，行为与 `delegate_complex_project` 一致。FIXBUG-001 的通知修复同样适用。

---

## 实施顺序建议

1. **先创建 `delegate_task.rs`** — 独立新文件，不影响现有代码
2. **修改 `mod.rs`** — 注册新模块
3. **修改 `loop.rs`** — 区分 Medium/High + 注入 `effective_system`
4. **修改 `main.rs`** — 注册新工具

## 验证方法

1. 重启 daemon
2. 在 TUI 中输入一个 Medium 任务（如"帮我搜一下 Google code wiki"）
3. 观察 `daemon.log`：
   - Preflight 应输出 `complexity=Medium`
   - 应看到 `[V4] Medium complexity — restricting tools to delegate_task`
   - LLM 应在 **1 轮**内调用 `delegate_task`（不再反复碰壁）
   - SubAgent 应启动并执行 browser 工具
4. 在 TUI 中输入一个 High 任务（如"帮我把整个鉴权模块从 JWT 换成 Session"）
5. 确认走的是 `delegate_complex_project` → Coordinator 4 阶段
