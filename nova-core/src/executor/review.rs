use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;

use crate::llm_backend::{CompletionRequest, LlmBackend, ToolSchema};
use crate::message::Message;

use super::types::{Notification, TaskId, TaskRequest, TaskStatus};
use super::ToolExecutor;
use super::util::{extract_response, to_completion_messages};

/// execute_with_review 的默认最大轮数（每轮：执行 + 验证）
const DEFAULT_MAX_ROUNDS: usize = 3;
/// 每个子 agent 的最大工具调用轮数
const DEFAULT_MAX_TURNS: usize = 50;

/// 默认的对抗性验证提示词
const DEFAULT_VERIFICATION_PROMPT: &str = r#"你是一个验证专家。你的工作不是确认实现能用——而是试图打破它。

关键约束：
- 只读：不能修改项目文件（/tmp 可以写临时测试脚本）
- 必须运行命令验证，不能只读代码就说 PASS
- 每个检查必须有：命令 → 输出 → 结果
- 最终输出 VERDICT: PASS / FAIL / PARTIAL

对抗性检查：
- 并发：并发请求会不会重复创建？
- 边界值：0, -1, 空字符串, 超长字符串, unicode
- 幂等性：同一变更请求两次会怎样？
- 孤儿操作：删除/引用不存在的 ID

自检：
- "代码看起来对" → 不是验证，运行它
- "测试已经通过了" → 实现者也是 LLM，独立验证
- "应该没问题" → "应该"不是"已验证"

输出格式（必须在最后）：
VERDICT: PASS
或
VERDICT: FAIL
或
VERDICT: PARTIAL

PARTIAL 仅用于环境限制（没有测试框架、工具不可用）——不是"我不确定"。"#;

/// 执行 + 自我审查。生成结果后换一个视角审视，发现错误则修正。
///
/// 内部行为：
/// 1. spawn executor subagent（全工具）→ 执行任务
/// 2. spawn reviewer subagent（只读工具）→ 对抗性验证
/// 3. reviewer 输出 VERDICT:
///    - PASS → 返回结果
///    - FAIL → 把失败原因传回 executor，重新执行
///    - PARTIAL → 返回结果 + 警告
/// 4. 最多 N 轮（默认 3）
pub async fn execute_with_review(
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
    let max_rounds = DEFAULT_MAX_ROUNDS;
    let max_turns = if req.max_turns > 0 {
        req.max_turns
    } else {
        DEFAULT_MAX_TURNS
    };

    // 验证提示词：主 Agent 提供的 或 默认对抗性模板
    let verification_prompt = req
        .review_prompt
        .clone()
        .unwrap_or_else(|| DEFAULT_VERIFICATION_PROMPT.to_string());

    // 只读工具 schema：用 ToolExecutor::is_read_only 过滤
    let read_only_schemas: Vec<_> = tool_schemas
        .iter()
        .filter(|s| tool_executor.is_read_only(&s.name))
        .cloned()
        .collect();

    let mut current_task = req.task_prompt.clone();
    let mut last_output = String::new();
    let mut verdict = String::from("PASS");

    for round in 0..max_rounds {
        // Phase 1: 执行
        let executor_output = run_executor(
            &current_task,
            &req.domain_prompt,
            llm,
            tool_schemas,
            max_turns,
            tool_executor,
            tool_ctx,
        )
        .await;

        match executor_output {
            Ok(output) => {
                last_output = output.clone();

                // Phase 2: 验证
                let review_result = run_reviewer(
                    &output,
                    &current_task,
                    &verification_prompt,
                    llm,
                    &read_only_schemas,
                    max_turns,
                    tool_executor,
                    tool_ctx,
                )
                .await;

                match review_result {
                    ReviewResult::Pass => {
                        verdict = "PASS".into();
                        break;
                    }
                    ReviewResult::Fail(feedback) => {
                        verdict = "FAIL".into();
                        // 把失败原因传回，重新执行
                        current_task = format!(
                            "之前的执行有问题，请修正：\n\n{}\n\n原始任务：\n{}",
                            feedback, req.task_prompt
                        );
                        if round == max_rounds - 1 {
                            // 最后一轮，返回带 FAIL 标记的结果
                            let _ = notify_tx
                                .send(Notification {
                                    id: task_id.clone(),
                                    name: task_name.clone(),
                                    tool: "execute_with_review".into(),
                                    status: TaskStatus::Completed {
                                        output: format!(
                                            "[验证 FAIL] {}\n\n执行结果：\n{}",
                                            feedback, last_output
                                        ),
                                        duration: start.elapsed(),
                                    },
                                    output: Some(last_output.clone()),
                                    channel: channel.clone(),
                                    phases: None,
                                })
                                .await;
                            return task_id;
                        }
                    }
                    ReviewResult::Partial(reason) => {
                        let _ = notify_tx
                            .send(Notification {
                                id: task_id.clone(),
                                name: task_name.clone(),
                                tool: "execute_with_review".into(),
                                status: TaskStatus::Completed {
                                    output: format!(
                                        "[验证 PARTIAL] {}\n\n执行结果：\n{}",
                                        reason, last_output
                                    ),
                                    duration: start.elapsed(),
                                },
                                output: Some(last_output.clone()),
                                channel: channel.clone(),
                                phases: None,
                            })
                            .await;
                        return task_id;
                    }
                }
            }
            Err(e) => {
                let _ = notify_tx
                    .send(Notification {
                        id: task_id.clone(),
                        name: task_name.clone(),
                        tool: "execute_with_review".into(),
                        status: TaskStatus::Failed {
                            error: format!("执行失败: {}", e),
                            duration: start.elapsed(),
                        },
                        output: None,
                        channel: channel.clone(),
                        phases: None,
                    })
                    .await;
                return task_id;
            }
        }
    }

    // 发送完成通知
    let _ = notify_tx
        .send(Notification {
            id: task_id.clone(),
            name: task_name.clone(),
            tool: "execute_with_review".into(),
            status: TaskStatus::Completed {
                output: format!("[验证 {}]\n{}", verdict, last_output),
                duration: start.elapsed(),
            },
            output: Some(last_output),
            channel: channel.clone(),
            phases: None,
        })
        .await;

    task_id
}

