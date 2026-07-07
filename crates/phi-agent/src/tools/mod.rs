//! The native coding toolset (pi's default four: read, bash, edit, write),
//! registered through the same `Host::register_tool` a user extension would
//! use (doctrine 01). grep/find/ls are deliberately absent — bash covers
//! them and the system prompt says so.

use phi_host::Host;
use tokio_util::sync::CancellationToken;

mod bash;
mod edit;
mod read;
mod write;

/// Source attribution for the registry.
pub(crate) const SOURCE: &str = "<native:phi-agent>";

/// Register the default coding tools. `cancel` aborts an in-flight bash
/// command (the token the frontend trips on Esc/Ctrl+C).
pub fn register_all(host: &Host, cancel: CancellationToken) -> anyhow::Result<()> {
    read::register(host)?;
    bash::register(host, cancel)?;
    edit::register(host)?;
    write::register(host)?;
    Ok(())
}

/// Fetch a required string argument, with tool-quality error text (the
/// model reads these).
pub(crate) fn require_str<'a>(args: &'a serde_json::Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("'{key}' must be a non-empty string"))
}
