use anyhow::{Context as _, Result};
use std::time::Duration;

/// Maximum bytes accepted from a model-listing endpoint. These are dev-only
/// manual fetches against hardcoded HTTPS URLs; the cap bounds memory if a
/// compromised endpoint or TLS MITM streams an oversized body.
pub(super) const MAX_MODELS_JSON_BYTES: usize = 64 * 1024 * 1024;

/// Whole-request deadline: connect + headers + body must finish inside this
/// budget, or the fetch is abandoned (a stalled endpoint must not hang the
/// catalog regen).
const FETCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Fetch and parse a JSON model listing with a body-size cap (`reqwest`'s
/// `.json()` buffers the whole response with no limit) and a whole-request
/// deadline (a stalled endpoint must not hang the regen).
pub(super) async fn fetch_capped_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T> {
    use futures::StreamExt as _;
    tokio::time::timeout(FETCH_TIMEOUT, async {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .context("building HTTP client")?;
        let response = client
            .get(url)
            .timeout(FETCH_TIMEOUT)
            .send()
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
    })
    .await
    .context("model-list fetch timed out")?
}
