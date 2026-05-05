//! Tension Tracker — tracks user emotional state and conversation tension.
//!
//! Calculates tension value (0-100) based on user state and session context.

use chrono::{Local, Timelike};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// User's long-term emotional state
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserState {
    /// Intimacy level (0-100)
    pub intimacy: u8,
    /// Trust level (0-100)
    pub trust: u8,
    /// Dependency level (0-100)
    pub dependency: u8,
    /// Last interaction gap in minutes
    pub last_gap_minutes: u32,
    /// Historical emotion tags
    pub history_tags: Vec<String>,
}

impl UserState {
    /// Create a new user state with defaults
    pub fn new() -> Self {
        Self::default()
    }

    /// Update intimacy
    pub fn set_intimacy(&mut self, value: u8) {
        self.intimacy = value.min(100);
    }

    /// Update trust
    pub fn set_trust(&mut self, value: u8) {
        self.trust = value.min(100);
    }

    /// Update dependency
    pub fn set_dependency(&mut self, value: u8) {
        self.dependency = value.min(100);
    }

    /// Record an interaction
    pub fn record_interaction(&mut self) {
        self.last_gap_minutes = 0;
    }

    /// Add time to the gap
    pub fn add_time(&mut self, minutes: u32) {
        self.last_gap_minutes += minutes;
    }
}

/// Current session's short-term state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    /// Current emotion
    pub emotion: Emotion,
    /// Current context (time of day)
    pub context: Context,
    /// User's intent in this message
    pub user_intent: UserIntent,
    /// Calculated tension value (0-100)
    pub tension: u8,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            emotion: Emotion::Calm,
            context: Context::Afternoon,
            user_intent: UserIntent::Task,
            tension: 0,
        }
    }
}

/// Emotion enum
#[derive(Debug, Clone, PartialEq, Copy, Serialize, Deserialize)]
pub enum Emotion {
    Calm,
    Excited,
    Anxious,
    Frustrated,
    Tense,
}

impl Emotion {
    /// Get weight for tension calculation
    fn weight(&self) -> f32 {
        match self {
            Emotion::Calm => 0.0,
            Emotion::Excited => 15.0,
            Emotion::Anxious => 25.0,
            Emotion::Frustrated => 30.0,
            Emotion::Tense => 35.0,
        }
    }
}

/// Context (time of day)
#[derive(Debug, Clone, PartialEq, Copy, Serialize, Deserialize)]
pub enum Context {
    Morning,
    Afternoon,
    Evening,
    Night,
    Working,
}

impl Context {
    /// Get weight for tension calculation
    fn weight(&self) -> f32 {
        match self {
            Context::Morning => 5.0,
            Context::Afternoon => 10.0,
            Context::Evening => 15.0,
            Context::Night => 25.0,
            Context::Working => 20.0,
        }
    }
}

/// User intent
#[derive(Debug, Clone, PartialEq, Copy, Serialize, Deserialize)]
pub enum UserIntent {
    Task,
    Casual,
    Seeking,
    Emotional,
}

impl UserIntent {
    /// Check if this intent suggests high tension
    fn tension_boost(&self) -> u8 {
        match self {
            UserIntent::Task => 0,
            UserIntent::Casual => 5,
            UserIntent::Seeking => 10,
            UserIntent::Emotional => 20,
        }
    }
}

/// Tension calculator
pub struct TensionCalculator;

impl TensionCalculator {
    /// Calculate tension value (0-100) based on user and session state
    pub fn calculate(user: &UserState, session: &SessionState) -> u8 {
        // Weighted formula:
        // intimacy: 40%, trust: 20%, emotion: 20%, context: 20%
        let intimacy_weight = (user.intimacy as f32) * 0.4;
        let trust_weight = (user.trust as f32) * 0.2;
        let emotion_weight = session.emotion.weight();
        let context_weight = session.context.weight();

        // Add intent boost
        let intent_boost = session.user_intent.tension_boost() as f32;

        // Long gap penalty (> 30 min)
        let gap_penalty = if user.last_gap_minutes > 30 {
            15.0
        } else if user.last_gap_minutes > 60 {
            25.0
        } else {
            0.0
        };

        // Calculate raw value
        let raw = intimacy_weight + trust_weight + emotion_weight + context_weight + intent_boost - gap_penalty;

        // Clamp to 0-100
        raw.clamp(0.0, 100.0) as u8
    }
}

/// Tension tracker — manages user and session state
pub struct TensionTracker {
    /// User's long-term state
    user_state: RwLock<UserState>,
    /// Current session state
    session_state: RwLock<SessionState>,
}

impl TensionTracker {
    /// Create a new TensionTracker
    pub fn new() -> Self {
        Self {
            user_state: RwLock::new(UserState::new()),
            session_state: RwLock::new(SessionState::default()),
        }
    }

    /// Get a snapshot of user state
    pub async fn user_state(&self) -> UserState {
        self.user_state.read().await.clone()
    }

    /// Get a snapshot of session state
    pub async fn session_state(&self) -> SessionState {
        self.session_state.read().await.clone()
    }

    /// Get current tension value
    pub async fn current_tension(&self) -> u8 {
        let user = self.user_state.read().await;
        let session = self.session_state.read().await;
        TensionCalculator::calculate(&user, &session)
    }

    /// Update session state from user message
    pub async fn update_from_message(&self, content: &str) -> SessionState {
        let mut session = self.session_state.write().await;

        // Detect emotion
        session.emotion = Self::detect_emotion(content);

        // Detect context
        session.context = Self::detect_context();

        // Detect intent
        session.user_intent = Self::detect_intent(content);

        // Recalculate tension
        let user = self.user_state.read().await;
        session.tension = TensionCalculator::calculate(&user, &session);

        session.clone()
    }

