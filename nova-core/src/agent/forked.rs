use anyhow::Result;
use std::future::Future;
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::warn;

/// Forked Agent — Strategy 4: background tasks via tokio::spawn
///
/// - Independent 16K input token budget
/// - Max 2 retries on failure
/// - Does not block main query loop
pub struct ForkedAgent;

impl ForkedAgent {
    /// Spawn a background task that can be retried.
    /// `task_fn` is called up to `max_retries + 1` times.
    pub fn spawn_with_retry<F, Fut>(
        max_retries: usize,
        task_fn: F,
    ) -> JoinHandle<Result<String>>
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String>> + Send,
    {
        tokio::spawn(async move {
            let mut last_err = None;
            for attempt in 0..=max_retries {
                match task_fn().await {
                    Ok(result) => return Ok(result),
                    Err(e) => {
                        warn!("Forked agent attempt {} failed: {}", attempt + 1, e);
                        last_err = Some(e);
                        if attempt < max_retries {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
            }
            Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Forked agent failed")))
        })
    }

    /// Spawn a simple one-shot background task (no retry).
    pub fn spawn<F>(task: F) -> JoinHandle<Result<()>>
    where
        F: Future<Output = Result<()>> + Send + 'static,
    {
        tokio::spawn(task)
    }
}
