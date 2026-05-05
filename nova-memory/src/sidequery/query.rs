use std::sync::Arc;
use anyhow::Result;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::info;

use nova_core::llm_backend::{
    LlmBackend, CompletionRequest, CompletionMessage,
    CompletionContent, ContentBlock,
};

/// Strategy 11: SideQuery — independent API query that doesn't enter the main loop.
/// - Does not count as a turn
/// - Independent 8K token budget
/// - Result returned via channel
#[derive(Clone)]
pub struct SideQuery {
    backend: Arc<dyn LlmBackend>,
    model: String,
}

impl SideQuery {
    pub fn new(backend: Arc<dyn LlmBackend>, model: String) -> Self {
        Self { backend, model }
    }

    /// Execute a side query in the background.
    /// Returns a oneshot receiver for the result.
    pub fn query(&self, system: &str, prompt: &str) -> (oneshot::Receiver<Result<String>>, JoinHandle<()>) {
        let (tx, rx) = oneshot::channel();
        let backend = self.backend.clone();
        let model = self.model.clone();
        let system = system.to_string();
        let prompt = prompt.to_string();

        let handle = tokio::spawn(async move {
            info!("SideQuery started");
            let result = Self::do_query(&backend, &model, &system, &prompt).await;
            let _ = tx.send(result);
        });

        (rx, handle)
    }

    /// Blocking version — await the result directly
    pub async fn query_await(&self, system: &str, prompt: &str) -> Result<String> {
        Self::do_query(&self.backend, &self.model, system, prompt).await
    }

    async fn do_query(
        backend: &Arc<dyn LlmBackend>,
        model: &str,
        system: &str,
        prompt: &str,
    ) -> Result<String> {
        let req = CompletionRequest {
            model: model.to_string(),
            max_tokens: 2048,
            system: system.to_string(),
            messages: vec![CompletionMessage::User {
                content: CompletionContent::Text(prompt.to_string()),
            }],
            tools: vec![],
            stream: false,
        };

        let resp = backend.complete(&req).await?;

        let mut result = String::new();
        for block in &resp.content {
            if let ContentBlock::Text { text } = block {
                result.push_str(text);
            }
        }
        Ok(result)
    }
}
