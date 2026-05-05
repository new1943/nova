// Feature: r3-open-connectivity, Property 2: SideQuery 通过 LlmBackend 委托调用
// **Validates: Requirements 3.2, 3.3**

use std::sync::Arc;
use async_trait::async_trait;
use proptest::prelude::*;
use tokio::sync::mpsc;

use nova_core::llm_backend::{
    CompletionRequest, CompletionResponse, ContentBlock, LlmBackend,
    StreamDelta, TokenUsage,
};
use nova_memory::sidequery::SideQuery;

/// Mock LLM backend that echoes back a configurable response text.
/// Used to verify SideQuery correctly delegates to LlmBackend::complete()
/// and extracts text from the response.
struct MockLlmBackend {
    response_text: String,
}

impl MockLlmBackend {
    fn new(response_text: String) -> Self {
        Self { response_text }
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

/// Mock backend that returns multiple text blocks to verify concatenation.
struct MultiBlockMockBackend {
    blocks: Vec<String>,
}

impl MultiBlockMockBackend {
    fn new(blocks: Vec<String>) -> Self {
        Self { blocks }
    }
}

#[async_trait]
impl LlmBackend for MultiBlockMockBackend {
    async fn complete(&self, _req: &CompletionRequest) -> anyhow::Result<CompletionResponse> {
        let content = self.blocks.iter()
            .map(|text| ContentBlock::Text { text: text.clone() })
            .collect();
        Ok(CompletionResponse {
            id: "mock-multi".into(),
            content,
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

proptest! {
    /// Property 2: For any system prompt and user prompt, SideQuery::query_await()
    /// delegates to LlmBackend::complete() and returns the concatenation of all
    /// ContentBlock::Text blocks from the response.
    #[test]
    fn sidequery_delegates_and_extracts_text(
        system in "\\PC{1,100}",
        prompt in "\\PC{1,100}",
        response_text in "\\PC{0,200}",
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let backend = Arc::new(MockLlmBackend::new(response_text.clone()));
            let sq = SideQuery::new(backend, "test-model".to_string());

            let result = sq.query_await(&system, &prompt).await.unwrap();
            prop_assert_eq!(result, response_text);
            Ok(())
        })?;
    }

    /// Property 2 (multi-block): For any list of text blocks, SideQuery concatenates
    /// all ContentBlock::Text entries in order.
    #[test]
    fn sidequery_concatenates_multiple_text_blocks(
        system in "\\PC{1,50}",
        prompt in "\\PC{1,50}",
        blocks in prop::collection::vec("\\PC{0,50}", 1..5),
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let expected: String = blocks.to_vec().concat();
            let backend = Arc::new(MultiBlockMockBackend::new(blocks));
            let sq = SideQuery::new(backend, "test-model".to_string());

            let result = sq.query_await(&system, &prompt).await.unwrap();
            prop_assert_eq!(result, expected);
            Ok(())
        })?;
    }
}
