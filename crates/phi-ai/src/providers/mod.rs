pub mod anthropic;
pub mod openai_responses;

use std::time::Duration;

use eventsource_stream::Eventsource;
use futures::StreamExt;

use crate::stream::{AssistantEvent, EventSink};
use crate::types::{AssistantMessage, Model, StopReason, StreamOptions};

pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest client must build")
}

const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

fn retryable_status(status: reqwest::StatusCode) -> bool {
    status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error()
}

fn retry_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    retry_after
        .unwrap_or_else(|| Duration::from_millis(1000u64.saturating_mul(1 << attempt.min(10))))
        .min(MAX_RETRY_DELAY)
}

fn parse_retry_after(response: &reqwest::Response) -> Option<Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Parses provider-specific SSE events while `run_sse` owns transport,
/// retries, cancellation and the never-fail-out-of-band contract.
pub(crate) trait SseHandler {
    fn on_event(&mut self, event_name: &str, data: &str, sink: &EventSink);
    /// Best-effort snapshot of the message assembled so far (used when the
    /// stream dies mid-flight).
    fn partial(&self) -> AssistantMessage;
}

/// Sends the request with retries; on success, forwards SSE events to the
/// handler. All failures surface as a terminal `Error` event on `sink`.
/// Returns `false` when a terminal event was already emitted.
pub(crate) async fn run_sse<H: SseHandler>(
    client: &reqwest::Client,
    build_request: impl Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    model: &Model,
    opts: &StreamOptions,
    sink: &EventSink,
    handler: &mut H,
) -> bool {
    let mut attempt: u32 = 0;
    let response = loop {
        if opts.cancel.is_cancelled() {
            sink.error(model, StopReason::Aborted, "aborted");
            return false;
        }

        let request = build_request(client).timeout(opts.timeout);
        let result = tokio::select! {
            () = opts.cancel.cancelled() => {
                sink.error(model, StopReason::Aborted, "aborted");
                return false;
            }
            result = request.send() => result,
        };

        match result {
            Ok(response) if response.status().is_success() => break response,
            Ok(response) => {
                let status = response.status();
                let retry_after = parse_retry_after(&response);
                let body = response.text().await.unwrap_or_default();
                let error = if body.is_empty() {
                    format!("{} status code (no body)", status.as_u16())
                } else {
                    format!("HTTP {}: {}", status.as_u16(), body)
                };
                if retryable_status(status) && attempt < opts.max_retries {
                    tokio::time::sleep(retry_delay(attempt, retry_after)).await;
                    attempt += 1;
                    continue;
                }
                sink.error(model, StopReason::Error, error);
                return false;
            }
            Err(error) => {
                if attempt < opts.max_retries && !error.is_builder() {
                    tokio::time::sleep(retry_delay(attempt, None)).await;
                    attempt += 1;
                    continue;
                }
                sink.error(model, StopReason::Error, error.to_string());
                return false;
            }
        }
    };

    sink.push(AssistantEvent::Start);
    let mut events = response.bytes_stream().eventsource();
    loop {
        let next = tokio::select! {
            () = opts.cancel.cancelled() => {
                let mut message = handler.partial();
                message.stop_reason = StopReason::Aborted;
                message.error_message = Some("aborted".to_owned());
                sink.push(AssistantEvent::Error { message });
                return false;
            }
            next = events.next() => next,
        };
        match next {
            Some(Ok(event)) => handler.on_event(&event.event, &event.data, sink),
            Some(Err(error)) => {
                let mut message = handler.partial();
                message.stop_reason = StopReason::Error;
                message.error_message = Some(error.to_string());
                sink.push(AssistantEvent::Error { message });
                return false;
            }
            None => return true,
        }
    }
}
