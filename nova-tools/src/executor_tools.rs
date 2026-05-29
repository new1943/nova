use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use nova_core::executor::registry::TaskRegistry;
use nova_core::executor::types::{ChannelContext, ExecMode, Notification, TaskRequest};
use nova_core::executor::ToolExecContext;
use nova_core::llm_backend::LlmBackend;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::registry::{ToolContext, ToolHandler};

/// Convert nova_llm ToolSchema to nova_core ToolSchema (identical fields, different types)
fn convert_schemas(schemas: &[nova_llm::types::ToolSchema]) -> Vec<nova_core::llm_backend::ToolSchema> {
    schemas.iter().map(|s| nova_core::llm_backend::ToolSchema {
        name: s.name.clone(),
        description: s.description.clone(),
        input_schema: s.input_schema.clone(),
    }).collect()
}

fn next_task_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    format!("task-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

fn parse_string(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn parse_string_vec(v: &Value, key: &str) -> Option<Vec<String>> {
    v.get(key).and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|item| item.as_str().map(|s| s.to_string()))
            .collect()
    })
}

fn parse_usize(v: &Value, key: &str) -> Option<usize> {
    v.get(key).and_then(|v| v.as_u64()).map(|n| n as usize)
}

fn build_channel_ctx(ctx: &ToolContext) -> ChannelContext {
    ChannelContext {
        platform: nova_core::platform::Platform::Tui,
        channel_id: ctx.channel_id.clone(),
        reply_to: None,
    }
}

fn build_tool_exec_ctx(ctx: &ToolContext) -> ToolExecContext {
    ToolExecContext::new(ctx.channel_id.clone(), ctx.workspace_dir.clone())
}

/// Shared executor state — held by all 5 tools via Arc.
pub struct ExecutorState {
    pub llm: Arc<dyn LlmBackend>,
    pub tool_executor: Arc<dyn nova_core::executor::ToolExecutor>,
    pub tool_schemas: Vec<nova_core::llm_backend::ToolSchema>,
    pub notify_tx: mpsc::Sender<Notification>,
}

impl ExecutorState {
    pub fn new(
        llm: Arc<dyn LlmBackend>,
        tool_executor: Arc<dyn nova_core::executor::ToolExecutor>,
        tool_schemas: Vec<nova_llm::types::ToolSchema>,
        notify_tx: mpsc::Sender<Notification>,
    ) -> Self {
        Self { llm, tool_executor, tool_schemas: convert_schemas(&tool_schemas), notify_tx }
    }
}

fn spawn_task(req: TaskRequest, state: &Arc<ExecutorState>, mode: &str) -> String {
    let task_id = req.id.clone();
    let task_id_ret = task_id.clone();
    let task_name = req.name.clone();
    let mode_owned = mode.to_string();
    let timeout = req.timeout;
    let channel = req.channel.clone();
    let state = Arc::clone(state);

    tracing::info!("[Executor] Spawning task {} ({}) mode={}", task_id_ret, task_name, mode);

    tokio::spawn(async move {
        let registry = TaskRegistry::global();
        let handle = tokio::spawn({
            let req = req;
            let state = state.clone();
            let mode = mode_owned.clone();
            async move {
                tracing::info!("[Executor] Task {} ({}) started execution", req.id, req.name);
                let tool_ctx = build_tool_exec_ctx(&ToolContext::new(
                    req.channel.channel_id.clone(),
                    None,
                ));
                match mode.as_str() {
                    "react" => {
                        nova_core::executor::react::execute_react(
                            &req, &state.llm, &state.tool_schemas,
                            &state.notify_tx, &state.tool_executor, &tool_ctx,
                        ).await;
                    }
                    "chain" => {
                        nova_core::executor::chain::execute_chain(
                            &req, &state.llm, &state.tool_schemas,
                            &state.notify_tx, &state.tool_executor, &tool_ctx,
                        ).await;
                    }
                    "parallel" => {
                        nova_core::executor::parallel::execute_parallel(
                            &req, &state.llm, &state.tool_schemas,
                            &state.notify_tx, &state.tool_executor, &tool_ctx,
                        ).await;
                    }
                    "with_review" => {
                        nova_core::executor::review::execute_with_review(
                            &req, &state.llm, &state.tool_schemas,
                            &state.notify_tx, &state.tool_executor, &tool_ctx,
                        ).await;
                    }
                    "project" => {
                        nova_core::executor::project::execute_project(
                            &req, &state.llm, &state.tool_schemas,
                            &state.notify_tx, &state.tool_executor, &tool_ctx,
                        ).await;
                    }
                    _ => {}
                }
                tracing::info!("[Executor] Task {} execution completed, deregistering", req.id);
                registry.deregister(&req.id).await;
            }
        });

        // Wrap with timeout + panic recovery to ensure notification is ALWAYS sent
        let notify_tx = state.notify_tx.clone();
        let tid = task_id.clone();
        let tname = task_name.clone();
        let mode_str = mode_owned.clone();
        let ch = channel.clone();
        let wrapped = tokio::spawn(async move {
            let result = tokio::time::timeout(timeout, handle).await;
            match result {
                Ok(Ok(())) => { /* normal completion, notification already sent by executor */ }
                Ok(Err(join_err)) => {
                    tracing::error!("[Executor] Task {} panicked: {}", tid, join_err);
                    let _ = notify_tx.send(nova_core::executor::types::Notification {
                        id: tid.clone(),
                        name: tname.clone(),
                        tool: format!("execute_{}", mode_str),
                        status: nova_core::executor::types::TaskStatus::Failed {
                            error: format!("Task panicked: {}", join_err),
                            duration: std::time::Duration::from_secs(0),
                        },
                        output: None,
                        channel: ch.clone(),
                        phases: None,
                    }).await;
                }
                Err(_elapsed) => {
                    tracing::warn!("[Executor] Task {} timed out after {:?}", tid, timeout);
                    let _ = notify_tx.send(nova_core::executor::types::Notification {
                        id: tid.clone(),
                        name: tname.clone(),
                        tool: format!("execute_{}", mode_str),
                        status: nova_core::executor::types::TaskStatus::TimedOut {
                            duration: timeout,
                        },
                        output: None,
                        channel: ch.clone(),
                        phases: None,
                    }).await;
                }
            }
            TaskRegistry::global().deregister(&tid).await;
        });

        registry.register(task_id, task_name, mode_owned, wrapped).await;
    });

    task_id_ret
}

