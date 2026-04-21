/// Token Budget — Strategy 2: dual threshold detection
///
/// Threshold 1 (90%): input_tokens > context_window * 0.9 → trigger Compact
/// Threshold 2 (3x marginal delta): (current - prev) > (prev - prev_prev) * 3 → stop loop
pub struct TokenBudget {
    context_window: usize,
    trigger_pct: f32,
    marginal_multiplier: f32,
    min_marginal_threshold: usize,
    prev_turn_input: usize,
    prev_prev_turn_input: usize,
}

#[derive(Debug)]
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
            min_marginal_threshold: 10000, // 从 1000 上调，以容忍网页快照等几千 Token 的正常突变
            prev_turn_input: 0,
            prev_prev_turn_input: 0,
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

    /// Threshold 2: marginal cost explosion (3x previous turn's marginal increase)
    ///
    /// Compares the *increase* in input tokens between turns, not the absolute values.
    /// As conversation grows, absolute input_tokens naturally rises; what matters is
    /// whether the *delta* (new tokens added this turn) is exploding relative to the
    /// previous turn's delta.
    pub fn is_diminishing(&self, current_turn_input: usize) -> bool {
        // Need at least two prior data points to compute deltas
        if self.prev_turn_input == 0 || self.prev_prev_turn_input == 0 {
            return false;
        }
        let current_delta = current_turn_input.saturating_sub(self.prev_turn_input);
        let prev_delta = self.prev_turn_input.saturating_sub(self.prev_prev_turn_input);

        prev_delta > 0
            && current_delta > self.min_marginal_threshold
            && current_delta > prev_delta * self.marginal_multiplier as usize
    }

    /// Record this turn's input tokens for next comparison
    pub fn record_turn(&mut self, input_tokens: usize) {
        self.prev_prev_turn_input = self.prev_turn_input;
        self.prev_turn_input = input_tokens;
    }

    /// Current usage percentage
    pub fn usage_pct(&self, input_tokens: usize) -> f32 {
        input_tokens as f32 / self.context_window as f32
    }
}
