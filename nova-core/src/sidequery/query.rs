use anyhow::Result;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::info;

use nova_api::client::ApiClient;
use nova_api::types::{ApiMessage, ApiRequest, Content, ContentBlock};

/// Strategy 11: SideQuery — independent API query that doesn't enter the main loop.
/// - Does not count as a turn
/// - Independent 8K token budget
/// - Result returned via channel
#[derive(Clone)]
pub struct SideQuery {
    api_key: String,
    api_base_url: String,
    model: String,
}

impl SideQuery {
    pub fn new(api_key: String, api_base_url: String, model: String) -> Self {
        Self { api_key, api_base_url, model }
    }

    /// Execute a side query in the background.
    /// Returns a oneshot receiver for the result.
    pub fn query(&self, system: &str, prompt: &str) -> (oneshot::Receiver<Result<String>>, JoinHandle<()>) {
        let (tx, rx) = oneshot::channel();
        let api_key = self.api_key.clone();
        let api_base_url = self.api_base_url.clone();
        let model = self.model.clone();
        let system = system.to_string();
        let prompt = prompt.to_string();

        let handle = tokio::spawn(async move {
            info!("SideQuery started");
            let result = Self::do_query(api_key, api_base_url, model, &system, &prompt).await;
            let _ = tx.send(result);
        });

        (rx, handle)
    }

    /// Blocking version — await the result directly
    pub async fn query_await(&self, system: &str, prompt: &str) -> Result<String> {
        Self::do_query(
            self.api_key.clone(),
            self.api_base_url.clone(),
            self.model.clone(),
            system,
            prompt,
        ).await
    }

    async fn do_query(
        api_key: String,
        api_base_url: String,
        model: String,
        system: &str,
        prompt: &str,
    ) -> Result<String> {
        let api = ApiClient::new(api_key, api_base_url);

        let req = ApiRequest {
            model,
            max_tokens: 2048,
            system: system.to_string(),
            messages: vec![ApiMessage::User {
                content: Content::Text(prompt.to_string()),
            }],
            tools: vec![],
            stream: false,
        };

        let resp = api.complete(&req).await?;

        let mut result = String::new();
        for block in &resp.content {
            if let ContentBlock::Text { text } = block {
                result.push_str(text);
            }
        }
        Ok(result)
    }
}
