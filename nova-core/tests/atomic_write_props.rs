use nova_core::atomic_write::{atomic_write, atomic_write_async};
use proptest::prelude::*;
use std::path::PathBuf;
use tempfile::TempDir;

/// Strategy to generate a valid filename (no path separators, no empty, no dots-only).
fn valid_filename_strategy() -> impl Strategy<Value = String> {
    "[a-zA-Z][a-zA-Z0-9_]{0,20}\\.[a-z]{1,4}"
}

/// Strategy to generate arbitrary byte content.
fn content_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..1024)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Feature: r4-self-evolution, Property 7: atomic_write 内容保持性
    ///
    /// **Validates: Requirements 6.4, 7.4, 8.2, 9.3**
    ///
    /// For any valid path and any byte content, after atomic_write succeeds,
    /// the target file contains exactly the same byte sequence as the written content.
    #[test]
    fn prop_atomic_write_content_preservation(
        filename in valid_filename_strategy(),
        content in content_strategy(),
    ) {
        let temp_dir = TempDir::new().unwrap();
        let target_path = temp_dir.path().join(&filename);

        // Perform atomic write
        let result = atomic_write(&target_path, &content);
        prop_assert!(result.is_ok(), "atomic_write failed: {:?}", result.err());

        // Read back and verify content is identical
        let read_back = std::fs::read(&target_path).unwrap();
        prop_assert_eq!(
            read_back,
            content,
            "Content mismatch: written bytes differ from read bytes"
        );
    }

    /// Feature: r4-self-evolution, Property 7: atomic_write_async 内容保持性
    ///
    /// **Validates: Requirements 6.4, 7.4, 8.2, 9.3**
    ///
    /// For any valid path and any byte content, after atomic_write_async succeeds,
    /// the target file contains exactly the same byte sequence as the written content.
    #[test]
    fn prop_atomic_write_async_content_preservation(
        filename in valid_filename_strategy(),
        content in content_strategy(),
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let temp_dir = TempDir::new().unwrap();
        let target_path = temp_dir.path().join(&filename);

        // Perform async atomic write
        let result = rt.block_on(atomic_write_async(&target_path, &content));
        prop_assert!(result.is_ok(), "atomic_write_async failed: {:?}", result.err());

        // Read back and verify content is identical
        let read_back = std::fs::read(&target_path).unwrap();
        prop_assert_eq!(
            read_back,
            content,
            "Content mismatch: written bytes differ from read bytes"
        );
    }

    /// Feature: r4-self-evolution, Property 7: atomic_write creates parent directories
    ///
    /// **Validates: Requirements 9.3**
    ///
    /// For any valid nested path, atomic_write should create parent directories
    /// and the written content should be preserved.
    #[test]
    fn prop_atomic_write_creates_parent_dirs(
        subdir in "[a-z]{1,5}",
        filename in valid_filename_strategy(),
        content in content_strategy(),
    ) {
        let temp_dir = TempDir::new().unwrap();
        let target_path = temp_dir.path().join(&subdir).join(&filename);

        let result = atomic_write(&target_path, &content);
        prop_assert!(result.is_ok(), "atomic_write failed: {:?}", result.err());

        let read_back = std::fs::read(&target_path).unwrap();
        prop_assert_eq!(read_back, content);
    }
}