/// 验证结果
enum ReviewResult {
    Pass,
    Fail(String),
    Partial(String),
}

/// 执行子 agent
async fn run_executor(
    task: &str,
    domain_prompt: &str,
    llm: &Arc<dyn LlmBackend>,
    tool_schemas: &[ToolSchema],
    max_turns: usize,
    tool_executor: &Arc<dyn ToolExecutor>,
    tool_ctx: &super::ToolExecContext,
) -> Result<String, String> {
    let system_prompt = build_executor_prompt(domain_prompt);
    let mut messages = vec![Message::user(task)];
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

/// 验证子 agent（只读，对抗性）
#[allow(clippy::too_many_arguments)]
async fn run_reviewer(
    executor_output: &str,
    original_task: &str,
    verification_prompt: &str,
    llm: &Arc<dyn LlmBackend>,
    read_only_schemas: &[ToolSchema],
    max_turns: usize,
    tool_executor: &Arc<dyn ToolExecutor>,
    tool_ctx: &super::ToolExecContext,
) -> ReviewResult {
    let system_prompt = verification_prompt.to_string();
    let user_prompt = format!(
        "请验证以下执行结果：\n\n\
         原始任务：\n{}\n\n\
         执行结果：\n{}\n\n\
         请验证结果的正确性，输出 VERDICT: PASS / FAIL / PARTIAL",
        original_task, executor_output
    );

    let mut messages = vec![Message::user(user_prompt)];
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
            tools: read_only_schemas.to_vec(),
            stream: false,
        };

        let response = match llm.complete(&completion_req).await {
            Ok(r) => r,
            Err(e) => return ReviewResult::Partial(format!("验证 LLM 调用失败: {}", e)),
        };

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

    // 解析 VERDICT
    parse_verdict(&last_output)
}

/// 从验证输出中解析 VERDICT
fn parse_verdict(output: &str) -> ReviewResult {
    let upper = output.to_uppercase();

    if upper.contains("VERDICT: PASS") {
        ReviewResult::Pass
    } else if upper.contains("VERDICT: FAIL") {
        // 提取 FAIL 原因
        let reason = output
            .lines()
            .skip_while(|l| !l.to_uppercase().contains("VERDICT: FAIL"))
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n");
        ReviewResult::Fail(reason)
    } else if upper.contains("VERDICT: PARTIAL") {
        let reason = output
            .lines()
            .skip_while(|l| !l.to_uppercase().contains("VERDICT: PARTIAL"))
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n");
        ReviewResult::Partial(reason)
    } else {
        // 没有明确 VERDICT，视为 PARTIAL
        ReviewResult::Partial("验证未输出明确 VERDICT".into())
    }
}

/// 构建执行者的系统提示词
fn build_executor_prompt(domain_prompt: &str) -> String {
    let mut prompt = String::new();

    if !domain_prompt.is_empty() {
        prompt.push_str(domain_prompt);
        prompt.push_str("\n\n");
    }

    prompt.push_str(
        "你正在执行一个任务。完成后输出你的结果。\n\
         你的结果将被一个独立的验证者审查，所以请确保：\n\
         1. 输出清晰、完整\n\
         2. 包含你做了什么、为什么这样做\n\
         3. 如果有测试结果，一并输出",
    );

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_verdict_pass() {
        match parse_verdict("检查完成\n\nVERDICT: PASS") {
            ReviewResult::Pass => {}
            _ => panic!("expected Pass"),
        }
    }

    #[test]
    fn parse_verdict_fail() {
        match parse_verdict("发现问题\n\nVERDICT: FAIL\n错误详情...") {
            ReviewResult::Fail(reason) => assert!(reason.contains("错误详情")),
            _ => panic!("expected Fail"),
        }
    }

    #[test]
    fn parse_verdict_partial() {
        match parse_verdict("无法完全验证\n\nVERDICT: PARTIAL") {
            ReviewResult::Partial(_) => {}
            _ => panic!("expected Partial"),
        }
    }

    #[test]
    fn parse_verdict_missing() {
        match parse_verdict("没有输出 verdict") {
            ReviewResult::Partial(reason) => assert!(reason.contains("未输出")),
            _ => panic!("expected Partial"),
        }
    }
}
