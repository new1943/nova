use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub enum SandboxLevel {
    None,
    ReadOnly,
    Restricted,
    Full,
}

#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    pub level: SandboxLevel,
    pub allowed_paths: HashSet<PathBuf>,
    pub blocked_paths: HashSet<PathBuf>,
    pub allow_network: bool,
    pub allow_write: bool,
    pub max_file_size: usize,
}

impl SandboxPolicy {
    pub fn unrestricted() -> Self {
        Self { level: SandboxLevel::None, allowed_paths: HashSet::new(), blocked_paths: HashSet::new(), allow_network: true, allow_write: true, max_file_size: 10 * 1024 * 1024 }
    }

    pub fn read_only() -> Self {
        Self { level: SandboxLevel::ReadOnly, allowed_paths: HashSet::new(), blocked_paths: HashSet::new(), allow_network: false, allow_write: false, max_file_size: 1024 * 1024 }
    }

    pub fn restricted(workspace: PathBuf) -> Self {
        let mut allowed = HashSet::new();
        allowed.insert(workspace);
        Self { level: SandboxLevel::Restricted, allowed_paths: allowed, blocked_paths: HashSet::new(), allow_network: false, allow_write: true, max_file_size: 1024 * 1024 }
    }

    pub fn check_path(&self, path: &std::path::Path) -> bool {
        match self.level {
            SandboxLevel::None => true,
            _ => {
                for blocked in &self.blocked_paths {
                    if path.starts_with(blocked) { return false; }
                }
                if !self.allowed_paths.is_empty() {
                    return self.allowed_paths.iter().any(|a| path.starts_with(a));
                }
                true
            }
        }
    }

    pub fn check_write(&self) -> bool { self.allow_write }
    pub fn check_network(&self) -> bool { self.allow_network }
}
