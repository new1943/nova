use anyhow::Result;
use std::sync::Arc;
use tracing::info;

use crate::subagent::{SubagentConfig, SubagentSpawner, SubagentType};
use crate::tools::registry::ToolRegistry;

/// Four-phase orchestration pipeline
#[derive(Debug, Clone, PartialEq)]
pub enum CoordinatorPhase {
    Research,
    Synthesis,
    Implementation,
    Verification,
}

impl CoordinatorPhase {
    fn next(&self) -> Option<Self> {
        match self {
            Self::Research => Some(Self::Synthesis),
            Self::Synthesis => Some(Self::Implementation),
            Self::Implementation => Some(Self::Verification),
            Self::Verification => None,
        }
    }
}

/// Strategy 14: Coordinator — parent AI that orchestrates worker subagents
/// through four phases: Research → Synthesis → Implementation → Verification
pub struct Coordinator {
    api_key: String,
    api_base_url: String,
    model: String,
    system_prompt: String,
    /// [V4 Fix] Channel for SubAgent TaskProgress events → Dispatcher → TaskManager
    shadow_tx: Option<tokio::sync::mpsc::Sender<crate::models::ShadowEvent>>,
    /// [V4 Fix] ToolRegistry for SubAgent tool execution
    tools: Option<Arc<ToolRegistry>>,
}

impl Coordinator {
    pub fn new(
        api_key: String,
        api_base_url: String,
        model: String,
        system_prompt: String,
        shadow_tx: Option<tokio::sync::mpsc::Sender<crate::models::ShadowEvent>>,
        tools: Option<Arc<ToolRegistry>>,
    ) -> Self {
        Self { api_key, api_base_url, model, system_prompt, shadow_tx, tools }
    }

    /// Run the full four-phase pipeline for a task
    pub async fn orchestrate(&self, task: &str) -> Result<CoordinatorResult> {
        let mut context = String::new();
        let mut phase = CoordinatorPhase::Research;

        loop {
            info!("Coordinator phase: {:?}", phase);

            let phase_prompt = match phase {
                CoordinatorPhase::Research => {
                    format!("Research phase. Task: {}\nGather information and analyze requirements.", task)
                }
                CoordinatorPhase::Synthesis => {
                    format!("Synthesis phase. Task: {}\nPrevious research:\n{}\nSynthesize findings into a plan.", task, context)
                }
                CoordinatorPhase::Implementation => {
                    format!("Implementation phase. Task: {}\nPlan:\n{}\nImplement the solution.", task, context)
                }
                CoordinatorPhase::Verification => {
                    format!("Verification phase. Task: {}\nImplementation:\n{}\nVerify correctness.", task, context)
                }
            };

            let agent_type = match phase {
                CoordinatorPhase::Research | CoordinatorPhase::Verification => SubagentType::ReadOnly,
                CoordinatorPhase::Synthesis | CoordinatorPhase::Implementation => SubagentType::Full,
            };

            let config = SubagentConfig {
                name: format!("coordinator-{:?}", phase).to_lowercase(),
                agent_type,
                team_name: None,
                system_prompt: self.system_prompt.clone(),
                api_key: self.api_key.clone(),
                api_base_url: self.api_base_url.clone(),
                model: self.model.clone(),
                max_input_tokens: 16_000,
                shadow_tx: self.shadow_tx.clone(), // [V4 Fix] Wired to ShadowEvent bus
                tools: self.tools.clone(), // [V4 Fix] Enable tools for SubAgent
            };

            let handle = SubagentSpawner::spawn(config, phase_prompt);
            let result = handle.join.await??;
            context = result;

            match phase.next() {
                Some(next) => phase = next,
                None => break,
            }
        }

        Ok(CoordinatorResult {
            output: context,
        })
    }
}

pub struct CoordinatorResult {
    pub output: String,
}
