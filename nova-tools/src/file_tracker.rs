//! File read tracker for tracking which files have been read.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use std::time::SystemTime;

/// Device files that should never be read
const BLOCKED_DEVICES: &[&str] = &[
    "/dev/zero", "/dev/random", "/dev/urandom",
    "/dev/stdin", "/dev/stdout", "/dev/stderr", "/dev/null",
];

/// Information about a file that was read
#[derive(Debug, Clone)]
pub struct ReadInfo {
    pub mtime: SystemTime,
    pub is_full_read: bool,
    pub read_at: SystemTime,
}

/// Global file read tracker (thread-safe via Arc<Mutex>)
pub type SharedFileReadTracker = Arc<Mutex<FileReadTracker>>;

/// Global file read tracker
#[derive(Debug, Default)]
pub struct FileReadTracker {
    reads: HashMap<PathBuf, ReadInfo>,
}

impl FileReadTracker {
    pub fn new() -> Self {
        Self { reads: HashMap::new() }
    }

    pub fn mark_read(&mut self, path: &Path, mtime: SystemTime, is_full: bool) {
        let canonical = Self::canonicalize(path);
        if let Some(canonical) = canonical {
            self.reads.insert(canonical, ReadInfo {
                mtime,
                is_full_read: is_full,
                read_at: SystemTime::now(),
            });
        }
    }

    pub fn was_read(&self, path: &Path) -> bool {
        let canonical = Self::canonicalize(path);
        canonical.is_some_and(|p| self.reads.contains_key(&p))
    }

    pub fn get_read_info(&self, path: &Path) -> Option<&ReadInfo> {
        let canonical = Self::canonicalize(path);
        canonical.and_then(|p| self.reads.get(&p))
    }

    pub fn is_full_read(&self, path: &Path) -> bool {
        self.get_read_info(path).is_some_and(|info| info.is_full_read)
    }

    pub fn was_modified_since_read(&self, path: &Path, current_mtime: SystemTime) -> bool {
        if let Some(info) = self.get_read_info(path) {
            current_mtime > info.mtime
        } else {
            false
        }
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.reads.clear();
    }

    fn canonicalize(path: &Path) -> Option<PathBuf> {
        std::fs::canonicalize(path).ok()
    }
}

/// Create a new shared tracker
pub fn create_shared_tracker() -> SharedFileReadTracker {
    Arc::new(Mutex::new(FileReadTracker::new()))
}

/// Check if a path is a blocked device file
pub fn is_blocked_device(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    BLOCKED_DEVICES.iter().any(|dev| path_str == *dev)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blocked_devices() {
        assert!(is_blocked_device(Path::new("/dev/zero")));
        assert!(is_blocked_device(Path::new("/dev/random")));
        assert!(!is_blocked_device(Path::new("/tmp/test")));
    }

    #[test]
    fn test_shared_tracker_creation() {
        let tracker = create_shared_tracker();
        assert!(tracker.try_lock().is_ok());
    }
}
