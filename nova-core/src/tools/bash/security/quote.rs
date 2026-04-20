//! Quote state tracking for bash parsing.
//!
//! Tracks whether we're inside single/double quotes and whether the current
//! character is escaped.

/// Quote state during bash parsing
#[derive(Debug, Clone, Default)]
pub struct QuoteState {
    /// True if currently inside single quotes
    pub in_single_quote: bool,
    /// True if currently inside double quotes
    pub in_double_quote: bool,
    /// True if the current character is escaped (preceded by \)
    pub escaped: bool,
}

impl QuoteState {
    /// Create a new quote state
    pub fn new() -> Self {
        Self::default()
    }

    /// Update state based on a character
    pub fn update(&mut self, c: char) {
        if self.escaped {
            // Any character after \ is escaped (literal)
            self.escaped = false;
            return;
        }

        match c {
            '\\' if !self.in_single_quote => {
                // Backslash escapes next char outside single quotes
                self.escaped = true;
            }
            '\'' if !self.in_double_quote => {
                // Toggle single quote state
                self.in_single_quote = !self.in_single_quote;
            }
            '"' if !self.in_single_quote => {
                // Toggle double quote state
                self.in_double_quote = !self.in_double_quote;
            }
            _ => {}
        }
    }

    /// Check if current position is quoted (inside any quote)
    pub fn is_quoted(&self) -> bool {
        self.in_single_quote || self.in_double_quote
    }

    /// Check if we're inside single quotes
    #[allow(dead_code)]
    pub fn is_single_quoted(&self) -> bool {
        self.in_single_quote
    }

    /// Check if we're inside double quotes
    pub fn is_double_quoted(&self) -> bool {
        self.in_double_quote
    }

    /// Check if current char is escaped
    pub fn is_escaped(&self) -> bool {
        self.escaped
    }

    /// Parse a command and return the unquoted content
    pub fn extract_unquoted(content: &str) -> (String, String) {
        // Returns (with_double_quotes, fully_unquoted)
        let mut with_double = String::new();
        let mut fully_unquoted = String::new();
        let mut state = QuoteState::new();

        for c in content.chars() {
            state.update(c);

            // Track what goes into each string
            if !state.is_single_quoted() {
                with_double.push(c);
            }
            if !state.in_single_quote {
                fully_unquoted.push(c);
            }
        }

        (with_double, fully_unquoted)
    }

    /// Check if content contains an unescaped character
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

    /// Reset the quote state
    pub fn reset(&mut self) {
        self.in_single_quote = false;
        self.in_double_quote = false;
        self.escaped = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quote_state_basic() {
        let mut state = QuoteState::new();
        assert!(!state.is_quoted());

        // Enter single quote
        state.update('\'');
        assert!(state.is_single_quoted());

        // Exit single quote
        state.update('\'');
        assert!(!state.is_single_quoted());
    }

    #[test]
    fn test_quote_state_double() {
        let mut state = QuoteState::new();

        state.update('"');
        assert!(state.is_double_quoted());

        state.update('"');
        assert!(!state.is_double_quoted());
    }

    #[test]
    fn test_escape() {
        let mut state = QuoteState::new();

        state.update('\\');
        assert!(state.is_escaped());

        state.update('n'); // The n is escaped, not toggling anything
        assert!(!state.is_escaped());
    }

    #[test]
    fn test_extract_unquoted() {
        let (with_double, fully_unquoted) = QuoteState::extract_unquoted("echo 'hello' \"world\"");
        assert!(fully_unquoted.contains("echo"));
        assert!(fully_unquoted.contains("world"));
    }

    #[test]
    fn test_has_unescaped() {
        assert!(QuoteState::has_unescaped("hello world", ' '));
        assert!(!QuoteState::has_unescaped("'hello world'", ' ')); // quoted
        assert!(!QuoteState::has_unescaped("hello\\ world", ' ')); // escaped
    }
}
