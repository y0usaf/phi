//! Client for connecting to a running phi server.
//!
//! Mirrors `ref/jcode`'s `server/client_api.rs`. Sends [`Request`]s over a
//! Unix socket and reads streamed [`ServerEvent`]s back.

use anyhow::Result;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use phi_protocol::{ReadHalf, Request, ServerEvent, WriteHalf, connect_socket, debug_socket_path, socket_path};

/// A client connected to a running server.
pub struct Client {
    reader: BufReader<ReadHalf>,
    writer: WriteHalf,
    next_id: u64,
}

impl Client {
    /// Connect to the default server socket.
    pub async fn connect() -> Result<Self> {
        Self::connect_with_path(socket_path()).await
    }

    /// Connect to a specific socket path.
    pub async fn connect_with_path(path: PathBuf) -> Result<Self> {
        let stream = connect_socket(&path).await?;
        let (reader, writer) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(reader),
            writer,
            next_id: 1,
        })
    }

    /// Connect to the debug socket.
    pub async fn connect_debug() -> Result<Self> {
        Self::connect_debug_with_path(debug_socket_path()).await
    }

    /// Connect to a specific debug socket path.
    pub async fn connect_debug_with_path(path: PathBuf) -> Result<Self> {
        let stream = connect_socket(&path).await?;
        let (reader, writer) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(reader),
            writer,
            next_id: 1,
        })
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Send a raw NDJSON line (for testing protocol robustness with
    /// malformed or unknown request types).
    pub async fn send_raw(&mut self, line: &str) -> Result<()> {
        let json = line.to_string() + "
";
        self.writer.write_all(json.as_bytes()).await?;
        Ok(())
    }

    async fn send_request(&mut self, request: Request) -> Result<u64> {
        let json = serde_json::to_string(&request)? + "\n";
        self.writer.write_all(json.as_bytes()).await?;
        match &request {
            Request::Message { id, .. }
            | Request::Cancel { id }
            | Request::Clear { id }
            | Request::Ping { id }
            | Request::GetState { id }
            | Request::Subscribe { id, .. }
            | Request::GetHistory { id }
            | Request::ResumeSession { id, .. }
            | Request::Shutdown { id } => Ok(*id),
        }
    }

    /// Send a user message and return the request id.
    pub async fn send_message(&mut self, content: &str) -> Result<u64> {
        let id = self.next_request_id();
        self.send_request(Request::Message {
            id,
            content: content.to_string(),
        })
        .await
    }

    /// Cancel the current generation.
    pub async fn cancel(&mut self) -> Result<u64> {
        let id = self.next_request_id();
        self.send_request(Request::Cancel { id }).await
    }

    /// Clear conversation history.
    pub async fn clear(&mut self) -> Result<()> {
        let id = self.next_request_id();
        self.send_request(Request::Clear { id }).await?;
        Ok(())
    }

    /// Subscribe to streamed events.
    pub async fn subscribe(
        &mut self,
        working_dir: Option<String>,
        target_session_id: Option<String>,
    ) -> Result<u64> {
        let id = self.next_request_id();
        self.send_request(Request::Subscribe {
            id,
            working_dir,
            target_session_id,
        })
        .await
    }

    /// Health-check the server.
    pub async fn ping(&mut self) -> Result<bool> {
        let id = self.next_request_id();
        self.send_request(Request::Ping { id }).await?;

        loop {
            let event = self.read_event().await?;
            match event {
                ServerEvent::Pong { id: pong_id } => return Ok(pong_id == id),
                ServerEvent::Ack { id: ack_id } if ack_id == id => continue,
                ServerEvent::Error { id: error_id, .. } if error_id == id => return Ok(false),
                _ => return Ok(false),
            }
        }
    }

    /// Get the current server state.
    pub async fn get_state(&mut self) -> Result<ServerEvent> {
        let id = self.next_request_id();
        self.send_request(Request::GetState { id }).await?;
        // Skip acks until we get a state event.
        loop {
            let event = self.read_event().await?;
            match event {
                ServerEvent::Ack { .. } => continue,
                _ => return Ok(event),
            }
        }
    }

    /// Get conversation history.
    pub async fn get_history(&mut self) -> Result<Vec<phi_protocol::HistoryMessage>> {
        let id = self.next_request_id();
        self.send_request(Request::GetHistory { id }).await?;
        loop {
            let event = self.read_event().await?;
            match event {
                ServerEvent::Ack { .. } => continue,
                ServerEvent::History { messages } => return Ok(messages),
                _ => return Ok(Vec::new()),
            }
        }
    }

    /// Resume a specific session by id.
    pub async fn resume_session(&mut self, session_id: &str) -> Result<u64> {
        let id = self.next_request_id();
        self.send_request(Request::ResumeSession {
            id,
            session_id: session_id.to_string(),
        })
        .await
    }

    /// Request server shutdown.
    pub async fn shutdown(&mut self) -> Result<()> {
        let id = self.next_request_id();
        self.send_request(Request::Shutdown { id }).await?;
        Ok(())
    }

    /// Read the next event from the server.
    pub async fn read_event(&mut self) -> Result<ServerEvent> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            anyhow::bail!("Server disconnected");
        }
        let event: ServerEvent = serde_json::from_str(&line)?;
        Ok(event)
    }
}
