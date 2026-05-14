//! Token counter using tiktoken (cl100k_base encoding).

use std::sync::Arc;
use once_cell::sync::Lazy;
use anyhow::Result;

/// A wrapper for precise token counting using tiktoken.
pub struct TokenCounter {
    #[allow(clippy::type_complexity)]
    encoder: Box<dyn Fn(&str) -> Vec<u32> + Send + Sync>,
}

impl TokenCounter {
    /// Create a new TokenCounter with cl100k_base encoding.
    pub fn new() -> Result<Self> {
        let bpe = tiktoken_rs::cl100k_base()?;
        let encoder = Box::new(move |text: &str| bpe.encode_with_special_tokens(text));
        Ok(Self { encoder })
    }

    /// Count tokens for a single text string.
    pub fn count(&self, text: &str) -> usize {
        (self.encoder)(text).len()
    }

    /// Count tokens for multiple strings and return total.
    pub fn count_many(&self, texts: &[&str]) -> usize {
        texts.iter().map(|t| self.count(t)).sum()
    }
}

/// Global token counter instance.
static COUNTER: Lazy<Arc<TokenCounter>> = Lazy::new(|| {
    Arc::new(TokenCounter::new().expect("Failed to create token counter"))
});

/// Initialize the global token counter.
pub fn init() -> Result<()> {
    let _ = *COUNTER;
    Ok(())
}

/// Get the global token counter.
pub fn get_counter() -> Arc<TokenCounter> {
    COUNTER.clone()
}

/// Count tokens for a text string using the global counter.
pub fn count_tokens(text: &str) -> usize {
    get_counter().count(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_tokens() {
        let tokens = count_tokens("hello");
        assert_eq!(tokens, 1);

        assert_eq!(count_tokens(""), 0);

        let chinese = count_tokens("你好世界");
        assert!(chinese > 0);
    }
}
