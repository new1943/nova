use anyhow::Result;
use std::future::Future;
use std::time::Duration;
use tracing::warn;

/// Retry policy for API calls and tool executions
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_retries: usize,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub backoff_multiplier: f64,
    /// Retry on these error substrings
    pub retryable_errors: Vec<String>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            retryable_errors: vec![
                "rate_limit".into(),
                "overloaded".into(),
                "timeout".into(),
                "connection".into(),
                "503".into(),
                "529".into(),
            ],
        }
    }
}

impl RetryPolicy {
    /// Execute an async operation with retry + exponential backoff
    pub async fn execute<F, Fut, T>(&self, operation: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let mut delay = self.initial_delay;

        for attempt in 0..=self.max_retries {
            match operation().await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    let err_str = e.to_string().to_lowercase();
                    let is_retryable = self.retryable_errors.iter()
                        .any(|r| err_str.contains(r));

                    if attempt >= self.max_retries || !is_retryable {
                        return Err(e);
                    }

                    warn!(
                        "Retry {}/{}: {} (waiting {:?})",
                        attempt + 1, self.max_retries, e, delay
                    );

                    tokio::time::sleep(delay).await;
                    delay = Duration::from_secs_f64(
                        (delay.as_secs_f64() * self.backoff_multiplier).min(self.max_delay.as_secs_f64())
                    );
                }
            }
        }

        unreachable!()
    }
}
