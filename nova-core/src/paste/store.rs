use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

/// Strategy 15: Paste Store — deduplicate pasted content by hash.
/// Same content stored once, referenced by hash in message history.
pub struct PasteStore {
    store: HashMap<u64, String>,
}

impl Default for PasteStore {
    fn default() -> Self { Self::new() }
}

impl PasteStore {
    pub fn new() -> Self {
        Self { store: HashMap::new() }
    }

    /// Store content, return its hash. If already stored, returns existing hash.
    pub fn store(&mut self, content: &str) -> u64 {
        let hash = Self::hash_content(content);
        self.store.entry(hash).or_insert_with(|| content.to_string());
        hash
    }

    /// Retrieve content by hash
    pub fn get(&self, hash: u64) -> Option<&str> {
        self.store.get(&hash).map(|s| s.as_str())
    }

    /// Check if content already exists
    pub fn contains(&self, content: &str) -> bool {
        self.store.contains_key(&Self::hash_content(content))
    }

    /// Get a reference placeholder for use in messages
    pub fn reference_tag(hash: u64) -> String {
        format!("[paste:{}]", hash)
    }

    /// Resolve a reference tag back to content
    pub fn resolve(&self, tag: &str) -> Option<&str> {
        let hash_str = tag.strip_prefix("[paste:")?.strip_suffix(']')?;
        let hash: u64 = hash_str.parse().ok()?;
        self.get(hash)
    }

    fn hash_content(content: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        hasher.finish()
    }

    pub fn len(&self) -> usize {
        self.store.len()
    }

    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }
}
