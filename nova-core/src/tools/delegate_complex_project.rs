//! delegate_complex_project tool — for complex multi-step project delegation.
//!
//! Spawns the Coordinator 4-phase pipeline (Research → Synthesis → Implementation → Verification).
//! Emits ShadowEvent::ProjectCompleted when the coordinator completes.

use std::sync::Arc;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::error;
use std::collections::HashMap;
use once_cell::sync::Lazy;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::AbortHandle;

use crate::coordinator::orchestrator::Coordinator;
use crate::models::{ShadowEvent, ShadowEventEmitter};
use crate::tools::registry::ToolRegistry;
use crate::task::TaskLogger;
use super::registry::Tool;

// Global registry of running background projects
pub static RUNNING_PROJECTS: Lazy<AsyncMutex<HashMap<String, AbortHandle>>> = Lazy::new(|| {
    AsyncMutex::new(HashMap::new())
});

/// Tool for delegating complex projects to the coordinator.
/// Spawns the Coordinator 4-phase pipeline in the background.
pub struct DelegateComplexProjectTool {
    emitter: Arc<dyn ShadowEventEmitter>,
    /// [V4 Fix] For Coordinator's SubAgents to emit TaskProgress events
    shadow_tx: tokio::sync::mpsc::Sender<ShadowEvent>,
    api_key: String,
    api_base_url: String,
    model: String,
    system_prompt: String,
    /// [V4 Fix] ToolRegistry for Coordinator's SubAgent tool execution
    tools: Option<Arc<ToolRegistry>>,
    /// [V2 Task System] Workspace directory for tasks.md
    workspace_dir: Option<std::path::PathBuf>,
}

impl DelegateComplexProjectTool {
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
            workspace_dir: None,
        }
    }

    pub fn with_workspace_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
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
impl Tool for DelegateComplexProjectTool {
    fn name(&self) -> &str {
        "delegate_complex_project"
    }