// ── execute_react ──────────────────────────────────────────────

pub struct ExecuteReactTool {
    state: Arc<ExecutorState>,
}

impl ExecuteReactTool {
    pub fn new(state: Arc<ExecutorState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl ToolHandler for ExecuteReactTool {
    fn name(&self) -> &str { "execute_react" }

    fn description(&self) -> &str {
        "Execute a task using ReAct (Reason+Act) loop. Best for debugging, research, \
         and exploratory tasks where each step's result determines the next. \
         The LLM decides what tool to call at each step based on observations."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task": { "type": "string", "description": "Task description (full prompt for the sub-agent)" },
                "name": { "type": "string", "description": "Short task name for display" },
                "domain_prompt": { "type": "string", "description": "Domain expert system prompt (optional)" },
                "max_turns": { "type": "integer", "description": "Max tool-call turns (default: 100)" }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let task = parse_string(&args, "task")
            .ok_or_else(|| anyhow::anyhow!("Missing required field: task"))?;
        let name = parse_string(&args, "name").unwrap_or_else(|| "ReAct任务".into());
        let domain_prompt = parse_string(&args, "domain_prompt").unwrap_or_default();
        let max_turns = parse_usize(&args, "max_turns").unwrap_or(0);

        let req = TaskRequest {
            id: next_task_id(),
            name,
            mode: ExecMode::React,
            task_prompt: task,
            domain_prompt,
            steps: None,
            review_prompt: None,
            research_prompt: None,
            implementation_prompt: None,
            max_turns,
            timeout: std::time::Duration::from_secs(600),
            tools: self.state.tool_schemas.iter().map(|s| s.name.clone()).collect(),
            channel: build_channel_ctx(ctx),
        };

        let task_id = spawn_task(req, &self.state, "react");
        Ok(format!("任务已派发，ID: {}，模式: react", task_id))
    }
}

// ── execute_chain ──────────────────────────────────────────────

pub struct ExecuteChainTool {
    state: Arc<ExecutorState>,
}

impl ExecuteChainTool {
    pub fn new(state: Arc<ExecutorState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl ToolHandler for ExecuteChainTool {
    fn name(&self) -> &str { "execute_chain" }

    fn description(&self) -> &str {
        "Execute a task as a fixed-step chain, running each step sequentially. \
         Best for pipeline tasks with predefined steps. No branching."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task": { "type": "string", "description": "Task description" },
                "name": { "type": "string", "description": "Short task name for display" },
                "steps": { "type": "array", "items": { "type": "string" }, "description": "Ordered list of steps to execute" },
                "domain_prompt": { "type": "string", "description": "Domain expert system prompt (optional)" },
                "max_turns": { "type": "integer", "description": "Max tool-call turns (default: 50)" }
            },
            "required": ["task", "steps"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let task = parse_string(&args, "task")
            .ok_or_else(|| anyhow::anyhow!("Missing required field: task"))?;
        let steps = parse_string_vec(&args, "steps")
            .ok_or_else(|| anyhow::anyhow!("Missing required field: steps"))?;
        let name = parse_string(&args, "name").unwrap_or_else(|| "Chain任务".into());
        let domain_prompt = parse_string(&args, "domain_prompt").unwrap_or_default();
        let max_turns = parse_usize(&args, "max_turns").unwrap_or(0);

        let req = TaskRequest {
            id: next_task_id(),
            name,
            mode: ExecMode::Chain,
            task_prompt: task,
            domain_prompt,
            steps: Some(steps),
            review_prompt: None,
            research_prompt: None,
            implementation_prompt: None,
            max_turns,
            timeout: std::time::Duration::from_secs(600),
            tools: self.state.tool_schemas.iter().map(|s| s.name.clone()).collect(),
            channel: build_channel_ctx(ctx),
        };

        let task_id = spawn_task(req, &self.state, "chain");
        Ok(format!("任务已派发，ID: {}，模式: chain", task_id))
    }
}

// ── execute_parallel ───────────────────────────────────────────

pub struct ExecuteParallelTool {
    state: Arc<ExecutorState>,
}

impl ExecuteParallelTool {
    pub fn new(state: Arc<ExecutorState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl ToolHandler for ExecuteParallelTool {
    fn name(&self) -> &str { "execute_parallel" }

    fn description(&self) -> &str {
        "Execute multiple independent sub-tasks in parallel. \
         Split task description by '---' delimiter to define sub-tasks. \
         Best for querying multiple data sources simultaneously."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task": { "type": "string", "description": "Task description, use '---' delimiter to split into parallel sub-tasks" },
                "name": { "type": "string", "description": "Short task name for display" },
                "domain_prompt": { "type": "string", "description": "Domain expert system prompt (optional)" },
                "max_turns": { "type": "integer", "description": "Max tool-call turns per sub-task (default: 50)" }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let task = parse_string(&args, "task")
            .ok_or_else(|| anyhow::anyhow!("Missing required field: task"))?;
        let name = parse_string(&args, "name").unwrap_or_else(|| "Parallel任务".into());
        let domain_prompt = parse_string(&args, "domain_prompt").unwrap_or_default();
        let max_turns = parse_usize(&args, "max_turns").unwrap_or(0);

        let req = TaskRequest {
            id: next_task_id(),
            name,
            mode: ExecMode::Parallel,
            task_prompt: task,
            domain_prompt,
            steps: None,
            review_prompt: None,
            research_prompt: None,
            implementation_prompt: None,
            max_turns,
            timeout: std::time::Duration::from_secs(600),
            tools: self.state.tool_schemas.iter().map(|s| s.name.clone()).collect(),
            channel: build_channel_ctx(ctx),
        };

        let task_id = spawn_task(req, &self.state, "parallel");
        Ok(format!("任务已派发，ID: {}，模式: parallel", task_id))
    }
}

// ── execute_with_review ────────────────────────────────────────

pub struct ExecuteWithReviewTool {
    state: Arc<ExecutorState>,
}

impl ExecuteWithReviewTool {
    pub fn new(state: Arc<ExecutorState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl ToolHandler for ExecuteWithReviewTool {
    fn name(&self) -> &str { "execute_with_review" }

    fn description(&self) -> &str {
        "Execute a task with adversarial self-review. Runs the task, then spawns a \
         read-only reviewer to verify the result. If verification fails, re-executes. \
         Best for tasks that need high confidence in correctness."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task": { "type": "string", "description": "Task description" },
                "name": { "type": "string", "description": "Short task name for display" },
                "domain_prompt": { "type": "string", "description": "Domain expert system prompt (optional)" },
                "review_prompt": { "type": "string", "description": "Custom verification prompt (optional, uses adversarial template if omitted)" },
                "max_turns": { "type": "integer", "description": "Max tool-call turns per sub-agent (default: 50)" }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let task = parse_string(&args, "task")
            .ok_or_else(|| anyhow::anyhow!("Missing required field: task"))?;
        let name = parse_string(&args, "name").unwrap_or_else(|| "Review任务".into());
        let domain_prompt = parse_string(&args, "domain_prompt").unwrap_or_default();
        let review_prompt = parse_string(&args, "review_prompt");
        let max_turns = parse_usize(&args, "max_turns").unwrap_or(0);

        let req = TaskRequest {
            id: next_task_id(),
            name,
            mode: ExecMode::WithReview,
            task_prompt: task,
            domain_prompt,
            steps: None,
            review_prompt,
            research_prompt: None,
            implementation_prompt: None,
            max_turns,
            timeout: std::time::Duration::from_secs(600),
            tools: self.state.tool_schemas.iter().map(|s| s.name.clone()).collect(),
            channel: build_channel_ctx(ctx),
        };

        let task_id = spawn_task(req, &self.state, "with_review");
        Ok(format!("任务已派发，ID: {}，模式: with_review", task_id))
    }
}

// ── execute_project ────────────────────────────────────────────

pub struct ExecuteProjectTool {
    state: Arc<ExecutorState>,
}

impl ExecuteProjectTool {
    pub fn new(state: Arc<ExecutorState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl ToolHandler for ExecuteProjectTool {
    fn name(&self) -> &str { "execute_project" }

    fn description(&self) -> &str {
        "Execute a complex project with 4-phase orchestration: \
         Research (parallel) → Synthesis → Implementation → Verification (adversarial). \
         If verification fails, re-implements and re-verifies up to 3 iterations. \
         Best for complex, multi-file changes requiring high confidence."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task": { "type": "string", "description": "Task description (can use '---' to split research directions)" },
                "name": { "type": "string", "description": "Short task name for display" },
                "domain_prompt": { "type": "string", "description": "Domain expert system prompt (optional)" },
                "research_prompt": { "type": "string", "description": "Custom research worker prompt (optional)" },
                "implementation_prompt": { "type": "string", "description": "Custom implementation worker prompt (optional)" },
                "review_prompt": { "type": "string", "description": "Custom verification worker prompt (optional)" },
                "max_turns": { "type": "integer", "description": "Max tool-call turns per phase (default: 50)" }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let task = parse_string(&args, "task")
            .ok_or_else(|| anyhow::anyhow!("Missing required field: task"))?;
        let name = parse_string(&args, "name").unwrap_or_else(|| "Project任务".into());
        let domain_prompt = parse_string(&args, "domain_prompt").unwrap_or_default();
        let research_prompt = parse_string(&args, "research_prompt");
        let implementation_prompt = parse_string(&args, "implementation_prompt");
        let review_prompt = parse_string(&args, "review_prompt");
        let max_turns = parse_usize(&args, "max_turns").unwrap_or(0);

        let req = TaskRequest {
            id: next_task_id(),
            name,
            mode: ExecMode::Project,
            task_prompt: task,
            domain_prompt,
            steps: None,
            review_prompt,
            research_prompt,
            implementation_prompt,
            max_turns,
            timeout: std::time::Duration::from_secs(1800),
            tools: self.state.tool_schemas.iter().map(|s| s.name.clone()).collect(),
            channel: build_channel_ctx(ctx),
        };

        let task_id = spawn_task(req, &self.state, "project");
        Ok(format!("任务已派发，ID: {}，模式: project", task_id))
    }
}

// ── list_tasks ─────────────────────────────────────────────────

pub struct TaskListTool;

#[async_trait]
impl ToolHandler for TaskListTool {
    fn name(&self) -> &str { "list_tasks" }

    fn description(&self) -> &str {
        "List all running background tasks. Shows task ID, name, execution mode, and elapsed time. \
         Use this to verify task dispatch and monitor progress."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> Result<String> {
        let registry = TaskRegistry::global();
        let running = registry.list().await;

        if running.is_empty() {
            return Ok("当前没有运行中的后台任务。".into());
        }

        let mut lines = vec![format!("运行中的后台任务 ({}):", running.len())];
        for t in &running {
            let elapsed = t.started_at.elapsed().as_secs();
            lines.push(format!(
                "  - [{}] {} ({}) — 已运行 {}s",
                t.id, t.name, t.tool, elapsed
            ));
        }
        Ok(lines.join("\n"))
    }
}

// ── stop_task ──────────────────────────────────────────────────

pub struct TaskStopTool;

#[async_trait]
impl ToolHandler for TaskStopTool {
    fn name(&self) -> &str { "stop_task" }

    fn description(&self) -> &str {
        "Stop a running background task by ID, or stop all tasks. \
         Use list_tasks first to get the task ID."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "description": "Task ID to stop (omit to stop all)" }
            },
            "required": []
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        let registry = TaskRegistry::global();

        if let Some(task_id) = args.get("task_id").and_then(|v| v.as_str()) {
            if registry.stop_by_id(task_id).await {
                Ok(format!("已停止任务: {}", task_id))
            } else {
                Err(anyhow::anyhow!("未找到任务: {}", task_id))
            }
        } else {
            let count = registry.stop_all().await;
            Ok(format!("已停止所有后台任务 ({} 个)", count))
        }
    }
}
