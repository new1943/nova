//! delegate_complex_project tool — for complex multi-step project delegation.
//!
//! Spawns the Coordinator 4-phase pipeline (Research → Synthesis → Implementation → Verification).

use std::sync::Arc;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::error;

use nova_core::models::{ShadowEvent, ShadowEventEmitter};
use nova_tools::delegate_base::RUNNING_PROJECTS;
use nova_tools::registry::{ToolHandler, ToolContext, ToolRegistry};
use nova_tools::task::TaskLogger;
use crate::coordinator::orchestrator::Coordinator;

/// Tool for delegating complex projects to the coordinator.
pub struct DelegateComplexProjectTool {
    emitter: Arc<dyn ShadowEventEmitter>,
    shadow_tx: tokio::sync::mpsc::Sender<ShadowEvent>,
    api_key: String,
    api_base_url: String,
    model: String,
    system_prompt: String,
    tools: Option<Arc<ToolRegistry>>,
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
impl ToolHandler for DelegateComplexProjectTool {
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

    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<String> {
        let project_goal = input.get("project_goal")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let initial_context = input.get("initial_context")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let task = format!("{}\n\nContext: {}", project_goal, initial_context);

        let emitter = self.emitter.clone();
        let shadow_tx = self.shadow_tx.clone();
        let api_key = self.api_key.clone();
        let api_base_url = self.api_base_url.clone();
        let model = self.model.clone();
        let system_prompt = self.system_prompt.clone();
        let tools = self.tools.clone();
        let project_goal_clone = project_goal.to_string();
        let workspace_dir = ctx.workspace_dir.clone();

        let channel_id = ctx.channel_id.clone();
        let channel_id_clone = channel_id.clone();

        let project_id = uuid::Uuid::new_v4().to_string();
        let project_id_clone = project_id.clone();

        // Log task start
        if let Some(ref ws) = workspace_dir {
            let _ = TaskLogger::append_task(ws, &project_id, project_goal).await;
        }

        // Spawn Coordinator 4-phase pipeline in background
        let join_handle = tokio::spawn(async move {
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

            RUNNING_PROJECTS.lock().await.remove(&project_id_clone);

            // Update task status
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

        RUNNING_PROJECTS.lock().await.insert(project_id.clone(), join_handle.abort_handle());

        Ok(format!(
            "✅ 项目已委托：{}\n\n后台架构团队正在处理中...\n\n任务 ID: {}\n\n(如果需要停止该任务，请使用 cancel_delegated_project 工具并提供此任务 ID)\n\n初始上下文：{}\n\n=== 🔴 系统指令 🔴 ===\n请立刻直接用自然语言回复用户，告知用户已经将任务派发给后台架构团队，请用户耐心等待，完成后会自动通知他们。不要再调用任何工具，必须直接回复！",
            project_goal,
            project_id,
            initial_context
        ))
    }
}

#[derive(Default)]
pub struct CancelDelegatedProjectTool;

impl CancelDelegatedProjectTool {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl ToolHandler for CancelDelegatedProjectTool {
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

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> Result<String> {
        let project_id = input.get("project_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
            
        if project_id.is_empty() {
            return Ok("错误: 未提供 project_id".to_string());
        }

        let cancelled = nova_tools::delegate_base::cancel_delegated_task(project_id).await;
        if cancelled {
            Ok(format!("✅ 成功终止了任务 ID 为 {} 的后台项目。\n\n=== 🔴 系统指令 🔴 ===\n请立刻回复用户，明确告知后台任务已经成功终止！", project_id))
        } else {
            Ok(format!("⚠️ 未找到任务 ID 为 {} 的后台项目。它可能已经完成，或者 ID 不正确。\n\n=== 🔴 系统指令 🔴 ===\n请向用户说明未能找到该任务，可能已结束或不存在。", project_id))
        }
    }
}
