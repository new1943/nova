//! Cache management for SkillsLoader.
//!
//! Provides shared, thread-safe access to SkillsLoader with invalidation capability.

use super::loader::SkillsLoader;
use anyhow::Result;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Shared skills loader type - thread-safe with interior mutability
pub type SharedSkillsLoader = Arc<Mutex<SkillsLoader>>;

/// Create a shared skills loader
pub fn create_shared_loader(skills_dir: PathBuf) -> Result<SharedSkillsLoader> {
    let mut loader = SkillsLoader::new(skills_dir);
    loader.load_all()?;
    Ok(Arc::new(Mutex::new(loader)))
}

/// Invalidate the cache and reload all skills
pub fn invalidate_skill_cache(loader: &SharedSkillsLoader) -> Result<()> {
    let mut skills = loader
        .lock()
        .map_err(|_| anyhow::anyhow!("Failed to acquire skills lock"))?;
    skills.reload()
}

/// Execute a skill modification and invalidate cache on success
pub fn with_cache_invalidation<F, R>(loader: &SharedSkillsLoader, operation: F) -> Result<R>
where
    F: FnOnce() -> Result<R>,
{
    let result = operation()?;
    invalidate_skill_cache(loader)?;
    Ok(result)
}

impl SkillsLoader {
    /// Reload all skills from disk
    pub fn reload(&mut self) -> Result<()> {
        self.load_all()
    }

    /// Convert into shared loader
    pub fn into_shared(self) -> SharedSkillsLoader {
        Arc::new(Mutex::new(self))
    }
}
