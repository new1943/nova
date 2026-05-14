//! Task registry — tracks running executor tasks for /stop and /tasks commands.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

/// Global singleton TaskRegistry shared across IPC and Discord paths.
static GLOBAL_REGISTRY: LazyLock<TaskRegistry> = LazyLock::new(TaskRegistry::new);

/// Information about a running task.
#[derive(Debug, Clone)]
pub struct RunningTask {
    pub id: String,
    pub name: String,
    pub tool: String,
    pub started_at: std::time::Instant,
}

/// Global task registry — tracks all running executor tasks.
///
/// Thread-safe via RwLock. Tasks register on spawn and deregister on completion.
/// `/stop` iterates all entries and aborts their JoinHandles.
/// `/tasks` reads the list for display.
#[derive(Clone)]
pub struct TaskRegistry {
    inner: Arc<RwLock<TaskRegistryInner>>,
}

struct TaskRegistryInner {
    tasks: HashMap<String, (RunningTask, tokio::task::AbortHandle)>,
}

impl TaskRegistry {
    /// Returns the global singleton TaskRegistry.
    pub fn global() -> &'static Self {
        &GLOBAL_REGISTRY
    }

    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(TaskRegistryInner {
                tasks: HashMap::new(),
            })),
        }
    }

    /// Register a running task. Returns the task ID.
    pub async fn register(
        &self,
        id: String,
        name: String,
        tool: String,
        handle: JoinHandle<()>,
    ) {
        let task = RunningTask {
            id: id.clone(),
            name,
            tool,
            started_at: std::time::Instant::now(),
        };
        let abort_handle = handle.abort_handle();
        self.inner.write().await.tasks.insert(id, (task, abort_handle));
    }

    /// Deregister a task (called when it completes naturally).
    pub async fn deregister(&self, id: &str) {
        self.inner.write().await.tasks.remove(id);
    }

    /// Abort all running tasks. Returns the count of tasks aborted.
    pub async fn stop_all(&self) -> usize {
        let mut inner = self.inner.write().await;
        let count = inner.tasks.len();
        for (_, (_, handle)) in inner.tasks.drain() {
            handle.abort();
        }
        count
    }

    /// Abort a specific task by ID. Returns true if found and aborted.
    pub async fn stop_by_id(&self, id: &str) -> bool {
        let mut inner = self.inner.write().await;
        if let Some((_, handle)) = inner.tasks.remove(id) {
            handle.abort();
            true
        } else {
            false
        }
    }

    /// List all running tasks.
    pub async fn list(&self) -> Vec<RunningTask> {
        self.inner
            .read()
            .await
            .tasks
            .values()
            .map(|(task, _)| task.clone())
            .collect()
    }

    /// Number of running tasks.
    pub async fn count(&self) -> usize {
        self.inner.read().await.tasks.len()
    }
}

impl Default for TaskRegistry {
    fn default() -> Self {
        Self::new()
    }
}
