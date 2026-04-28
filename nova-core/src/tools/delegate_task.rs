//! delegate_task tool — for medium complexity task delegation.
//!
//! Spawns a single SubAgent directly (not the Coordinator 4-phase pipeline).
//!适合需要 1-2 轮工具调用的任务（如网页搜索、单文件修改、代码解释等）。
//! Emits ShadowEvent::ProjectCompleted when the SubAgent completes.

use std::sync::Arc;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::{info, error};

use crate::models::{ShadowEvent, ShadowEventEmitter};
use crate::subagent::{SubagentConfig, SubagentSpawner, SubagentType};
use crate::tools::registry::ToolRegistry;
use crate::task::TaskLogger;
use super::registry::Tool;

// Re-use RUNNING_PROJECTS from delegate_complex_project so cancel works for both
pub use super::delegate_complex_project::RUNNING_PROJECTS;

/// Tool for delegating medium complexity tasks to a single SubAgent.
/// Spawns a single Full-capability SubAgent directly (no Coordinator pipeline).
pub struct DelegateTaskTool {
    emitter: Arc<dyn ShadowEventEmitter>,
    /// For SubAgent to emit TaskProgress events
    shadow_tx: tokio::sync::mpsc::Sender<ShadowEvent>,
    api_key: String,
    api_base_url: String,
    model: String,
    system_prompt: String,
    /// ToolRegistry for SubAgent tool execution
    tools: Option<Arc<ToolRegistry>>,
    /// [V2 Task System] Workspace directory for tasks.md
    workspace_dir: Option<std::path::PathBuf>,
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
            workspace_dir: None,
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

    pub fn with_workspace_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.workspace_dir = Some(dir);
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
        let task = input.get("task")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let context = input.get("context")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let full_task = if context.is_empty() {
            task.to_string()
        } else {
            format!("{}\n\nContext: {}", task, context)
        };

        // Clone data for the background task
        let emitter = self.emitter.clone();
        let shadow_tx = self.shadow_tx.clone();
        let api_key = self.api_key.clone();
        let api_base_url = self.api_base_url.clone();
        let model = self.model.clone();
        let system_prompt = self.system_prompt.clone();
        let tools = self.tools.clone();
        let task_clone = task.to_string();
        let workspace_dir = self.workspace_dir.clone();

        let channel_id = crate::tools::CURRENT_CHANNEL_ID
            .try_with(|id| id.clone())
            .unwrap_or_else(|_| "coordinator".to_string());
        let channel_id_clone = channel_id.clone();

        let project_id = uuid::Uuid::new_v4().to_string();
        let project_id_clone = project_id.clone();

        // [V2 Task System] Log task start to tasks.md
        if let Some(ref ws) = workspace_dir {
            let _ = TaskLogger::append_task(ws, &project_id, task).await;
        }

        info!("[FIXBUG-002] delegate_task spawned: {}", project_id);

        // Spawn single SubAgent directly (not Coordinator)
        let join_handle = tokio::spawn(async move {
            // [V6 Fix] 动态加载真实的 system_prompt，防止子 Agent 因为缺少日期信息而产生幻觉
            let actual_system_prompt = if let Some(ref ws) = workspace_dir {
                let mut loader = crate::workspace::BootstrapLoader::new(ws.clone());
                let tool_desc = tools.as_ref().map(|t| t.describe_all()).unwrap_or_default();
                loader.build_system_prompt(&tool_desc)
            } else {
                system_prompt
            };

            let config = SubagentConfig {
                name: format!("task-{}", &project_id_clone[..8]),
                agent_type: SubagentType::Full,
                team_name: None,
                system_prompt: actual_system_prompt,
                api_key,
                api_base_url,
                model,
                max_input_tokens: 32_000,
                shadow_tx: Some(shadow_tx),
                tools,
            };

            let handle = SubagentSpawner::spawn(config, full_task);
            let result = handle.join.await;

            // Clean up registry upon completion
            RUNNING_PROJECTS.lock().await.remove(&project_id_clone);

            // [V2 Task System] Update task status in tasks.md
            if let Some(ref ws) = workspace_dir {
                match &result {
                    Ok(Ok(_)) => {
                        let _ = TaskLogger::update_status_v2(ws, &project_id_clone, "Completed", None).await;
                    }
                    Ok(Err(e)) => {
                        let _ = TaskLogger::update_status_v2(ws, &project_id_clone, "Failed", Some(&e.to_string())).await;
                    }
                    Err(e) => {
                        let _ = TaskLogger::update_status_v2(ws, &project_id_clone, "Failed", Some(&e.to_string())).await;
                    }
                }
            }

            match result {
                Ok(Ok(output)) => {
                    info!("[FIXBUG-002] delegate_task completed: {}", project_id_clone);
                    emitter.emit(ShadowEvent::ProjectCompleted {
                        project_id: project_id_clone.clone(),
                        report: format!(
                            "任务完成：{}\n\n执行结果：\n{}",
                            task_clone,
                            output
                        ),
                        channel_id: channel_id_clone.clone(),
                    });
                }
                Ok(Err(e)) => {
                    error!("[FIXBUG-002] delegate_task failed: {}", e);
                    emitter.emit(ShadowEvent::ProjectCompleted {
                        project_id: project_id_clone.clone(),
                        report: format!(
                            "任务失败：{}\n\n错误：{}",
                            task_clone,
                            e
                        ),
                        channel_id: channel_id_clone.clone(),
                    });
                }
                Err(e) => {
                    error!("[FIXBUG-002] delegate_task join error: {}", e);
                    emitter.emit(ShadowEvent::ProjectCompleted {
                        project_id: project_id_clone.clone(),
                        report: format!(
                            "任务异常终止：{}\n\n错误：{}",
                            task_clone,
                            e
                        ),
                        channel_id: channel_id_clone.clone(),
                    });
                }
            }
        });

        // Store abort handle for cancellation
        RUNNING_PROJECTS.lock().await.insert(project_id.clone(), join_handle.abort_handle());

        Ok(format!(
            "✅ 任务已委托：{}\n\n后台助手正在处理中...\n\n任务 ID: {}\n\n(如果需要停止该任务，请使用 cancel_delegated_project 工具并提供此任务 ID)\n\n=== 🔴 系统指令 🔴 ===\n请立刻直接用自然语言回复用户，告知用户已经将任务派发给后台助手，请用户耐心等待，完成后会自动通知他们。不要再调用任何工具，必须直接回复！",
            task,
            project_id,
        ))
    }
}
