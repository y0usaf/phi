//! The phi server crate: a daemon that owns the RLM engine and provider,
//! listens on a Unix socket, and streams events to connected clients.
//!
//! Mirrors `ref/jcode`'s `jcode-app-core` server layer, scoped to phi's
//! single-session RLM engine.
//!
//! Architecture:
//! - [`server::Server`] binds sockets and accepts connections.
//! - [`session::ServerSession`] wraps the RLM [`RlmSession`] and bridges
//!   [`RlmEvent`]s to wire [`ServerEvent`]s.
//! - [`client::Client`] connects to a running server and sends requests /
//!   reads events.

pub mod client;
pub mod server;
pub mod session;

pub use client::Client;
pub use server::{EXIT_IDLE_TIMEOUT, Server};
