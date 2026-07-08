//! Fetch-with-retry — port of the spec's request loop
//! (`openai-codex-responses.ts`, main stream function: attempt loop,
//! header timeout, retry-after handling, abort checks).
//!
//! Spec nuances preserved (pinned by tests):
//! - a *retryable* error status picks the smart delay (retry-after when
//!   present, capped for 429; backoff otherwise);
//! - every other failure — non-retryable statuses included — still
//!   retries with plain backoff while attempts remain, *unless* the
//!   error text mentions "usage limit" (the spec's catch-all `catch`);
//! - abort wins over everything and surfaces as
//!   [`TransportError::Aborted`];
//! - the header timeout covers time-to-response-headers only and is
//!   retryable like any network error.

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use reqwest::header::HeaderMap;

use super::TransportError;
use super::abort::AbortSignal;
use super::retry::{
    DEFAULT_MAX_RETRIES, DEFAULT_SSE_HEADER_TIMEOUT_MS, backoff_delay_ms, cap_retry_delay_ms,
    is_retryable_error, retry_after_delay_ms, sleep_ms,
};
use super::sse::SseReader;

/// Retry knobs from the spec's `StreamOptions`
/// (`maxRetries`/`maxRetryDelayMs`) plus the SSE header timeout.
#[derive(Clone, Debug)]
pub struct RetryOptions {
    /// Spec: `maxRetries` (default 0).
    pub max_retries: u32,
    /// Spec: `maxRetryDelayMs` (default 60 000; 0 disables the cap).
    pub max_retry_delay_ms: Option<f64>,
    /// Spec: `DEFAULT_SSE_HEADER_TIMEOUT_MS` (not configurable there).
    pub header_timeout_ms: u64,
}

impl Default for RetryOptions {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_RETRIES,
            max_retry_delay_ms: None,
            header_timeout_ms: DEFAULT_SSE_HEADER_TIMEOUT_MS,
        }
    }
}

/// POST `body` to `url`, retrying per the spec's loop. Returns the first
/// successful response; the caller owns body consumption (see
/// [`response_sse_reader`]).
pub async fn post_with_retry(
    client: &reqwest::Client,
    url: &str,
    headers: &HeaderMap,
    body: &str,
    retry: &RetryOptions,
    signal: Option<&AbortSignal>,
) -> Result<reqwest::Response, TransportError> {
    let mut last_error = None;
    for attempt in 0..=retry.max_retries {
        if let Some(signal) = signal
            && signal.is_aborted()
        {
            return Err(TransportError::Aborted);
        }

        let send = client
            .post(url)
            .headers(headers.clone())
            .body(body.to_string())
            .send();
        match send_with_limits(send, retry.header_timeout_ms, signal).await {
            Ok(response) => {
                if response.status().is_success() {
                    return Ok(response);
                }

                let status = response.status().as_u16();
                let status_text = response
                    .status()
                    .canonical_reason()
                    .unwrap_or_default()
                    .to_string();
                let response_headers = response.headers().clone();
                let text = match response.text().await {
                    Ok(text) => text,
                    // Spec: a body-read failure falls into the generic
                    // catch (network path).
                    Err(error) => {
                        let error = TransportError::Network(error.to_string());
                        match retry_generic(attempt, retry, &error, signal).await? {
                            true => {
                                last_error = Some(error);
                                continue;
                            }
                            false => return Err(error),
                        }
                    }
                };

                if attempt < retry.max_retries && is_retryable_error(status, &text) {
                    let delay_ms = match retry_after_delay_ms(&response_headers) {
                        None => backoff_delay_ms(attempt),
                        Some(delay) if status == 429 => {
                            cap_retry_delay_ms(delay, retry.max_retry_delay_ms)
                        }
                        Some(delay) => delay,
                    };
                    sleep_ms(delay_ms, signal).await?;
                    continue;
                }

                // Spec: the thrown status error is caught by the same
                // catch as network failures and retried on backoff while
                // attempts remain (usage-limit errors excepted).
                let error = TransportError::Status {
                    status,
                    status_text,
                    body: text,
                };
                match retry_generic(attempt, retry, &error, signal).await? {
                    true => {
                        last_error = Some(error);
                        continue;
                    }
                    false => return Err(error),
                }
            }
            Err(TransportError::Aborted) => return Err(TransportError::Aborted),
            Err(error) => match retry_generic(attempt, retry, &error, signal).await? {
                true => {
                    last_error = Some(error);
                    continue;
                }
                false => return Err(error),
            },
        }
    }
    // Unreachable: the final attempt always returns. Kept for totality.
    Err(last_error.unwrap_or_else(|| TransportError::Network("Failed after retries".to_string())))
}

/// The spec's catch-all retry decision: backoff-sleep and retry while
/// attempts remain and the error does not mention "usage limit".
async fn retry_generic(
    attempt: u32,
    retry: &RetryOptions,
    error: &TransportError,
    signal: Option<&AbortSignal>,
) -> Result<bool, TransportError> {
    if attempt < retry.max_retries && !error.to_string().contains("usage limit") {
        sleep_ms(backoff_delay_ms(attempt), signal).await?;
        return Ok(true);
    }
    Ok(false)
}

/// Race the request against the header timeout and the abort signal
/// (spec: `createSSEHeaderTimeout` + `combineAbortSignals`).
async fn send_with_limits(
    send: impl Future<Output = Result<reqwest::Response, reqwest::Error>>,
    header_timeout_ms: u64,
    signal: Option<&AbortSignal>,
) -> Result<reqwest::Response, TransportError> {
    let timeout = tokio::time::sleep(std::time::Duration::from_millis(header_timeout_ms));
    tokio::pin!(timeout);
    tokio::pin!(send);
    match signal {
        Some(signal) => tokio::select! {
            _ = signal.aborted() => Err(TransportError::Aborted),
            _ = &mut timeout => Err(TransportError::HeaderTimeout(header_timeout_ms)),
            result = &mut send => result.map_err(|e| TransportError::Network(e.to_string())),
        },
        None => tokio::select! {
            _ = &mut timeout => Err(TransportError::HeaderTimeout(header_timeout_ms)),
            result = &mut send => result.map_err(|e| TransportError::Network(e.to_string())),
        },
    }
}

/// SSE reader over a response body (spec: `parseSSE(response, signal)`).
pub fn response_sse_reader(
    response: reqwest::Response,
    signal: Option<AbortSignal>,
) -> SseReader<BoxStream<'static, Result<bytes::Bytes, reqwest::Error>>> {
    SseReader::new(response.bytes_stream().boxed(), signal)
}
