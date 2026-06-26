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
#[cfg(unix)]
pub use transport::{DaemonLock, daemon_lock_path, daemon_lock_path_for, signal_ready};
pub use wire::{HistoryMessage, Request, ServerEvent};

/// Peek the `"id"` field from a JSON line *before* full deserialization.
///
/// When `decode_request` fails (unknown variant, missing field, etc.) we
/// still want to correlate the error with the client's request id so the
/// client knows which request failed.  This peeks the top-level `"id"` key
/// from the raw JSON value and returns it, or `0` if the payload is not
/// valid JSON or has no numeric `id`.
pub fn peek_request_id(line: &str) -> u64 {
    let value: serde_json::Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    value.get("id").and_then(|v| v.as_u64()).unwrap_or(0)
}

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
