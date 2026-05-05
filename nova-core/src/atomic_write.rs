use std::io::Write;
use std::path::Path;

use anyhow::Result;

/// Synchronous atomic write: writes to a temp file then renames.
/// Temp file naming: `.{filename}.tmp` in the same directory.
pub fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let filename = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("invalid path: no filename"))?;
    let temp_path = parent.join(format!(".{}.tmp", filename.to_string_lossy()));

    // Ensure parent directory exists
    std::fs::create_dir_all(parent)?;

    // Write to temp file
    let write_result = (|| -> Result<()> {
        let mut file = std::fs::File::create(&temp_path)?;
        file.write_all(content)?;
        file.sync_all()?; // fsync to ensure data is flushed to disk
        Ok(())
    })();

    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(e);
    }

    // Atomic rename
    if let Err(e) = std::fs::rename(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(anyhow::anyhow!("atomic rename failed: {}", e));
    }

    Ok(())
}

/// Asynchronous atomic write using tokio::fs.
pub async fn atomic_write_async(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let filename = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("invalid path: no filename"))?;
    let temp_path = parent.join(format!(".{}.tmp", filename.to_string_lossy()));

    // Ensure parent directory exists
    tokio::fs::create_dir_all(parent).await?;

    // Write to temp file
    let write_result = tokio::fs::write(&temp_path, content).await;
    if let Err(e) = write_result {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(anyhow::anyhow!("atomic write failed: {}", e));
    }

    // Atomic rename
    if let Err(e) = tokio::fs::rename(&temp_path, path).await {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(anyhow::anyhow!("atomic rename failed: {}", e));
    }

    Ok(())
}
