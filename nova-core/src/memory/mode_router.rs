//! Mode Router — decides Nova's interaction mode based on tension and state.
//!
//! Modes: Normal → SoftIntimate → HighIntimate → Cooling

use std::sync::Arc;
use tokio::sync::RwLock;

/// Interaction mode enum
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    /// Default mode — task-focused, concise
    Normal,
    /// Warm and gentle — slight care added
    SoftIntimate,
    /// High care mode — obvious warmth and concern
    HighIntimate,
    /// Cooling down — slightly distant, not overly warm
    Cooling,
}

impl Mode {
    /// Get description for this mode
    pub fn description(&self) -> &'static str {
        match self {
            Mode::Normal => "简洁直接，专注任务",
            Mode::SoftIntimate => "温和亲密，稍带关怀",
            Mode::HighIntimate => "高亲密，明显关怀模式",
            Mode::Cooling => "冷却模式，收敛热情",
        }
    }

    /// Get response length hint for this mode
    pub fn response_length(&self) -> &'static str {
        match self {
            Mode::Normal => "短句",
            Mode::SoftIntimate => "中句",
            Mode::HighIntimate => "中长句",
            Mode::Cooling => "短句",
        }
    }

    /// Get tone hint for this mode
    pub fn tone(&self) -> &'static str {
        match self {
            Mode::Normal => "简洁",
            Mode::SoftIntimate => "温和",
            Mode::HighIntimate => "关怀",
            Mode::Cooling => "收敛",
        }
    }
}

/// Decide mode based on tension value and last mode
pub fn decide_mode(tension: u8, last_mode: &Mode) -> Mode {
    // If we were cooling, stay cooling until fully recovered (tension < 50)
    if *last_mode == Mode::Cooling && tension >= 50 {
        return Mode::Cooling;
    }

    if tension < 50 {
        Mode::Normal
    } else if tension < 75 {
        Mode::SoftIntimate
    } else {
        // High tension (>= 75) and not cooling
        Mode::HighIntimate
    }
}

/// Mode router — manages current mode and processes messages
pub struct ModeRouter {
    tension_tracker: Arc<super::TensionTracker>,
    /// Current interaction mode
    current_mode: RwLock<Mode>,
    /// Previous mode (for transition detection)
    previous_mode: RwLock<Mode>,
    /// Consecutive high tension turns
    high_tension_turns: RwLock<u8>,
}

impl ModeRouter {
    /// Create a new ModeRouter
    pub fn new(tension_tracker: Arc<super::TensionTracker>) -> Self {
        Self {
            tension_tracker,
            current_mode: RwLock::new(Mode::Normal),
            previous_mode: RwLock::new(Mode::Normal),
            high_tension_turns: RwLock::new(0),
        }
    }

    /// Get current mode
    pub async fn current_mode(&self) -> Mode {
        self.current_mode.read().await.clone()
    }

    /// Get previous mode
    pub async fn previous_mode(&self) -> Mode {
        self.previous_mode.read().await.clone()
    }

    /// Check if mode changed
    pub async fn mode_changed(&self) -> bool {
        let current = self.current_mode.read().await;
        let previous = self.previous_mode.read().await;
        *current != *previous
    }

    /// Process user message and update mode
    /// Returns the new current mode
    pub async fn process(&self, message: &str) -> Mode {
        // Update tension tracker from message
        let session_state = self.tension_tracker.update_from_message(message).await;

        // Get current tension
        let tension = session_state.tension;

        // Get current mode (clone since Mode is not Copy)
        let current = self.current_mode.read().await.clone();

        // Decide new mode
        let new_mode = decide_mode(tension, &current);

        // Track consecutive high tension
        {
            let mut turns = self.high_tension_turns.write().await;
            if tension >= 75 {
                *turns += 1;
            } else {
                *turns = 0;
            }
        }

        // Update mode if changed
        if new_mode != current {
            let mut prev = self.previous_mode.write().await;
            *prev = current;
            let mut curr = self.current_mode.write().await;
            *curr = new_mode.clone();
        }

        new_mode
    }

    /// Force mode to Cooling (e.g., after high intimacy period)
    pub async fn cool_down(&self) {
        let mut prev = self.previous_mode.write().await;
        *prev = self.current_mode.read().await.clone();
        let mut curr = self.current_mode.write().await;
        *curr = Mode::Cooling;
    }

    /// Reset to Normal mode
    pub async fn reset(&self) {
        let mut prev = self.previous_mode.write().await;
        *prev = self.current_mode.read().await.clone();
        let mut curr = self.current_mode.write().await;
        *curr = Mode::Normal;
    }

    /// Get high tension turn count
    pub async fn high_tension_turns(&self) -> u8 {
        *self.high_tension_turns.read().await
    }

    /// Check if proactive mechanism should trigger (5-12% probability)
    pub fn should_proactive(&self, high_tension_turns: u8) -> bool {
        use std::time::{SystemTime, UNIX_EPOCH};
        // Use current time as random seed (pseudo-random)
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos() as u64;

        // 8% probability when conditions met
        let probability = if high_tension_turns > 3 { 12 } else { 5 };
        seed % 100 < probability
    }

    /// Get mode hints for prompt injection
    pub async fn get_mode_hints(&self) -> ModeHints {
        let mode = self.current_mode.read().await.clone();
        ModeHints {
            mode: mode.clone(),
            description: mode.description().to_string(),
            response_length: mode.response_length().to_string(),
            tone: mode.tone().to_string(),
            tension: self.tension_tracker.current_tension().await,
        }
    }
}

/// Mode hints for prompt injection
#[derive(Debug, Clone)]
pub struct ModeHints {
    pub mode: Mode,
    pub description: String,
    pub response_length: String,
    pub tone: String,
    pub tension: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mode_descriptions() {
        assert_eq!(Mode::Normal.description(), "简洁直接，专注任务");
        assert_eq!(Mode::SoftIntimate.description(), "温和亲密，稍带关怀");
        assert_eq!(Mode::HighIntimate.description(), "高亲密，明显关怀模式");
        assert_eq!(Mode::Cooling.description(), "冷却模式，收敛热情");
    }

    #[test]
    fn test_decide_mode_normal() {
        assert_eq!(decide_mode(30, &Mode::Normal), Mode::Normal);
        assert_eq!(decide_mode(49, &Mode::Normal), Mode::Normal);
    }

    #[test]
    fn test_decide_mode_soft_intimate() {
        assert_eq!(decide_mode(50, &Mode::Normal), Mode::SoftIntimate);
        assert_eq!(decide_mode(74, &Mode::Normal), Mode::SoftIntimate);
    }

    #[test]
    fn test_decide_mode_high_intimate() {
        assert_eq!(decide_mode(75, &Mode::Normal), Mode::HighIntimate);
        assert_eq!(decide_mode(90, &Mode::SoftIntimate), Mode::HighIntimate);
    }

    #[test]
    fn test_decide_mode_cooling() {
        // When cooling, stay cooling until tension drops below 50
        assert_eq!(decide_mode(80, &Mode::Cooling), Mode::Cooling);
        assert_eq!(decide_mode(60, &Mode::Cooling), Mode::Cooling);
        assert_eq!(decide_mode(49, &Mode::Cooling), Mode::Normal); // Recovery
    }

    #[test]
    fn test_decide_mode_transition() {
        // High intimate should transition to normal at low tension
        assert_eq!(decide_mode(40, &Mode::HighIntimate), Mode::Normal);
    }
}