    fn description(&self) -> &str {
        "当你面对一个多步骤、跨文件的复杂开发任务或深度网页检索任务时，必须调用此工具将任务委托给后台架构团队。调用后你只需安抚用户即可。参数：project_goal（一句话总结最终目标），initial_context（目前已知的文件路径、背景要求或初始线索）。"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "project_goal": {
                    "type": "string",
                    "description": "用一句话总结最终想要达成的目标"
                },
                "initial_context": {
                    "type": "string",
                    "description": "目前已知的文件路径、背景要求或初始线索"
                }
            },
            "required": ["project_goal"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let project_goal = input.get("project_goal")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let initial_context = input.get("initial_context")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let task = format!("{}\n\nContext: {}", project_goal, initial_context);

        // Clone data for the background task
        let emitter = self.emitter.clone();
        let shadow_tx = self.shadow_tx.clone();
        let api_key = self.api_key.clone();
        let api_base_url = self.api_base_url.clone();
        let model = self.model.clone();
        let system_prompt = self.system_prompt.clone();
        let tools = self.tools.clone();
        let project_goal_clone = project_goal.to_string();
        let workspace_dir = self.workspace_dir.clone();

        let channel_id = crate::tools::CURRENT_CHANNEL_ID
            .try_with(|id| id.clone())
            .unwrap_or_else(|_| "coordinator".to_string());
        let channel_id_clone = channel_id.clone();

        let project_id = uuid::Uuid::new_v4().to_string();
        let project_id_clone = project_id.clone();

        // [V2 Task System] Log task start to tasks.md
        if let Some(ref ws) = workspace_dir {
            let _ = TaskLogger::append_task(ws, &project_id, project_goal).await;
        }

        // [V4 Fix] Spawn Coordinator 4-phase pipeline in background
        let join_handle = tokio::spawn(async move {
            // [V6 Fix] 动态加载真实的 system_prompt，防止子 Agent (特别是 Verification) 因为缺少日期信息而产生幻觉
            let actual_system_prompt = if let Some(ref ws) = workspace_dir {
                let mut loader = crate::workspace::BootstrapLoader::new(ws.clone());
                let tool_desc = tools.as_ref().map(|t| t.describe_all()).unwrap_or_default();
                loader.build_system_prompt(&tool_desc)
            } else {
                system_prompt
            };

            let coordinator = Coordinator::new(
                api_key,
                api_base_url,
                model,
                actual_system_prompt,
                Some(shadow_tx),
                tools,
            );

            let res = coordinator.orchestrate(&task).await;

            // Clean up registry upon completion
            RUNNING_PROJECTS.lock().await.remove(&project_id_clone);

            // [V2 Task System] Update task status in tasks.md
            if let Some(ref ws) = workspace_dir {
                match &res {
                    Ok(_) => {
                        let _ = TaskLogger::update_status_v2(ws, &project_id_clone, "Completed", None).await;
                    }
                    Err(e) => {
                        let _ = TaskLogger::update_status_v2(ws, &project_id_clone, "Failed", Some(&e.to_string())).await;
                    }
                }
            }

            match res {
                Ok(result) => {
                    // Coordinator completed successfully — emit ProjectCompleted
                    emitter.emit(ShadowEvent::ProjectCompleted {
                        project_id: project_id_clone.clone(),
                        report: format!(
                            "项目完成：{}\n\n执行结果：\n{}",
                            project_goal_clone,
                            result.output
                        ),
                        channel_id: channel_id_clone.clone(),
                    });
                }
                Err(e) => {
                    error!("Coordinator failed: {}", e);
                    // Emit error as ProjectCompleted for visibility
                    emitter.emit(ShadowEvent::ProjectCompleted {
                        project_id: project_id_clone.clone(),
                        report: format!(
                            "项目失败：{}\n\n错误：{}",
                            project_goal_clone,
                            e
                        ),
                        channel_id: channel_id_clone.clone(),
                    });
                }
            }
        });

        // Store abort handle
        RUNNING_PROJECTS.lock().await.insert(project_id.clone(), join_handle.abort_handle());

        Ok(format!(
            "✅ 项目已委托：{}\n\n后台架构团队正在处理中...\n\n任务 ID: {}\n\n(如果需要停止该任务，请使用 cancel_delegated_project 工具并提供此任务 ID)\n\n初始上下文：{}\n\n=== 🔴 系统指令 🔴 ===\n请立刻直接用自然语言回复用户，告知用户已经将任务派发给后台架构团队，请用户耐心等待，完成后会自动通知他们。不要再调用任何工具，必须直接回复！",
            project_goal,
            project_id,
            initial_context
        ))
    }
}

pub struct CancelDelegatedProjectTool;

impl CancelDelegatedProjectTool {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Tool for CancelDelegatedProjectTool {
    fn name(&self) -> &str {
        "cancel_delegated_project"
    }

    fn description(&self) -> &str {
        "取消或停止一个正在后台执行的复杂委托项目。需要提供任务的 ID。"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "project_id": {
                    "type": "string",
                    "description": "要取消的任务 ID (由 delegate_complex_project 给出)"
                }
            },
            "required": ["project_id"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let project_id = input.get("project_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
            
        if project_id.is_empty() {
            return Ok("错误: 未提供 project_id".to_string());
        }

        let mut registry = RUNNING_PROJECTS.lock().await;
        if let Some(handle) = registry.remove(project_id) {
            handle.abort();
            Ok(format!("✅ 成功终止了任务 ID 为 {} 的后台项目。\n\n=== 🔴 系统指令 🔴 ===\n请立刻回复用户，明确告知后台任务已经成功终止！", project_id))
        } else {
            Ok(format!("⚠️ 未找到任务 ID 为 {} 的后台项目。它可能已经完成，或者 ID 不正确。\n\n=== 🔴 系统指令 🔴 ===\n请向用户说明未能找到该任务，可能已结束或不存在。", project_id))
        }
    }
}
