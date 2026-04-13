pub mod post_sampling;
pub mod stop;

use anyhow::Result;
use async_trait::async_trait;
use tracing::{error, info};

use crate::message::Message;
use crate::session::manager::Session;

/// Base trait for all hooks
pub trait Hook: Send + Sync {
    fn name(&self) -> &str;
    fn enabled(&self) -> bool { true }
}

/// Strategy 5: PostSampling Hook — runs after each LLM response (forked, non-blocking)
#[async_trait]
pub trait PostSamplingHook: Hook {
    async fn run(&self, response: &Message, session: &Session) -> Result<()>;
}

/// Strategy 6: StopHook — runs after turn ends (serial, blocking)
#[async_trait]
pub trait StopHook: Hook {
    async fn run(&self, session: &mut Session) -> Result<()>;
}

/// Hook manager: registers and fires hooks
pub struct HookManager {
    post_sampling: Vec<Box<dyn PostSamplingHook>>,
    stop: Vec<Box<dyn StopHook>>,
}

impl Default for HookManager {
    fn default() -> Self { Self::new() }
}

impl HookManager {
    pub fn new() -> Self {
        Self {
            post_sampling: Vec::new(),
            stop: Vec::new(),
        }
    }

    pub fn register_post_sampling(&mut self, hook: Box<dyn PostSamplingHook>) {
        self.post_sampling.push(hook);
    }

    pub fn register_stop(&mut self, hook: Box<dyn StopHook>) {
        self.stop.push(hook);
    }

    /// Fire all PostSampling hooks (async).
    /// In production, these run in forked agents. Here we run them sequentially.
    /// Errors are logged but don't propagate.
    pub async fn fire_post_sampling(&self, response: &Message, session: &Session) {
        for hook in &self.post_sampling {
            if !hook.enabled() {
                continue;
            }
            let name = hook.name().to_string();
            info!("Firing PostSampling hook: {}", name);
            if let Err(e) = hook.run(response, session).await {
                error!("PostSampling hook '{}' failed: {}", name, e);
            }
        }
    }

    /// Fire all StopHooks serially (blocking the main loop).
    /// Errors are logged but don't stop other hooks.
    pub async fn fire_stop(&self, session: &mut Session) {
        for hook in &self.stop {
            if !hook.enabled() {
                continue;
            }
            let name = hook.name().to_string();
            info!("Firing StopHook: {}", name);
            if let Err(e) = hook.run(session).await {
                error!("StopHook '{}' failed: {}", name, e);
            }
        }
    }
}
