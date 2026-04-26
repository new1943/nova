use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use crate::protocol::{Event, Request};

/// IPC Server (daemon side) — Unix Domain Socket
pub struct IpcServer {
    listener: UnixListener,
    socket_path: PathBuf,
}

impl IpcServer {
    pub async fn bind(socket_path: &Path) -> Result<Self> {
        // Remove stale socket
        let _ = std::fs::remove_file(socket_path);
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(socket_path)?;
        Ok(Self {
            listener,
            socket_path: socket_path.to_path_buf(),
        })
    }

    pub async fn accept(&self) -> Result<IpcConnection> {
        let (stream, _) = self.listener.accept().await?;
        Ok(IpcConnection::new(stream))
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Bidirectional IPC connection (JSON lines protocol)
pub struct IpcConnection {
    reader: BufReader<OwnedReadHalf>,
    writer: Arc<Mutex<OwnedWriteHalf>>,
}

/// Clone is not supported since BufReader<OwnedReadHalf> is !Clone.
/// Use clone_writer() to get a shared reference to the writer for sending events.
impl Clone for IpcConnection {
    fn clone(&self) -> Self {
        panic!("IpcConnection does not support clone() — use clone_writer() instead");
    }
}

impl IpcConnection {
    pub fn new(stream: UnixStream) -> Self {
        let (read_half, write_half) = stream.into_split();
        Self {
            reader: BufReader::new(read_half),
            writer: Arc::new(Mutex::new(write_half)),
        }
    }

    /// Clone the writer as an Arc<Mutex<OwnedWriteHalf>>.
    /// Use this to share the writer with other tasks for sending events.
    pub fn clone_writer(&self) -> Arc<Mutex<OwnedWriteHalf>> {
        self.writer.clone()
    }

    /// Read a Request from the connection
    pub async fn recv_request(&mut self) -> Result<Option<Request>> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None); // EOF
        }
        Ok(Some(serde_json::from_str(line.trim())?))
    }

    /// Send an Event to the connection
    pub async fn send_event(&self, event: &Event) -> Result<()> {
        let mut data = serde_json::to_string(event)?;
        data.push('\n');
        let mut w = self.writer.lock().await;
        w.write_all(data.as_bytes()).await?;
        w.flush().await?;
        Ok(())
    }
}

// Re-export OwnedReadHalf for use in main.rs heartbeat
pub use tokio::net::unix::OwnedReadHalf;
