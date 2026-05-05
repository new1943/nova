use super::loader::SkillsLoader;
use anyhow::Result;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub type SharedSkillsLoader = Arc<Mutex<SkillsLoader>>;

pub fn create_shared_loader(skills_dir: PathBuf) -> Result<SharedSkillsLoader> {
    let mut loader = SkillsLoader::new(skills_dir);
    loader.load_all()?;
    Ok(Arc::new(Mutex::new(loader)))
}

pub fn invalidate_skill_cache(loader: &SharedSkillsLoader) -> Result<()> {
    let mut skills = loader.lock().map_err(|_| anyhow::anyhow!("Failed to acquire skills lock"))?;
    skills.reload()
}

pub fn with_cache_invalidation<F, R>(loader: &SharedSkillsLoader, operation: F) -> Result<R>
where F: FnOnce() -> Result<R>,
{
    let result = operation()?;
    invalidate_skill_cache(loader)?;
    Ok(result)
}

impl SkillsLoader {
    pub fn reload(&mut self) -> Result<()> { self.load_all() }
    pub fn into_shared(self) -> SharedSkillsLoader { Arc::new(Mutex::new(self)) }
}
