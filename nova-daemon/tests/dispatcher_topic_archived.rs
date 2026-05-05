//! Integration tests for Dispatcher → MemoryKeeper routing.
//!
//! Task 6.1: TopicArchived routes to MemoryKeeper
//! Task 6.2: Graceful degradation without MemoryKeeper (no panic)

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use nova_core::llm_backend::{
    CompletionRequest, CompletionResponse, ContentBlock, LlmBackend,
    StreamDelta, TokenUsage,
};
use nova_core::message::Message;
use nova_core::models::ShadowEvent;
use nova_daemon::dispatcher::Dispatcher;
use nova_memory::sidequery::{MemoryKeeper, SideQuery};

/// Mock LLM backend that returns a fixed response (for SideQuery in MemoryKeeper).
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

/// Task 6.1: TopicArchived event is correctly routed to MemoryKeeper,
/// causing its buffer to grow.
#[tokio::test]
async fn test_topic_archived_routes_to_memory_keeper() {
    let temp_dir = tempfile::tempdir().unwrap();
    let mock_backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new("- mock memory"));
    let mock_side_query = SideQuery::new(mock_backend, "test-model".to_string());
    let memory_keeper = Arc::new(MemoryKeeper::new(
        temp_dir.path().to_path_buf(),
        mock_side_query,
    ));

    let dispatcher = Dispatcher::new(temp_dir.path().to_path_buf())
        .with_memory_keeper(memory_keeper.clone());
    let sender = dispatcher.spawn();

    // Send TopicArchived event
    let transcript = vec![
        Message::user("hello"),
        Message::assistant(Some("hi there".to_string()), None),
    ];
    sender.emit(ShadowEvent::TopicArchived {
        transcript: transcript.clone(),
    });

    // Wait for async processing through the dispatcher loop
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify MemoryKeeper received the transcript (buffer grew by 1)
    assert_eq!(memory_keeper.buffer_size().await, 1);
}

/// Task 6.1 (additional): Multiple TopicArchived events accumulate in buffer.
#[tokio::test]
async fn test_multiple_topic_archived_accumulate() {
    let temp_dir = tempfile::tempdir().unwrap();
    let mock_backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new("- memory entry"));
    let mock_side_query = SideQuery::new(mock_backend, "test-model".to_string());
    let memory_keeper = Arc::new(MemoryKeeper::new(
        temp_dir.path().to_path_buf(),
        mock_side_query,
    ));

    let dispatcher = Dispatcher::new(temp_dir.path().to_path_buf())
        .with_memory_keeper(memory_keeper.clone());
    let sender = dispatcher.spawn();

    // Send multiple TopicArchived events
    for i in 0..3 {
        sender.emit(ShadowEvent::TopicArchived {
            transcript: vec![Message::user(format!("message {}", i))],
        });
    }

    // Wait for async processing
    tokio::time::sleep(Duration::from_millis(200)).await;

    // All 3 topics should be buffered
    assert_eq!(memory_keeper.buffer_size().await, 3);
}

/// Task 6.2: Sending TopicArchived without a configured MemoryKeeper
/// should NOT panic — the event is gracefully dropped.
#[tokio::test]
async fn test_topic_archived_without_memory_keeper_no_panic() {
    let temp_dir = tempfile::tempdir().unwrap();

    // Create Dispatcher WITHOUT MemoryKeeper
    let dispatcher = Dispatcher::new(temp_dir.path().to_path_buf());
    let sender = dispatcher.spawn();

    // Send event — should not panic
    sender.emit(ShadowEvent::TopicArchived {
        transcript: vec![Message::user("test message")],
    });

    // Wait to ensure the dispatcher loop processes the event
    tokio::time::sleep(Duration::from_millis(100)).await;

    // If we reach here without panic, the test passes.
    // Send another event to confirm the dispatcher loop is still alive
    sender.emit(ShadowEvent::TopicArchived {
        transcript: vec![
            Message::user("second message"),
            Message::assistant(Some("response".to_string()), None),
        ],
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    // No panic — graceful degradation confirmed
}
