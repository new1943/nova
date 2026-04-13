use anyhow::Result;
use tokio::task::JoinHandle;
use tracing::info;

use nova_api::client::ApiClient;
use nova_api::types::{ApiMessage, ApiRequest, Content, ContentBlock};

/// Subagent capability level
#[derive(Debug, Clone)]
pub enum SubagentType {
    /// Read-only: can only research/search/plan
    ReadOnly,
    /// Full capability: can read/write files, execute commands
    Full,
}

/// Configuration for spawning a subagent
#[derive(Debug, Clone)]
pub struct SubagentConfig {
    pub name: String,
    pub agent_type: SubagentType,
    pub team_name: Option<String>,
    pub system_prompt: String,
    pub api_key: String,
    pub api_base_url: String,
    pub model: String,
    pub max_input_tokens: usize,
}

impl SubagentConfig {
    pub fn new(name: impl Into<String>, agent_type: SubagentType) -> Self {
        Self {
            name: name.into(),
            agent_type,
            team_name: None,
            system_prompt: String::new(),
            api_key: String::new(),
            api_base_url: "https://api.minimaxi.com/anthropic".into(),
            model: "MiniMax-M2.7".into(),
            max_input_tokens: 16_000,
        }
    }
}

/// Handle to a running subagent
pub struct SubagentHandle {
    pub name: String,
    pub join: JoinHandle<Result<String>>,
}

/// Subagent spawner — creates background agents
pub struct SubagentSpawner;

impl SubagentSpawner {
    /// Spawn a subagent that executes a single task and returns the result.
    /// Runs in background via tokio::spawn.
    pub fn spawn(config: SubagentConfig, task: String) -> SubagentHandle {
        let name = config.name.clone();
        let join = tokio::spawn(async move {
            info!("Subagent '{}' started: {}", config.name, task);

            let api = ApiClient::new(config.api_key, config.api_base_url);

            let mut prompt = config.system_prompt;
            match config.agent_type {
                SubagentType::ReadOnly => {
                    prompt.push_str("\n\nYou are a read-only agent. You can only research, search, and plan. Do not modify any files.");
                }
                SubagentType::Full => {}
            }

            let req = ApiRequest {
                model: config.model,
                max_tokens: 4096,
                system: prompt,
                messages: vec![ApiMessage::User {
                    content: Content::Text(task),
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

            info!("Subagent '{}' completed ({} chars)", config.name, result.len());
            Ok(result)
        });

        SubagentHandle { name, join }
    }

    /// Spawn multiple subagents in parallel, collect results
    pub async fn spawn_parallel(
        configs: Vec<(SubagentConfig, String)>,
    ) -> Vec<(String, Result<String>)> {
        let handles: Vec<SubagentHandle> = configs
            .into_iter()
            .map(|(cfg, task)| Self::spawn(cfg, task))
            .collect();

        let mut results = Vec::new();
        for handle in handles {
            let name = handle.name;
            let result = handle.join.await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("Join error: {}", e)));
            results.push((name, result));
        }
        results
    }
}
