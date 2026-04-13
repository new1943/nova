/// Token Budget — Strategy 2: dual threshold detection
///
/// Threshold 1 (90%): input_tokens > context_window * 0.9 → trigger Compact
/// Threshold 2 (3x marginal): current_turn > prev_turn * 3 && current > min → stop loop
pub struct TokenBudget {
    context_window: usize,
    trigger_pct: f32,
    marginal_multiplier: f32,
    min_marginal_threshold: usize,
    prev_turn_input: usize,
}

pub enum BudgetCheck {
    Ok,
    NeedsCompact,
    Diminishing,
}

impl TokenBudget {
    pub fn new(context_window: usize, trigger_pct: f32) -> Self {
        Self {
            context_window,
            trigger_pct,
            marginal_multiplier: 3.0,
            min_marginal_threshold: 1000,
            prev_turn_input: 0,
        }
    }

    /// Check budget after receiving API usage
    pub fn check(&self, input_tokens: usize) -> BudgetCheck {
        if self.needs_compact(input_tokens) {
            return BudgetCheck::NeedsCompact;
        }
        if self.is_diminishing(input_tokens) {
            return BudgetCheck::Diminishing;
        }
        BudgetCheck::Ok
    }

    /// Threshold 1: context nearly full
    pub fn needs_compact(&self, input_tokens: usize) -> bool {
        input_tokens as f32 > self.context_window as f32 * self.trigger_pct
    }

    /// Threshold 2: marginal cost explosion (3x previous turn)
    pub fn is_diminishing(&self, current_turn_input: usize) -> bool {
        self.prev_turn_input > 0
            && current_turn_input > self.min_marginal_threshold
            && current_turn_input > self.prev_turn_input * self.marginal_multiplier as usize
    }

    /// Record this turn's input tokens for next comparison
    pub fn record_turn(&mut self, input_tokens: usize) {
        self.prev_turn_input = input_tokens;
    }

    /// Current usage percentage
    pub fn usage_pct(&self, input_tokens: usize) -> f32 {
        input_tokens as f32 / self.context_window as f32
    }
}
