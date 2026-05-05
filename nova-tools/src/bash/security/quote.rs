//! Quote state tracking for bash parsing.

/// Quote state during bash parsing
#[derive(Debug, Clone, Default)]
pub struct QuoteState {
    pub in_single_quote: bool,
    pub in_double_quote: bool,
    pub escaped: bool,
}

impl QuoteState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, c: char) {
        if self.escaped {
            self.escaped = false;
            return;
        }

        match c {
            '\\' if !self.in_single_quote => {
                self.escaped = true;
            }
            '\'' if !self.in_double_quote => {
                self.in_single_quote = !self.in_single_quote;
            }
            '"' if !self.in_single_quote => {
                self.in_double_quote = !self.in_double_quote;
            }
            _ => {}
        }
    }

    pub fn is_quoted(&self) -> bool {
        self.in_single_quote || self.in_double_quote
    }

    #[allow(dead_code)]
    pub fn is_single_quoted(&self) -> bool {
        self.in_single_quote
    }

    pub fn is_double_quoted(&self) -> bool {
        self.in_double_quote
    }

    pub fn is_escaped(&self) -> bool {
        self.escaped
    }

    /// Parse a command and return the unquoted content
    pub fn extract_unquoted(content: &str) -> (String, String) {
        let mut with_double = String::new();
        let mut fully_unquoted = String::new();
        let mut state = QuoteState::new();

        for c in content.chars() {
            state.update(c);

            if !state.is_single_quoted() {
                with_double.push(c);
            }
            if !state.in_single_quote {
                fully_unquoted.push(c);
            }
        }

        (with_double, fully_unquoted)
    }

    pub fn has_unescaped(content: &str, char: char) -> bool {
        let mut state = QuoteState::new();
        for c in content.chars() {
            let was_escaped = state.is_escaped();
            state.update(c);

            if c == char && !state.is_quoted() && !was_escaped {
                return true;
            }
        }
        false
    }

    pub fn reset(&mut self) {
        self.in_single_quote = false;
        self.in_double_quote = false;
        self.escaped = false;
    }
}
