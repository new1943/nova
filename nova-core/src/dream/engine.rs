use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::info;

use crate::sidequery::SideQuery;

/// Strategy 12: autoDream — background exploration when user is idle.
///
/// - Triggers after 30s of user inactivity
/// - Independent 8K token budget (via SideQuery)
/// - Results sent as suggestions to TUI
/// - Must be enabled by user (/dream command)
pub struct DreamEngine {
    enabled: Arc<AtomicBool>,
    idle_threshold: Duration,
    last_activity: Arc<std::sync::Mutex<Instant>>,
    side_query: SideQuery,
}

/// Dream suggestion emitted to TUI
#[derive(Debug, Clone)]
pub struct DreamSuggestion {
    pub content: String,
}

impl DreamEngine {
    pub fn new(api_key: String, api_base_url: String, model: String) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(false)),
            idle_threshold: Duration::from_secs(30),
            last_activity: Arc::new(std::sync::Mutex::new(Instant::now())),
            side_query: SideQuery::new(api_key, api_base_url, model),
        }
    }

    /// Enable/disable dreaming
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Record user activity (resets idle timer)
    pub fn touch(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
    }

    /// Check if user has been idle long enough
    fn is_idle(&self) -> bool {
        let last = *self.last_activity.lock().unwrap();
        last.elapsed() >= self.idle_threshold
    }

    /// Start the dream loop in background. Sends suggestions via channel.
    pub fn start(
        self: Arc<Self>,
        context_summary: String,
        tx: mpsc::Sender<DreamSuggestion>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;

                if !self.is_enabled() || !self.is_idle() {
                    continue;
                }

                info!("autoDream triggered");

                let prompt = format!(
                    "Based on this conversation context, suggest one interesting follow-up question or exploration:\n\n{}",
                    context_summary
                );

                match self.side_query.query_await(
                    "You are a creative assistant. Suggest brief, insightful follow-up ideas in Chinese. Keep it under 50 characters.",
                    &prompt,
                ).await {
                    Ok(suggestion) if !suggestion.is_empty() => {
                        if tx.send(DreamSuggestion { content: suggestion }).await.is_err() {
                            break;
                        }
                    }
                    _ => {}
                }

                // Don't dream again for a while
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        })
    }
}
