// Unit tests for SideQuery using MockLlmBackend
// Validates: Requirements 3.4

use std::sync::Arc;
use async_trait::async_trait;
use tokio::sync::mpsc;

use nova_core::llm_backend::{
    CompletionRequest, CompletionResponse, ContentBlock, LlmBackend,
    StreamDelta, TokenUsage,
};
use nova_memory::sidequery::SideQuery;

/// Mock LLM backend for unit testing.
struct MockLlmBackend {
    response_text: String,
}

impl MockLlmBackend {
    fn new(response_text: &str) -> Self {
        Self {
            response_text: response_text.to_string(),
        }
    }
}

#[async_trait]
impl LlmBackend for MockLlmBackend {
    async fn complete(&self, _req: &CompletionRequest) -> anyhow::Result<CompletionResponse> {
        Ok(CompletionResponse {
            id: "mock-id".into(),
            content: vec![ContentBlock::Text {
                text: self.response_text.clone(),
            }],
            stop_reason: Some("end_turn".into()),
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
            },
        })
    }

    async fn stream(
        &self,
        _req: &CompletionRequest,
        _tx: mpsc::Sender<StreamDelta>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Mock backend that returns an error to test error propagation.
struct ErrorMockBackend;

#[async_trait]
impl LlmBackend for ErrorMockBackend {
    async fn complete(&self, _req: &CompletionRequest) -> anyhow::Result<CompletionResponse> {
        Err(anyhow::anyhow!("mock error: connection refused"))
    }

    async fn stream(
        &self,
        _req: &CompletionRequest,
        _tx: mpsc::Sender<StreamDelta>,
    ) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("mock error: stream failed"))
    }
}

#[tokio::test]
async fn sidequery_returns_expected_result_through_mock() {
    let backend = Arc::new(MockLlmBackend::new("Hello from mock!"));
    let sq = SideQuery::new(backend, "test-model".to_string());

    let result = sq.query_await("system prompt", "user prompt").await.unwrap();
    assert_eq!(result, "Hello from mock!");
}

#[tokio::test]
async fn sidequery_query_background_returns_result() {
    let backend = Arc::new(MockLlmBackend::new("background result"));
    let sq = SideQuery::new(backend, "test-model".to_string());

    let (rx, handle) = sq.query("system", "prompt");
    handle.await.unwrap();
    let result = rx.await.unwrap().unwrap();
    assert_eq!(result, "background result");
}

#[tokio::test]
async fn sidequery_propagates_backend_error() {
    let backend = Arc::new(ErrorMockBackend);
    let sq = SideQuery::new(backend, "test-model".to_string());

    let result = sq.query_await("system", "prompt").await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("mock error"));
}

#[tokio::test]
async fn sidequery_handles_empty_response() {
    let backend = Arc::new(MockLlmBackend::new(""));
    let sq = SideQuery::new(backend, "test-model".to_string());

    let result = sq.query_await("system", "prompt").await.unwrap();
    assert_eq!(result, "");
}
