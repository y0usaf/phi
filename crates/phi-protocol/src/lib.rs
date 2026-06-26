//! Client-server protocol for phi.
//!
//! Newline-delimited JSON over a Unix domain socket. The server owns the RLM
//! engine and provider; clients send [`Request`]s and stream [`ServerEvent`]s
//! back during message processing.
//!
//! Mirrors `ref/jcode`'s `jcode-protocol` crate.

pub mod transport;
mod wire;

pub use transport::{
    Listener, ReadHalf, Stream, WriteHalf, cleanup_socket_pair, connect_socket, debug_socket_path,
    is_socket_path, reap_stale_socket_if_dead, remove_socket, socket_has_live_listener,
    socket_path,
};
pub use wire::{HistoryMessage, Request, ServerEvent};

use anyhow::{Context as _, Result};
use tokio::io::AsyncWriteExt;

/// Serialize a value as a single NDJSON line and write it to `writer`.
pub async fn write_line<W, T>(writer: &mut W, value: &T) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
    T: serde::Serialize,
{
    let json = serde_json::to_string(value)? + "\n";
    writer.write_all(json.as_bytes()).await?;
    Ok(())
}

/// Decode a single NDJSON line into a [`Request`].
pub fn decode_request(line: &str) -> Result<Request> {
    serde_json::from_str(line.trim()).context("invalid request JSON")
}

/// Encode a [`ServerEvent`] as a single NDJSON line.
pub fn encode_event(event: &ServerEvent) -> Result<String> {
    Ok(serde_json::to_string(event)? + "\n")
}
