use anyhow::{Context as _, Result};

/// Maximum bytes accepted from a model-listing endpoint. These are dev-only
/// manual fetches against hardcoded HTTPS URLs; the cap bounds memory if a
/// compromised endpoint or TLS MITM streams an oversized body.
pub(super) const MAX_MODELS_JSON_BYTES: usize = 64 * 1024 * 1024;

/// Fetch and parse a JSON model listing with a body-size cap. `reqwest`'s
/// `.json()` buffers the entire response with no limit, so we read the bytes
/// explicitly and reject anything over `MAX_MODELS_JSON_BYTES` before parsing.
pub(super) async fn fetch_capped_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T> {
    use futures::StreamExt as _;
    let response = reqwest::get(url)
        .await
        .context("fetching model listing")?
        .error_for_status()?;
    let mut buf: Vec<u8> = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading model-list body")?;
        buf.extend_from_slice(&chunk);
        if buf.len() > MAX_MODELS_JSON_BYTES {
            anyhow::bail!(
                "model-list response from {url} exceeded {} bytes",
                MAX_MODELS_JSON_BYTES
            );
        }
    }
    serde_json::from_slice(&buf).context("parsing model-list response")
}
