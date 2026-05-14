//! Integration tests for the executor module.
//!
//! Uses a mock LlmBackend to test execute_chain, execute_react, and execute_parallel
//! without making real API calls.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use async_trait::async_trait;
use tokio::sync::mpsc;

use nova_core::executor::types::{
    ChannelContext, Notification, TaskRequest, TaskStatus,
};
use nova_core::executor::{ToolExecContext, ToolExecutor};
use nova_core::platform::Platform;

/// Mock tool executor that returns a fixed string for any tool call.
struct MockToolExecutor;

#[async_trait]
impl ToolExecutor for MockToolExecutor {
    async fn execute(&self, name: &str, _args: serde_json::Value, _ctx: &ToolExecContext) -> anyhow::Result<String> {
        Ok(format!("[mock] {} executed", name))
    }
}

fn mock_tool_executor() -> Arc<dyn ToolExecutor> {
    Arc::new(MockToolExecutor)
}

fn mock_tool_ctx() -> ToolExecContext {
    ToolExecContext::new("test".into(), None)
}
use nova_core::llm_backend::{
    CompletionRequest, CompletionResponse,
    ContentBlock, LlmBackend, StreamDelta, TokenUsage,
};

/// Mock LLM backend that returns predefined responses in sequence.
struct MockLlm {
    responses: Vec<CompletionResponse>,
    call_count: AtomicUsize,
}

impl MockLlm {
    fn new(responses: Vec<CompletionResponse>) -> Self {
        Self {
            responses,
            call_count: AtomicUsize::new(0),
        }
    }

    fn single_text(text: &str) -> Self {
        Self::new(vec![text_response(text)])
    }

    fn into_backend(self) -> Arc<dyn LlmBackend> {
        Arc::new(self)
    }
}

fn text_response(text: &str) -> CompletionResponse {
    CompletionResponse {
        id: "mock-1".into(),
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        stop_reason: Some("end_turn".into()),
        usage: TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
        },
    }
}

fn tool_use_response(tool_id: &str, tool_name: &str, input: serde_json::Value) -> CompletionResponse {
    CompletionResponse {
        id: "mock-2".into(),
        content: vec![ContentBlock::ToolUse {
            id: tool_id.to_string(),
            name: tool_name.to_string(),
            input,
        }],
        stop_reason: Some("tool_use".into()),
        usage: TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
        },
    }
}

#[async_trait]
impl LlmBackend for MockLlm {
    async fn complete(&self, _req: &CompletionRequest) -> anyhow::Result<CompletionResponse> {
        let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
        if idx < self.responses.len() {
            Ok(self.responses[idx].clone())
        } else {
            Ok(text_response("(mock exhausted)"))
        }
    }

