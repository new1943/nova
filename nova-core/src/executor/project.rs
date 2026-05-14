use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;

use crate::llm_backend::{CompletionRequest, LlmBackend, ToolSchema};
use crate::message::Message;

use super::types::{Notification, PhaseInfo, TaskId, TaskRequest, TaskStatus};
use super::ToolExecutor;
use super::util::{extract_response, to_completion_messages};

/// execute_project 的默认最大轮数（每阶段的工具调用轮数）
const DEFAULT_MAX_TURNS: usize = 50;
/// 最大实现+验证循环次数
const DEFAULT_MAX_ITERATIONS: usize = 3;

/// 默认的研究提示词
const DEFAULT_RESEARCH_PROMPT: &str = r#"你是一个代码库研究专家。你的任务是调查、分析、理解问题。

约束：
- 只读：不能修改任何文件
- 输出：报告你的发现，包括文件路径、行号、类型签名
- 深度：足够让实现者不需要再重复调研

完成后输出你的发现。"#;

/// 默认的实现提示词
const DEFAULT_IMPLEMENTATION_PROMPT: &str = r#"你是一个实现专家。你将收到一个具体的实施方案（spec），包含文件路径、行号、具体改什么。

约束：
- 按 spec 精确执行，不要偏离
- 修改后运行相关测试验证
- 完成后报告你做了什么、修改了哪些文件

完成后输出你的实现结果。"#;

/// 默认的验证提示词
const DEFAULT_VERIFICATION_PROMPT: &str = r#"你是一个验证专家。你的工作不是确认实现能用——而是试图打破它。

约束：
- 只读：不能修改项目文件（/tmp 可以写临时测试脚本）
- 必须运行命令验证，不能只读代码就说 PASS
- 每个检查必须有：命令 → 输出 → 结果
- 最终输出 VERDICT: PASS / FAIL / PARTIAL

自检：
- "代码看起来对" → 不是验证，运行它
- "测试已经通过了" → 实现者也是 LLM，独立验证
- "应该没问题" → "应该"不是"已验证""#;

