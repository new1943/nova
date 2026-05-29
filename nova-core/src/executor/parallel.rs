use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;

use crate::llm_backend::{CompletionRequest, LlmBackend, ToolSchema};
use crate::message::Message;

use super::types::{Notification, TaskId, TaskRequest, TaskStatus};
use super::ToolExecutor;
use super::util::{extract_response, to_completion_messages};

/// execute_parallel 的默认最大轮数（每个子任务）
const DEFAULT_MAX_TURNS: usize = 50;

/// 多个独立子任务同时执行。适合同时查询多个数据源。
///
/// 内部行为：
/// 1. 将 task_prompt 按分隔符拆分为多个子任务
/// 2. 为每个子任务 spawn 一个 tokio task
/// 3. 每个子任务独立执行 LLM 工具循环
/// 4. 所有子任务完成后，汇总结果
///
/// 子任务拆分方式：
/// - 如果 task_prompt 包含 "---" 分隔符，按分隔符拆分
/// - 否则作为单个任务执行（退化为 react 模式）
pub async fn execute_parallel(
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

    // 拆分子任务
    let subtasks = split_subtasks(&req.task_prompt);

    if subtasks.len() <= 1 {
        // 单任务，退化为 react 模式
        return super::react::execute_react(req, llm, tool_schemas, notify_tx, tool_executor, tool_ctx).await;
    }

    // 并行执行所有子任务
    let mut handles = Vec::new();
    for (i, subtask) in subtasks.into_iter().enumerate() {
        let llm = Arc::clone(llm);
        let tool_schemas = tool_schemas.to_vec();
        let domain_prompt = req.domain_prompt.clone();
        let subtask_id = format!("{}-{}", task_id, i);
        let tool_executor = Arc::clone(tool_executor);
        let tool_ctx = tool_ctx.clone();

        let handle = tokio::spawn(async move {
            execute_single_subtask(
                &subtask_id,
                &subtask,
                &domain_prompt,
                &llm,
                &tool_schemas,
                max_turns,
                &tool_executor,
                &tool_ctx,
            )
            .await
        });

        handles.push((i, handle));
    }

    // 收集所有子任务结果
    let mut results = Vec::new();
    let mut all_success = true;
    for (i, handle) in handles {
        match handle.await {
            Ok(Ok(output)) => {
                results.push(format!("子任务 {}:\n{}", i + 1, output));
            }
            Ok(Err(e)) => {
                results.push(format!("子任务 {} 失败: {}", i + 1, e));
                all_success = false;
            }
            Err(e) => {
                results.push(format!("子任务 {} panic: {}", i + 1, e));
                all_success = false;
            }
        }
    }

    let output = results.join("\n\n---\n\n");
    let duration = start.elapsed();

    // 发送完成通知
    let status = if all_success {
        TaskStatus::Completed {
            output: output.clone(),
            duration,
        }
    } else {
        TaskStatus::Failed {
            error: "部分子任务失败".into(),
            duration,
        }
    };

    let notification = Notification {
        id: task_id.clone(),
        name: task_name.clone(),
        tool: "execute_parallel".into(),
        status,
        output: Some(output),
        channel: channel.clone(),
        phases: None,
    };
    match notify_tx.send(notification).await {
        Ok(_) => tracing::info!("[execute_parallel] Task {} notification sent successfully", task_id),
        Err(e) => tracing::error!("[execute_parallel] Task {} notification send FAILED: {}", task_id, e),
    }

    task_id
}

/// 按 "---" 分隔符拆分子任务
fn split_subtasks(task_prompt: &str) -> Vec<String> {
    task_prompt
        .split("\n---\n")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// 执行单个子任务（内部 react 循环）
#[allow(clippy::too_many_arguments)]
async fn execute_single_subtask(
    _task_id: &str,
    task_prompt: &str,
    domain_prompt: &str,
    llm: &Arc<dyn LlmBackend>,
    tool_schemas: &[ToolSchema],
    max_turns: usize,
    tool_executor: &Arc<dyn ToolExecutor>,
    tool_ctx: &super::ToolExecContext,
) -> Result<String, String> {
    let system_prompt = build_parallel_subtask_prompt(domain_prompt);
    let mut messages = vec![Message::user(task_prompt)];
    let mut turn_count = 0;
    let mut last_output = String::new();

    loop {
        if turn_count >= max_turns {
            break;
        }

        let llm_messages = to_completion_messages(&messages);
        let completion_req = CompletionRequest {
            model: String::new(),
            max_tokens: 4096,
            system: system_prompt.clone(),
            messages: llm_messages,
            tools: tool_schemas.to_vec(),
            stream: false,
        };

        let response = llm
            .complete(&completion_req)
            .await
            .map_err(|e| format!("LLM call failed: {}", e))?;

        let (text, tool_calls) = extract_response(&response);
        last_output = text.clone();

        messages.push(Message::assistant(Some(text), Some(tool_calls.clone())));

        if tool_calls.is_empty() {
            break;
        }

        for call in &tool_calls {
            let result = match tool_executor.execute(&call.name, call.arguments.clone(), tool_ctx).await {
                Ok(r) => r,
                Err(e) => format!("[tool error] {}: {}", call.name, e),
            };
            messages.push(Message::tool_result(&call.id, &result));
        }

        turn_count += 1;
    }

    Ok(last_output)
}

/// 构建并行子任务的系统提示词
fn build_parallel_subtask_prompt(domain_prompt: &str) -> String {
    let mut prompt = String::new();

    if !domain_prompt.is_empty() {
        prompt.push_str(domain_prompt);
        prompt.push_str("\n\n");
    }

    prompt.push_str(
        "你正在执行一个并行子任务。\
         你的任务是独立完成分配给你的工作，不需要关心其他子任务。\n\n\
         完成后输出你的结果。不要调用不必要的工具。",
    );

    prompt
}
