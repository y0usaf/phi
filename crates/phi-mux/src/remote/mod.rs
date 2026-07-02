//! Client/server split for the mux: one headless daemon owns the `MuxApp`
//! (sessions, canonical screen); any number of thin terminal clients attach
//! over a unix socket, send serialized input events, and write the daemon's
//! pre-rendered ANSI frames verbatim to their own tty.

pub mod client;
pub mod daemon;
pub mod protocol;
pub mod server;