/// 复杂项目协调器。自动编排：规划→执行→验收→循环，直到所有步骤完成。
///
/// 四阶段模式（参考 Claude Code Coordinator）：
///
/// Phase 1: Research（并行）
///   - spawn N 个 research worker（只读工具）
///   - 每个 worker 独立调研，返回 findings
///   - 所有 worker 完成后，结果汇总给协调器
///
/// Phase 2: Synthesis（协调器自己做，不 spawn）
///   - 协调器读取所有 research findings
///   - 理解问题，制定具体实施方案（spec）
///   - 关键：协调器必须自己综合理解，不能写 "based on your findings"
///
/// Phase 3: Implementation（串行或小批量并行）
///   - 按 spec 逐个 spawn implementation worker
///   - 每个 worker 拿到自包含的 prompt（文件路径、行号、具体改什么）
///   - worker 完成后通知协调器
///
/// Phase 4: Verification（对抗性验证）
///   - spawn verification worker（只读工具）
///   - 验证是对抗性的：试图打破它，不是确认它存在
///   - 输出 VERDICT: PASS / FAIL / PARTIAL
///   - FAIL → 回到 Phase 3 修正 → 再验证
pub async fn execute_project(
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
    let max_iterations = DEFAULT_MAX_ITERATIONS;

    // 提示词：主 Agent 提供的 或 默认模板
    let research_prompt = req
        .research_prompt
        .clone()
        .unwrap_or_else(|| DEFAULT_RESEARCH_PROMPT.to_string());
    let implementation_prompt = req
        .implementation_prompt
        .clone()
        .unwrap_or_else(|| DEFAULT_IMPLEMENTATION_PROMPT.to_string());
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

    let mut phases = Vec::new();

    // ── Phase 1: Research（并行）─────────────────────────────
    let research_start = Instant::now();

    // 将 task_prompt 按 "---" 拆分为多个研究方向
    let research_tasks = split_research_tasks(&req.task_prompt);
    let research_count = research_tasks.len();

    // 并行 spawn research workers
    let mut research_handles = Vec::new();
    for (i, research_task) in research_tasks.into_iter().enumerate() {
        let llm = Arc::clone(llm);
        let research_prompt = research_prompt.clone();
        let subtask_id = format!("{}-research-{}", task_id, i);
        let read_only_schemas = read_only_schemas.clone();
        let tool_executor = Arc::clone(tool_executor);
        let tool_ctx = tool_ctx.clone();

        let handle = tokio::spawn(async move {
            run_subagent(
                &subtask_id,
                &research_task,
                &research_prompt,
                &llm,
                &read_only_schemas,
                max_turns,
                &tool_executor,
                &tool_ctx,
            )
            .await
        });
        research_handles.push(handle);
    }

    // 收集研究结果
    let mut research_findings = Vec::new();
    for (i, handle) in research_handles.into_iter().enumerate() {
        match handle.await {
            Ok(Ok(finding)) => {
                research_findings.push(format!("## 研究方向 {}\n{}", i + 1, finding));
            }
            Ok(Err(e)) => {
                research_findings.push(format!("## 研究方向 {} 失败\n{}", i + 1, e));
            }
            Err(e) => {
                research_findings.push(format!("## 研究方向 {} panic\n{}", i + 1, e));
            }
        }
    }

    phases.push(PhaseInfo {
        name: "research".into(),
        worker_count: research_count,
        duration: research_start.elapsed(),
        verdict: None,
    });

    // ── Phase 2: Synthesis（协调器自己做）────────────────────
    let synthesis_start = Instant::now();

    // 用 LLM 综合研究结果，生成实施方案
    let synthesis_prompt = format!(
        "你是项目协调器。以下是你派出的研究 workers 的发现：\n\n\
         {}\n\n\
         原始任务：{}\n\n\
         请综合这些发现，制定一个具体的实施方案（spec）。\n\
         spec 必须包含：\n\
         1. 具体要修改哪些文件（文件路径）\n\
         2. 每个文件要改什么（行号、函数名、具体改动）\n\
         3. 修改的顺序和依赖关系\n\n\
         不要写 \"based on your findings\" —— 你自己理解后再写 spec。",
        research_findings.join("\n\n---\n\n"),
        req.task_prompt
    );

    let spec = match run_subagent(
        &format!("{}-synthesis", task_id),
        &synthesis_prompt,
        &req.domain_prompt,
        llm,
        &[],
        max_turns,
        tool_executor,
        tool_ctx,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            let _ = notify_tx
                .send(Notification {
                    id: task_id.clone(),
                    name: task_name.clone(),
                    tool: "execute_project".into(),
                    status: TaskStatus::Failed {
                        error: format!("Synthesis failed: {}", e),
                        duration: start.elapsed(),
                    },
                    output: None,
                    channel: channel.clone(),
                    phases: Some(phases),
                })
                .await;
            return task_id;
        }
    };

    phases.push(PhaseInfo {
        name: "synthesis".into(),
        worker_count: 1,
        duration: synthesis_start.elapsed(),
        verdict: None,
    });

    // ── Phase 3 + 4: Implementation → Verification 循环 ─────
    let mut current_spec = spec;
    let mut implementation_output = String::new();

    for iteration in 0..max_iterations {
        // Phase 3: Implementation
        let impl_start = Instant::now();

        let impl_prompt = format!(
            "请按以下 spec 实施：\n\n{}\n\n{}",
            current_spec, implementation_prompt
        );

        implementation_output = match run_subagent(
            &format!("{}-impl-{}", task_id, iteration),
            &impl_prompt,
            &req.domain_prompt,
            llm,
            tool_schemas,
            max_turns,
            tool_executor,
            tool_ctx,
        )
        .await
        {
            Ok(output) => output,
            Err(e) => {
                let _ = notify_tx
                    .send(Notification {
                        id: task_id.clone(),
                        name: task_name.clone(),
                        tool: "execute_project".into(),
                        status: TaskStatus::Failed {
                            error: format!("Implementation failed: {}", e),
                            duration: start.elapsed(),
                        },
                        output: None,
                        channel: channel.clone(),
                        phases: Some(phases),
                    })
                    .await;
                return task_id;
            }
        };

        phases.push(PhaseInfo {
            name: format!("implementation-{}", iteration + 1),
            worker_count: 1,
            duration: impl_start.elapsed(),
            verdict: None,
        });

        // Phase 4: Verification
        let verify_start = Instant::now();

        let verify_prompt = format!(
            "请验证以下实现：\n\n原始任务：{}\n\n实施方案：\n{}\n\n实现结果：\n{}\n\n{}",
            req.task_prompt, current_spec, implementation_output, verification_prompt
        );

        let verification_output = match run_subagent(
            &format!("{}-verify-{}", task_id, iteration),
            &verify_prompt,
            "",
            llm,
            &read_only_schemas,
            max_turns,
            tool_executor,
            tool_ctx,
        )
        .await
        {
            Ok(output) => output,
            Err(e) => {
                phases.push(PhaseInfo {
                    name: format!("verification-{}", iteration + 1),
                    worker_count: 1,
                    duration: verify_start.elapsed(),
                    verdict: Some("ERROR".into()),
                });
                let _ = notify_tx
                    .send(Notification {
                        id: task_id.clone(),
                        name: task_name.clone(),
                        tool: "execute_project".into(),
                        status: TaskStatus::Failed {
                            error: format!("Verification failed: {}", e),
                            duration: start.elapsed(),
                        },
                        output: Some(implementation_output),
                        channel: channel.clone(),
                        phases: Some(phases),
                    })
                    .await;
                return task_id;
            }
        };

        // 解析 VERDICT
        let verdict = parse_verdict(&verification_output);

        phases.push(PhaseInfo {
            name: format!("verification-{}", iteration + 1),
            worker_count: 1,
            duration: verify_start.elapsed(),
            verdict: Some(verdict.clone()),
        });

        match verdict.as_str() {
            "PASS" => {
                // 验证通过，任务完成
                let _ = notify_tx
                    .send(Notification {
                        id: task_id.clone(),
                        name: task_name.clone(),
                        tool: "execute_project".into(),
                        status: TaskStatus::Completed {
                            output: implementation_output.clone(),
                            duration: start.elapsed(),
                        },
                        output: Some(implementation_output),
                        channel: channel.clone(),
                        phases: Some(phases),
                    })
                    .await;
                return task_id;
            }
            "FAIL" => {
                // 验证失败，修正 spec 重新实现
                current_spec = format!(
                    "之前的实现有问题，请修正：\n\n{}\n\n原始 spec：\n{}",
                    verification_output, current_spec
                );
                // 继续循环
            }
            _ => {
                // PARTIAL 或其他，返回结果 + 警告
                let _ = notify_tx
                    .send(Notification {
                        id: task_id.clone(),
                        name: task_name.clone(),
                        tool: "execute_project".into(),
                        status: TaskStatus::Completed {
                            output: format!(
                                "[验证 PARTIAL]\n{}\n\n实现结果：\n{}",
                                verification_output, implementation_output
                            ),
                            duration: start.elapsed(),
                        },
                        output: Some(implementation_output),
                        channel: channel.clone(),
                        phases: Some(phases),
                    })
                    .await;
                return task_id;
            }
        }
    }

    // 达到最大迭代次数
    let _ = notify_tx
        .send(Notification {
            id: task_id.clone(),
            name: task_name.clone(),
            tool: "execute_project".into(),
            status: TaskStatus::Completed {
                output: format!(
                    "[达到最大验证轮数 {}]\n{}",
                    max_iterations, implementation_output
                ),
                duration: start.elapsed(),
            },
            output: Some(implementation_output),
            channel: channel.clone(),
            phases: Some(phases),
        })
        .await;

    task_id
}

