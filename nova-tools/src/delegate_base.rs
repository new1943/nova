//! delegate_base — shared logic for delegate tools.
//!
//! Provides the RUNNING_PROJECTS registry and common lifecycle management
//! for delegated tasks. The actual delegate tools (delegate_task, delegate_complex_project)
//! live in nova-agent since they depend on Coordinator/SubagentSpawner.

use std::collections::HashMap;
use std::sync::Arc;
use once_cell::sync::Lazy;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::AbortHandle;

use nova_core::models::ShadowEventEmitter;
use crate::task::TaskLogger;
use crate::registry::ToolRegistry;

/// Global registry of running background projects (shared between delegate tools)
pub static RUNNING_PROJECTS: Lazy<AsyncMutex<HashMap<String, AbortHandle>>> =
    Lazy::new(|| AsyncMutex::new(HashMap::new()));

/// Configuration for a delegated task
pub struct DelegateConfig {
    pub emitter: Arc<dyn ShadowEventEmitter>,
    pub shadow_tx: tokio::sync::mpsc::Sender<nova_core::models::ShadowEvent>,
    pub api_key: String,
    pub api_base_url: String,
    pub model: String,
    pub system_prompt: String,
    pub tools: Option<Arc<ToolRegistry>>,
    pub workspace_dir: Option<std::path::PathBuf>,
}

/// Spawn a delegated task with common lifecycle management.
///
/// Returns the generated project_id.
pub async fn spawn_delegated_task(
    config: DelegateConfig,
    task_description: &str,
    _channel_id: String,
) -> String {
    let project_id = uuid::Uuid::new_v4().to_string();

    // Log task start
    if let Some(ref ws) = config.workspace_dir {
        let _ = TaskLogger::append_task(ws, &project_id, task_description).await;
    }

    project_id
}

/// Cancel a running delegated task by project_id.
///
/// Returns Ok(true) if the task was found and cancelled, Ok(false) if not found.
pub async fn cancel_delegated_task(project_id: &str) -> bool {
    let mut registry = RUNNING_PROJECTS.lock().await;
    if let Some(handle) = registry.remove(project_id) {
        handle.abort();
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_running_projects_registry() {
        let registry = RUNNING_PROJECTS.lock().await;
        assert!(registry.is_empty());
    }

    #[tokio::test]
    async fn test_cancel_nonexistent_task() {
        let result = cancel_delegated_task("nonexistent-id").await;
        assert!(!result);
    }
}
