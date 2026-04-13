use anyhow::Result;
use std::path::PathBuf;
use std::process::Command;
use tracing::{info, warn};

/// A temporary git worktree for isolated code changes
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    repo_root: PathBuf,
    cleaned: bool,
}

impl Worktree {
    /// Create a new worktree at the given path from the repo root
    fn new(repo_root: PathBuf, path: PathBuf, branch: String) -> Self {
        Self { path, branch, repo_root, cleaned: false }
    }

    /// Clean up the worktree (remove branch + directory)
    pub fn cleanup(&mut self) -> Result<()> {
        if self.cleaned {
            return Ok(());
        }
        info!("Cleaning up worktree: {:?}", self.path);

        let _ = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .current_dir(&self.repo_root)
            .output();

        let _ = Command::new("git")
            .args(["branch", "-D", &self.branch])
            .current_dir(&self.repo_root)
            .output();

        self.cleaned = true;
        Ok(())
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        if !self.cleaned {
            if let Err(e) = self.cleanup() {
                warn!("Failed to cleanup worktree: {}", e);
            }
        }
    }
}

/// Manages worktree creation and cleanup per session
pub struct WorktreeManager {
    repo_root: PathBuf,
    base_dir: PathBuf,
}

impl WorktreeManager {
    pub fn new(repo_root: PathBuf) -> Self {
        let base_dir = repo_root.join(".nova-worktrees");
        Self { repo_root, base_dir }
    }

    /// Check if the repo root is a git repository
    pub fn is_git_repo(&self) -> bool {
        self.repo_root.join(".git").exists()
    }

    /// Create an isolated worktree for a session
    pub fn create(&self, session_id: &str) -> Result<Worktree> {
        if !self.is_git_repo() {
            anyhow::bail!("Not a git repository: {:?}", self.repo_root);
        }

        let branch = format!("nova-session-{}", &session_id[..8.min(session_id.len())]);
        let wt_path = self.base_dir.join(&branch);

        std::fs::create_dir_all(&self.base_dir)?;

        let output = Command::new("git")
            .args(["worktree", "add", "-b", &branch])
            .arg(&wt_path)
            .current_dir(&self.repo_root)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git worktree add failed: {}", stderr);
        }

        info!("Created worktree: {:?} (branch: {})", wt_path, branch);
        Ok(Worktree::new(self.repo_root.clone(), wt_path, branch))
    }
}
