//! Shared preflight types — promoted to nova-core because `TurnContext` directly references them.
//!
//! Only type definitions live here. The `PreFlightChecker` implementation logic
//! will reside in nova-agent.

/// Pre-flight check result — returned from the lightweight classification LLM call.
#[derive(Debug, Clone, Default)]
pub struct PreFlightCheckResult {
    /// Chain-of-thought reasoning from the classifier
    pub thinking: String,
    /// True if the user's message indicates a topic shift (new topic, context switch)
    pub topic_shift: bool,
    /// Task complexity level — drives tool interception
    pub complexity: Complexity,
    /// Brief reason for the classification
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Complexity {
    #[default]
    Low,
    Medium,
    High,
}

impl Complexity {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "high" => Complexity::High,
            "medium" => Complexity::Medium,
            _ => Complexity::Low,
        }
    }
}