    async fn stream(
        &self,
        _req: &CompletionRequest,
        _tx: mpsc::Sender<StreamDelta>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

fn make_task_request(name: &str, task_prompt: &str) -> TaskRequest {
    TaskRequest {
        id: format!("test-{}", name),
        name: name.to_string(),
        mode: nova_core::executor::types::ExecMode::React,
        task_prompt: task_prompt.to_string(),
        domain_prompt: "你是测试专家".to_string(),
        steps: None,
        review_prompt: None,
        research_prompt: None,
        implementation_prompt: None,
        max_turns: 10,
        timeout: Duration::from_secs(60),
        tools: vec![],
        channel: ChannelContext {
            platform: Platform::Tui,
            channel_id: "test".into(),
            reply_to: None,
        },
    }
}

fn make_chain_request(name: &str, task_prompt: &str, steps: Vec<String>) -> TaskRequest {
    let mut req = make_task_request(name, task_prompt);
    req.mode = nova_core::executor::types::ExecMode::Chain;
    req.steps = Some(steps);
    req
}

fn make_parallel_request(name: &str, task_prompt: &str) -> TaskRequest {
    let mut req = make_task_request(name, task_prompt);
    req.mode = nova_core::executor::types::ExecMode::Parallel;
    req
}

// ── execute_chain tests ──────────────────────────────────────────

#[tokio::test]
async fn chain_simple_text_response() {
    let llm = MockLlm::single_text("任务完成").into_backend();
    let (tx, mut rx) = mpsc::channel::<Notification>(16);
    let req = make_chain_request("chain-simple", "执行任务", vec!["步骤1".into()]);

    let task_id = nova_core::executor::chain::execute_chain(&req, &llm, &[], &tx, &mock_tool_executor(), &mock_tool_ctx()).await;
    assert_eq!(task_id, "test-chain-simple");

    let notification = rx.recv().await.unwrap();
    match &notification.status {
        TaskStatus::Completed { output, .. } => {
            assert_eq!(output, "任务完成");
        }
        other => panic!("expected Completed, got {:?}", other),
    }
}

#[tokio::test]
async fn chain_with_tool_calls() {
    let llm = MockLlm::new(vec![
        tool_use_response("call-1", "read_file", serde_json::json!({"path": "test.rs"})),
        text_response("读取完成"),
    ]).into_backend();
    let (tx, mut rx) = mpsc::channel::<Notification>(16);
    let req = make_chain_request("chain-tools", "读取文件", vec!["读取test.rs".into()]);

    let _task_id = nova_core::executor::chain::execute_chain(&req, &llm, &[], &tx, &mock_tool_executor(), &mock_tool_ctx()).await;

    let notification = rx.recv().await.unwrap();
    match &notification.status {
        TaskStatus::Completed { output, .. } => {
            assert_eq!(output, "读取完成");
        }
        other => panic!("expected Completed, got {:?}", other),
    }
}

// ── execute_react tests ──────────────────────────────────────────

#[tokio::test]
async fn react_simple_text_response() {
    let llm = MockLlm::single_text("调研结果").into_backend();
    let (tx, mut rx) = mpsc::channel::<Notification>(16);
    let req = make_task_request("react-simple", "调研任务");

    let _task_id = nova_core::executor::react::execute_react(&req, &llm, &[], &tx, &mock_tool_executor(), &mock_tool_ctx()).await;

    let notification = rx.recv().await.unwrap();
    match &notification.status {
        TaskStatus::Completed { output, .. } => {
            assert_eq!(output, "调研结果");
        }
        other => panic!("expected Completed, got {:?}", other),
    }
}

#[tokio::test]
async fn react_with_tool_loop() {
    let llm = MockLlm::new(vec![
        tool_use_response("c1", "grep", serde_json::json!({"pattern": "fn main"})),
        tool_use_response("c2", "read_file", serde_json::json!({"path": "main.rs"})),
        text_response("找到main函数"),
    ]).into_backend();
    let (tx, mut rx) = mpsc::channel::<Notification>(16);
    let req = make_task_request("react-loop", "找到入口函数");

    let _task_id = nova_core::executor::react::execute_react(&req, &llm, &[], &tx, &mock_tool_executor(), &mock_tool_ctx()).await;

    let notification = rx.recv().await.unwrap();
    match &notification.status {
        TaskStatus::Completed { output, .. } => {
            assert_eq!(output, "找到main函数");
        }
        other => panic!("expected Completed, got {:?}", other),
    }
}

// ── execute_parallel tests ───────────────────────────────────────

#[tokio::test]
async fn parallel_splits_subtasks() {
    let llm = MockLlm::new(vec![
        text_response("子任务1结果"),
        text_response("子任务2结果"),
        text_response("子任务3结果"),
    ]).into_backend();
    let (tx, mut rx) = mpsc::channel::<Notification>(16);
    let req = make_parallel_request("parallel-split", "子任务1\n---\n子任务2\n---\n子任务3");

    let _task_id = nova_core::executor::parallel::execute_parallel(&req, &llm, &[], &tx, &mock_tool_executor(), &mock_tool_ctx()).await;

    let notification = rx.recv().await.unwrap();
    match &notification.status {
        TaskStatus::Completed { output, .. } => {
            assert!(output.contains("子任务1结果"));
            assert!(output.contains("子任务2结果"));
            assert!(output.contains("子任务3结果"));
        }
        other => panic!("expected Completed, got {:?}", other),
    }
}

#[tokio::test]
async fn parallel_single_task_degrades_to_react() {
    let llm = MockLlm::single_text("单任务结果").into_backend();
    let (tx, mut rx) = mpsc::channel::<Notification>(16);
    let req = make_task_request("parallel-single", "单个任务");

    let _task_id = nova_core::executor::parallel::execute_parallel(&req, &llm, &[], &tx, &mock_tool_executor(), &mock_tool_ctx()).await;

    let notification = rx.recv().await.unwrap();
    match &notification.status {
        TaskStatus::Completed { output, .. } => {
            assert_eq!(output, "单任务结果");
        }
        other => panic!("expected Completed, got {:?}", other),
    }
}

// ── Notification tests ───────────────────────────────────────────

#[test]
fn notification_completed_xml() {
    let n = Notification {
        id: "test-1".into(),
        name: "测试".into(),
        tool: "execute_react".into(),
        status: TaskStatus::Completed {
            output: "done".into(),
            duration: Duration::from_secs_f64(1.5),
        },
        output: Some("done".into()),
        channel: ChannelContext {
            platform: Platform::Tui,
            channel_id: "ch1".into(),
            reply_to: None,
        },
        phases: None,
    };
    let xml = n.to_xml();
    assert!(xml.contains("<task-id>test-1</task-id>"));
    assert!(xml.contains("<status>completed</status>"));
}

#[test]
fn notification_failed_xml() {
    let n = Notification {
        id: "test-2".into(),
        name: "失败任务".into(),
        tool: "execute_chain".into(),
        status: TaskStatus::Failed {
            error: "LLM error".into(),
            duration: Duration::from_secs_f64(0.5),
        },
        output: None,
        channel: ChannelContext {
            platform: Platform::Discord,
            channel_id: "ch2".into(),
            reply_to: None,
        },
        phases: None,
    };
    let xml = n.to_xml();
    assert!(xml.contains("<status>failed</status>"));
    assert!(xml.contains("LLM error"));
}