    /// Record user interaction (resets gap timer)
    pub async fn record_interaction(&self) {
        self.user_state.write().await.record_interaction();
    }

    /// Add time to gap counter (call periodically)
    pub async fn add_time(&self, minutes: u32) {
        self.user_state.write().await.add_time(minutes);
    }

    /// Increase intimacy
    pub async fn increase_intimacy(&self, delta: u8) {
        let mut user = self.user_state.write().await;
        user.intimacy = (user.intimacy + delta).min(100);
    }

    /// Decrease intimacy
    pub async fn decrease_intimacy(&self, delta: u8) {
        let mut user = self.user_state.write().await;
        user.intimacy = user.intimacy.saturating_sub(delta);
    }

    /// Increase trust
    pub async fn increase_trust(&self, delta: u8) {
        let mut user = self.user_state.write().await;
        user.trust = (user.trust + delta).min(100);
    }

    /// Add history tag
    pub async fn add_tag(&self, tag: String) {
        let mut user = self.user_state.write().await;
        if !user.history_tags.contains(&tag) {
            user.history_tags.push(tag);
        }
    }

    /// Detect emotion from message content
    fn detect_emotion(content: &str) -> Emotion {
        let lower = content.to_lowercase();

        // Anxious indicators
        if lower.contains("焦虑") || lower.contains("担心") || lower.contains("害怕") {
            return Emotion::Anxious;
        }

        // Frustrated indicators
        if lower.contains("挫败") || lower.contains("不行") || lower.contains("失败")
            || lower.contains("没用") || lower.contains("不行") {
            return Emotion::Frustrated;
        }

        // Tense indicators
        if lower.contains("紧张") || lower.contains("压力大") || lower.contains("焦虑") {
            return Emotion::Tense;
        }

        // Excited indicators
        if lower.contains("开心") || lower.contains("太好了") || lower.contains("棒")
            || lower.contains("厉害") || lower.contains("太好了") {
            return Emotion::Excited;
        }

        Emotion::Calm
    }

    /// Detect context from current time
    fn detect_context() -> Context {
        let hour = Local::now().hour();

        if (5..12).contains(&hour) {
            Context::Morning
        } else if (12..14).contains(&hour) {
            Context::Afternoon
        } else if (14..18).contains(&hour) {
            Context::Working
        } else if (18..23).contains(&hour) {
            Context::Evening
        } else {
            Context::Night
        }
    }

    /// Detect user intent from message content
    fn detect_intent(content: &str) -> UserIntent {
        let lower = content.to_lowercase();

        // Seeking (questions)
        if lower.contains("？") || lower.contains("怎么") || lower.contains("什么")
            || lower.contains("为什么") || lower.contains("?") {
            return UserIntent::Seeking;
        }

        // Emotional
        if lower.contains("想") || lower.contains("感觉") || lower.contains("觉得")
            || lower.contains("希望") {
            return UserIntent::Emotional;
        }

        // Casual
        if lower.contains("你好") || lower.contains("嗨") || lower.contains("在")
            || lower.contains("在吗") {
            return UserIntent::Casual;
        }

        UserIntent::Task
    }
}

impl Default for TensionTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emotion_weights() {
        assert_eq!(Emotion::Calm.weight(), 0.0);
        assert_eq!(Emotion::Excited.weight(), 15.0);
        assert_eq!(Emotion::Anxious.weight(), 25.0);
        assert_eq!(Emotion::Frustrated.weight(), 30.0);
        assert_eq!(Emotion::Tense.weight(), 35.0);
    }

    #[test]
    fn test_context_weights() {
        assert_eq!(Context::Morning.weight(), 5.0);
        assert_eq!(Context::Afternoon.weight(), 10.0);
        assert_eq!(Context::Evening.weight(), 15.0);
        assert_eq!(Context::Night.weight(), 25.0);
        assert_eq!(Context::Working.weight(), 20.0);
    }

    #[tokio::test]
    async fn test_tension_calculation() {
        let tracker = TensionTracker::new();

        // With default state, tension should be low
        let tension = tracker.current_tension().await;
        assert!(tension < 50); // Default should be calm
    }

    #[tokio::test]
    async fn test_emotion_detection() {
        let tracker = TensionTracker::new();

        // Anxious message
        let state = tracker.update_from_message("我有点担心这个事情").await;
        assert_eq!(state.emotion, Emotion::Anxious);

        // Frustrated message
        let state = tracker.update_from_message("这样不行，完全失败了").await;
        assert_eq!(state.emotion, Emotion::Frustrated);

        // Excited message
        let state = tracker.update_from_message("太好了！非常棒！").await;
        assert_eq!(state.emotion, Emotion::Excited);

        // Neutral message
        let state = tracker.update_from_message("帮我看看这个代码").await;
        assert_eq!(state.emotion, Emotion::Calm);
    }

    #[tokio::test]
    async fn test_intent_detection() {
        let tracker = TensionTracker::new();

        // Seeking
        let state = tracker.update_from_message("这个怎么做？").await;
        assert_eq!(state.user_intent, UserIntent::Seeking);

        // Emotional
        let state = tracker.update_from_message("我感觉不太好").await;
        assert_eq!(state.user_intent, UserIntent::Emotional);

        // Task
        let state = tracker.update_from_message("帮我写个函数").await;
        assert_eq!(state.user_intent, UserIntent::Task);

        // Casual
        let state = tracker.update_from_message("你好，在吗").await;
        assert_eq!(state.user_intent, UserIntent::Casual);
    }
}