/// 按 "---" 分隔符拆分研究方向
fn split_research_tasks(task_prompt: &str) -> Vec<String> {
    let tasks: Vec<String> = task_prompt
        .split("\n---\n")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if tasks.is_empty() {
        vec![task_prompt.to_string()]
    } else {
        tasks
    }
}

/// 运行单个 subagent
#[allow(clippy::too_many_arguments)]
async fn run_subagent(
    _task_id: &str,
    task_prompt: &str,
    domain_prompt: &str,
    llm: &Arc<dyn LlmBackend>,
    tool_schemas: &[ToolSchema],
    max_turns: usize,
    tool_executor: &Arc<dyn ToolExecutor>,
    tool_ctx: &super::ToolExecContext,
) -> Result<String, String> {
    let system_prompt = if domain_prompt.is_empty() {
        String::new()
    } else {
        domain_prompt.to_string()
    };

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

/// 从输出中解析 VERDICT
fn parse_verdict(output: &str) -> String {
    let upper = output.to_uppercase();

    if upper.contains("VERDICT: PASS") {
        "PASS".to_string()
    } else if upper.contains("VERDICT: FAIL") {
        "FAIL".to_string()
    } else {
        "PARTIAL".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_single_task() {
        let tasks = split_research_tasks("单一任务");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0], "单一任务");
    }

    #[test]
    fn split_multiple_tasks() {
        let tasks = split_research_tasks("任务一\n---\n任务二\n---\n任务三");
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0], "任务一");
        assert_eq!(tasks[1], "任务二");
        assert_eq!(tasks[2], "任务三");
    }

    #[test]
    fn parse_verdict_pass() {
        assert_eq!(parse_verdict("VERDICT: PASS"), "PASS");
    }

    #[test]
    fn parse_verdict_fail() {
        assert_eq!(parse_verdict("VERDICT: FAIL"), "FAIL");
    }

    #[test]
    fn parse_verdict_missing() {
        assert_eq!(parse_verdict("没有 verdict"), "PARTIAL");
    }
}
