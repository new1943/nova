use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

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
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl IpcConnection {
    pub fn new(stream: UnixStream) -> Self {
        let (read, write) = stream.into_split();
        Self {
            reader: BufReader::new(read),
            writer: write,
        }
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
    pub async fn send_event(&mut self, event: &Event) -> Result<()> {
        let mut data = serde_json::to_string(event)?;
        data.push('\n');
        self.writer.write_all(data.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }
}
