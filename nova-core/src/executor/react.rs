use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;

use crate::llm_backend::{CompletionRequest, LlmBackend, ToolSchema};
use crate::message::Message;

use super::types::{Notification, TaskId, TaskRequest, TaskStatus};
use super::ToolExecutor;
use super::util::{extract_response, to_completion_messages};

/// execute_react 的默认最大轮数
const DEFAULT_MAX_TURNS: usize = 100;

/// 边查边想，每步结果决定下一步。适合调试、调研、探索性任务。
///
/// 与 execute_chain 的区别：
/// - chain: 步骤固定，按列表顺序执行
/// - react: 无预设步骤，LLM 每步推理后自己决定下一步
///
/// 内部行为：
/// 1. 将领域专家提示词注入系统提示词
/// 2. LLM 看到工具列表，自己决定调哪个工具
/// 3. 工具结果追加到消息历史，LLM 看到后决定下一步
/// 4. 直到 LLM 不再调用工具（输出纯文本）或达到 max_turns
pub async fn execute_react(
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

    // 构建系统提示词：领域专家提示 + react 模式说明
    let system_prompt = build_react_system_prompt(&req.domain_prompt);

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
                        tool: "execute_react".into(),
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

        // 如果没有工具调用，任务完成（LLM 决定不再调用工具）
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
    let _ = notify_tx
        .send(Notification {
            id: task_id.clone(),
            name: task_name.clone(),
            tool: "execute_react".into(),
            status: TaskStatus::Completed {
                output: last_output.clone(),
                duration: start.elapsed(),
            },
            output: Some(last_output),
            channel: channel.clone(),
            phases: None,
        })
        .await;

    task_id
}

/// 构建 react 模式的系统提示词
fn build_react_system_prompt(domain_prompt: &str) -> String {
    let mut prompt = String::new();

    // 领域专家提示词
    if !domain_prompt.is_empty() {
        prompt.push_str(domain_prompt);
        prompt.push_str("\n\n");
    }

    // react 模式说明
    prompt.push_str(
        "你正在执行一个需要边查边想的任务。\
         每一步的结果会影响你下一步的决策。\n\n\
         你可以使用提供的工具来探索、分析、修改。\
         每次工具调用后，你会看到结果，然后决定下一步做什么。\n\n\
         当你认为任务已完成时，输出最终结果（不再调用工具）。\
         不要提前结束——确保你已经充分探索和验证。",
    );

    prompt
}
