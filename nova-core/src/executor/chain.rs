use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;

use crate::llm_backend::{CompletionRequest, LlmBackend, ToolSchema};
use crate::message::Message;

use super::types::{Notification, TaskId, TaskRequest, TaskStatus};
use super::ToolExecutor;
use super::util::{extract_response, to_completion_messages};

/// execute_chain 的默认最大轮数
const DEFAULT_MAX_TURNS: usize = 50;

/// 固定步骤，顺序执行，无分支。适合流水线任务。
///
/// 内部行为：
/// 1. 将 steps 列表注入系统提示词
/// 2. 按步骤顺序执行，每步由 LLM 调用工具完成
/// 3. 每步结果追加到消息历史，LLM 看到后继续下一步
/// 4. 所有步骤完成或达到 max_turns 后返回结果
pub async fn execute_chain(
    req: &TaskRequest,
    llm: &Arc<dyn LlmBackend>,
    tool_schemas: &[ToolSchema],
    notify_tx: &mpsc::Sender<Notification>,
    tool_executor: &Arc<dyn ToolExecutor>,
    tool_ctx: &super::ToolExecContext,
) -> TaskId {
    let start = Instant::now();
    let task_id = req.id.clone();
    let task_name = req.name.clone();
    let channel = req.channel.clone();
    let max_turns = if req.max_turns > 0 {
        req.max_turns
    } else {
        DEFAULT_MAX_TURNS
    };

    // 构建系统提示词：领域专家提示 + 步骤列表
    let system_prompt = build_chain_system_prompt(&req.domain_prompt, &req.steps);

    // 初始消息：任务描述
    let mut messages = vec![Message::user(&req.task_prompt)];

    let mut turn_count = 0;
    let mut last_output = String::new();

    // 工具循环
    loop {
        if turn_count >= max_turns {
            break;
        }

        // 构建 CompletionRequest
        let llm_messages = to_completion_messages(&messages);
        let completion_req = CompletionRequest {
            model: String::new(), // 由 LlmBackend 实现决定
            max_tokens: 4096,
            system: system_prompt.clone(),
            messages: llm_messages,
            tools: tool_schemas.to_vec(),
            stream: false,
        };

        // 调用 LLM
        let response = match llm.complete(&completion_req).await {
            Ok(resp) => resp,
            Err(e) => {
                let _ = notify_tx
                    .send(Notification {
                        id: task_id.clone(),
                        name: task_name.clone(),
                        tool: "execute_chain".into(),
                        status: TaskStatus::Failed {
                            error: format!("LLM call failed: {}", e),
                            duration: start.elapsed(),
                        },
                        output: None,
                        channel: channel.clone(),
                        phases: None,
                    })
                    .await;
                return task_id;
            }
        };

        // 提取文本和工具调用
        let (text, tool_calls) = extract_response(&response);
        last_output = text.clone();

        // 追加 assistant 消息
        messages.push(Message::assistant(
            Some(text.clone()),
            Some(tool_calls.clone()),
        ));

        // 如果没有工具调用，任务完成
        if tool_calls.is_empty() {
            break;
        }

        // 执行工具调用
        for call in &tool_calls {
            let result = match tool_executor.execute(&call.name, call.arguments.clone(), tool_ctx).await {
                Ok(r) => r,
                Err(e) => format!("[tool error] {}: {}", call.name, e),
            };
            messages.push(Message::tool_result(&call.id, &result));
        }

        turn_count += 1;
    }

    // 发送完成通知
    let notification = Notification {
        id: task_id.clone(),
        name: task_name.clone(),
        tool: "execute_chain".into(),
        status: TaskStatus::Completed {
            output: last_output.clone(),
            duration: start.elapsed(),
        },
        output: Some(last_output),
        channel: channel.clone(),
        phases: None,
    };
    match notify_tx.send(notification).await {
        Ok(_) => tracing::info!("[execute_chain] Task {} notification sent successfully", task_id),
        Err(e) => tracing::error!("[execute_chain] Task {} notification send FAILED: {}", task_id, e),
    }

    task_id
}

/// 构建 chain 模式的系统提示词
fn build_chain_system_prompt(domain_prompt: &str, steps: &Option<Vec<String>>) -> String {
    let mut prompt = String::new();

    // 领域专家提示词
    if !domain_prompt.is_empty() {
        prompt.push_str(domain_prompt);
        prompt.push_str("\n\n");
    }

    // 执行模式说明
    prompt.push_str("你正在执行一个固定步骤的任务。按以下步骤顺序执行，每步完成后继续下一步：\n\n");

    // 步骤列表
    if let Some(steps) = steps {
        for (i, step) in steps.iter().enumerate() {
            prompt.push_str(&format!("{}. {}\n", i + 1, step));
        }
        prompt.push('\n');
    }

    prompt.push_str("执行完所有步骤后，输出最终结果。不要跳过任何步骤。");

    prompt
}
