use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use std::path::Path;

use crate::protocol::{Event, Request};

/// IPC Client (TUI side) — connects to daemon Unix Socket
pub struct IpcClient {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl IpcClient {
    pub async fn connect(socket_path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket_path).await?;
        let (read, write) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(read),
            writer: write,
        })
    }

    /// Send a Request to the daemon
    pub async fn send_request(&mut self, req: &Request) -> Result<()> {
        let mut data = serde_json::to_string(req)?;
        data.push('\n');
        self.writer.write_all(data.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// Receive an Event from the daemon (None = EOF)
    pub async fn recv_event(&mut self) -> Result<Option<Event>> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(line.trim())?))
    }
}