// ── Boundary Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod boundary_tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    /// Test: invalid path with no filename (e.g., "/" or empty-like paths)
    #[test]
    fn test_invalid_path_no_filename() {
        let path = PathBuf::from("/");
        let result = atomic_write(&path, b"hello");
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("invalid path"),
            "Expected 'invalid path' error for root path"
        );
    }

    /// Test: invalid path with no filename (async version)
    #[tokio::test]
    async fn test_invalid_path_no_filename_async() {
        let path = PathBuf::from("/");
        let result = atomic_write_async(&path, b"hello").await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("invalid path"),
            "Expected 'invalid path' error for root path"
        );
    }

    /// Test: rename failure cleans up temp file.
    /// Simulate by making the target directory read-only after creating the temp file.
    #[test]
    fn test_rename_failure_cleans_temp_file() {
        let temp_dir = TempDir::new().unwrap();
        let target_dir = temp_dir.path().join("readonly_dir");
        fs::create_dir_all(&target_dir).unwrap();

        let target_path = target_dir.join("test.txt");
        let temp_file_path = target_dir.join(".test.txt.tmp");

        // First, write a file successfully so the target exists
        atomic_write(&target_path, b"original").unwrap();

        // Make the directory read-only to prevent rename from working
        // On Unix, removing write permission on the directory prevents creating new files
        // and renaming into it. But since temp is in the same dir, we need a different approach.
        // Instead, we'll make the target a directory to cause rename to fail.
        fs::remove_file(&target_path).unwrap();
        fs::create_dir(&target_path).unwrap();

        // Now atomic_write should fail because we can't rename a file over a directory
        let result = atomic_write(&target_path, b"new content");
        assert!(result.is_err(), "Expected rename to fail when target is a directory");

        // Verify temp file was cleaned up
        assert!(
            !temp_file_path.exists(),
            "Temp file should be cleaned up after rename failure"
        );
    }

    /// Test: rename failure cleans up temp file (async version).
    #[tokio::test]
    async fn test_rename_failure_cleans_temp_file_async() {
        let temp_dir = TempDir::new().unwrap();
        let target_dir = temp_dir.path().join("readonly_dir_async");
        fs::create_dir_all(&target_dir).unwrap();

        let target_path = target_dir.join("test.txt");
        let temp_file_path = target_dir.join(".test.txt.tmp");

        // Write a file successfully first
        atomic_write_async(&target_path, b"original").await.unwrap();

        // Make the target a directory to cause rename to fail
        fs::remove_file(&target_path).unwrap();
        fs::create_dir(&target_path).unwrap();

        // Now atomic_write_async should fail
        let result = atomic_write_async(&target_path, b"new content").await;
        assert!(result.is_err(), "Expected rename to fail when target is a directory");

        // Verify temp file was cleaned up
        assert!(
            !temp_file_path.exists(),
            "Temp file should be cleaned up after rename failure"
        );
    }

    /// Test: write failure cleans up temp file (simulate by making parent read-only).
    #[test]
    fn test_write_failure_on_readonly_parent() {
        let temp_dir = TempDir::new().unwrap();
        let target_dir = temp_dir.path().join("write_fail_dir");
        fs::create_dir_all(&target_dir).unwrap();

        // Make directory read-only
        let perms = fs::Permissions::from_mode(0o555);
        fs::set_permissions(&target_dir, perms).unwrap();

        let target_path = target_dir.join("test.txt");
        let result = atomic_write(&target_path, b"content");

        // Restore permissions for cleanup
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(&target_dir, perms).unwrap();

        assert!(result.is_err(), "Expected write to fail on read-only directory");

        // Temp file should not exist
        let temp_file_path = target_dir.join(".test.txt.tmp");
        assert!(
            !temp_file_path.exists(),
            "Temp file should not exist after write failure"
        );
    }

    /// Test: parent directories are created if they don't exist.
    #[test]
    fn test_creates_parent_directories() {
        let temp_dir = TempDir::new().unwrap();
        let nested_path = temp_dir.path().join("a").join("b").join("c").join("file.txt");

        let result = atomic_write(&nested_path, b"nested content");
        assert!(result.is_ok(), "Should create parent dirs: {:?}", result.err());

        let read_back = fs::read(&nested_path).unwrap();
        assert_eq!(read_back, b"nested content");
    }

    /// Test: parent directories are created if they don't exist (async).
    #[tokio::test]
    async fn test_creates_parent_directories_async() {
        let temp_dir = TempDir::new().unwrap();
        let nested_path = temp_dir.path().join("x").join("y").join("z").join("file.txt");

        let result = atomic_write_async(&nested_path, b"nested async content").await;
        assert!(result.is_ok(), "Should create parent dirs: {:?}", result.err());

        let read_back = fs::read(&nested_path).unwrap();
        assert_eq!(read_back, b"nested async content");
    }

    /// Test: overwriting an existing file preserves atomicity.
    #[test]
    fn test_overwrite_existing_file() {
        let temp_dir = TempDir::new().unwrap();
        let target_path = temp_dir.path().join("overwrite.txt");

        // Write initial content
        atomic_write(&target_path, b"initial").unwrap();
        assert_eq!(fs::read(&target_path).unwrap(), b"initial");

        // Overwrite with new content
        atomic_write(&target_path, b"updated").unwrap();
        assert_eq!(fs::read(&target_path).unwrap(), b"updated");
    }

    /// Test: writing empty content is valid.
    #[test]
    fn test_empty_content() {
        let temp_dir = TempDir::new().unwrap();
        let target_path = temp_dir.path().join("empty.txt");

        let result = atomic_write(&target_path, b"");
        assert!(result.is_ok());

        let read_back = fs::read(&target_path).unwrap();
        assert_eq!(read_back, b"");
    }
}
