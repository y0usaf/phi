pub mod anthropic;
pub mod cursor;
pub mod openai_completions;
pub mod openai_responses;
mod retry;
mod sse;

use std::sync::OnceLock;
use std::time::Duration;

use crate::stream::{AssistantEvent, EventSink};
use crate::types::{Model, StopReason, StreamOptions};

pub use retry::is_terminal_rate_limit;
pub(crate) use retry::{
    fresh_transport_client, is_transient_transport_error, parse_retry_after, retry_delay,
    retryable_status,
};
pub(crate) use sse::{CappedSse, CappedSseError, MAX_SSE_EVENT_BYTES, SseHandler};

/// The one shared client: every provider clones this handle, so all
/// requests share a single connection pool (clone is an Arc bump, not a
/// new pool). HTTP/2 keepalive pings detect a silently-dead streaming
/// connection instead of letting a turn hang until the request timeout;
/// TCP keepalive covers the HTTP/1.1 fallback.
pub(crate) fn http_client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(30))
                .tcp_keepalive(Duration::from_secs(30))
                .pool_idle_timeout(Duration::from_secs(90))
                .pool_max_idle_per_host(8)
                .http2_keep_alive_interval(Duration::from_secs(30))
                .http2_keep_alive_timeout(Duration::from_secs(15))
                .http2_keep_alive_while_idle(true)
                .build()
                .expect("shared reqwest client must build")
        })
        .clone()
}

/// Sends the request with retries; on success, forwards SSE events to the
/// handler. All failures surface as a terminal `Error` event on `sink`.
/// Returns `false` when a terminal event was already emitted.
///
/// Retries cover both pre-connection failures (retryable HTTP status codes,
/// `send()` errors) and mid-stream transport failures (transient errors like
/// "error decoding response body", connection reset, HTTP/2 stream errors).
/// On a mid-stream retry where partial output was already emitted, a
/// `RetryRollback` event is sent so the consumer can discard the partial
/// attempt before the retry replays from the top. A fresh unpooled client is
/// used for retry attempts to avoid reusing poisoned TLS connections.
pub(crate) async fn run_sse<H: SseHandler>(
    client: &reqwest::Client,
    build_request: impl Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    model: &Model,
    opts: &StreamOptions,
    sink: &EventSink,
    handler: &mut H,
) -> bool {
    let mut attempt: u32 = 0;
    let mut saw_output = false;
    // After the first mid-stream retry, switch to a fresh unpooled client for
    // all subsequent attempts to avoid poisoned pooled connections.
    let mut retry_client: Option<reqwest::Client> = None;
    loop {
        if opts.cancel.is_cancelled() {
            sink.error(model, StopReason::Aborted, "aborted");
            return false;
        }

        // Pick the client for this attempt: original pooled for attempt 0,
        // fresh unpooled for retries.
        let active_client = retry_client.as_ref().unwrap_or(client);

        let request = build_request(active_client).timeout(opts.timeout);
        let result = tokio::select! {
            () = opts.cancel.cancelled() => {
                sink.error(model, StopReason::Aborted, "aborted");
                return false;
            }
            result = request.send() => result,
        };

        let response = match result {
            Ok(response) if response.status().is_success() => response,
            Ok(response) => {
                let status = response.status();
                let retry_after = parse_retry_after(&response);
                let body = response.text().await.unwrap_or_default();
                let error = if body.is_empty() {
                    format!("{} status code (no body)", status.as_u16())
                } else {
                    format!("HTTP {}: {}", status.as_u16(), body)
                };
                if retryable_status(status)
                    && attempt < opts.max_retries
                    && !is_terminal_rate_limit(&body)
                {
                    tokio::time::sleep(retry_delay(attempt, retry_after)).await;
                    attempt += 1;
                    if retry_client.is_none()
                        && let Ok(c) = fresh_transport_client()
                    {
                        retry_client = Some(c);
                    }
                    continue;
                }
                sink.error(model, StopReason::Error, error);
                return false;
            }
            Err(error) => {
                if attempt < opts.max_retries && !error.is_builder() {
                    tokio::time::sleep(retry_delay(attempt, None)).await;
                    attempt += 1;
                    if retry_client.is_none()
                        && let Ok(c) = fresh_transport_client()
                    {
                        retry_client = Some(c);
                    }
                    continue;
                }
                sink.error(model, StopReason::Error, error.to_string());
                return false;
            }
        };

        // Connection succeeded. Signal start (or rollback if this is a retry
        // after partial output was seen).
        if saw_output {
            sink.push(AssistantEvent::RetryRollback {
                attempt,
                max: opts.max_retries,
            });
        }
        sink.push(AssistantEvent::Start);
        handler.reset(model);
        saw_output = false;

        let mut events = CappedSse::new(response.bytes_stream());
        loop {
            let next = tokio::select! {
                () = opts.cancel.cancelled() => {
                    let mut message = handler.partial();
                    message.stop_reason = StopReason::Aborted;
                    message.error_message = Some("aborted".to_owned());
                    sink.push(AssistantEvent::Error { message });
                    return false;
                }
                next = events.next_events() => next,
            };
            match next {
                Some(Ok(batch)) => {
                    for event in batch {
                        handler.on_event(&event.event, &event.data, sink);
                        // Detect replay-visible output: if the handler's
                        // partial message now has content, mark saw_output so
                        // a subsequent transport error triggers rollback.
                        if !handler.partial().content.is_empty() {
                            saw_output = true;
                        }
                    }
                }
                Some(Err(CappedSseError::Oversized)) => {
                    let mut message = handler.partial();
                    message.stop_reason = StopReason::Error;
                    message.error_message = Some(format!(
                        "SSE event exceeded {MAX_SSE_EVENT_BYTES}-byte cap (possible server/relay abuse)"
                    ));
                    sink.push(AssistantEvent::Error { message });
                    return false;
                }
                Some(Err(CappedSseError::Transport(error))) => {
                    if attempt < opts.max_retries && is_transient_transport_error(&error) {
                        tokio::time::sleep(retry_delay(attempt, None)).await;
                        attempt += 1;
                        if retry_client.is_none()
                            && let Ok(c) = fresh_transport_client()
                        {
                            retry_client = Some(c);
                        }
                        // Break out of the inner streaming loop and re-enter
                        // the outer loop to reconnect and replay from the top.
                        break;
                    }
                    let mut message = handler.partial();
                    message.stop_reason = StopReason::Error;
                    message.error_message = Some(error.to_string());
                    sink.push(AssistantEvent::Error { message });
                    return false;
                }
                None => return true,
            }
        }
        // Mid-stream retry: the outer loop continues, reconnecting with the
        // fresh client and replaying from the top.
    }
}
