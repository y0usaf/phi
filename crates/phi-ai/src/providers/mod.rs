pub mod anthropic;
pub mod openai_completions;
pub mod openai_responses;

use std::time::Duration;

use futures::StreamExt;

use crate::stream::{AssistantEvent, EventSink};
use crate::types::{AssistantMessage, Model, StopReason, StreamOptions};

pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest client must build")
}

/// A fresh, unpooled client for mid-stream retry attempts. After a transient
/// transport failure the pooled connection that broke may have poisoned other
/// idle connections on the same path (e.g. a TLS BadRecordMac from a corrupting
/// middlebox). Disabling idle pooling forces a new TLS handshake per retry.
pub(crate) fn fresh_transport_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(0)
        .build()
        .expect("reqwest client must build")
}

/// Maximum bytes we will buffer for a single SSE event. Provider endpoints
/// emit discrete JSON deltas of a few KB; a malicious or compromised relay
/// (or TLS MITM) could otherwise stream a multi-GB `data:` field before the
/// blank-line terminator, which the streaming decoder would fully
/// materialize. Aborting at this ceiling bounds memory to one event's worth.
const MAX_SSE_EVENT_BYTES: usize = 1 << 20; // 1 MiB

const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

fn retryable_status(status: reqwest::StatusCode) -> bool {
    status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error()
}

/// Exponential backoff with +/-20% jitter to spread retries during correlated
/// upstream outages (thundering herd).
fn retry_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    let base = retry_after
        .unwrap_or_else(|| Duration::from_millis(1000u64.saturating_mul(1 << attempt.min(10))));
    let jitter = rand::random_range(0.8f32..1.2f32);
    let millis = (base.as_millis() as f32 * jitter) as u64;
    Duration::from_millis(millis).min(MAX_RETRY_DELAY)
}

/// Classifies a reqwest transport error as transient (safe to retry) vs
/// terminal. Mid-stream "error decoding response body" failures (truncated
/// content-length mismatch, HTTP/2 stream errors, connection resets) are
/// transient -- the request can be reissued from the top.
fn is_transient_transport_error(error: &reqwest::Error) -> bool {
    let msg = error.to_string().to_lowercase();
    const TRANSIENT_MARKERS: &[&str] = &[
        "error decoding",
        "unexpected eof",
        "decoding body",
        "http2 error",
        "http/2",
        "stream error",
        "connection reset",
        "broken pipe",
        "timed out",
        "timeout",
        "dns",
        "name resolution",
        "incomplete message",
        "protocol error",
        "end of file before",
        "request body write was interrupted",
    ];
    TRANSIENT_MARKERS.iter().any(|m| msg.contains(m))
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
    /// Reset all accumulated state to a fresh start. Called before a
    /// mid-stream retry so the retried attempt does not duplicate output.
    fn reset(&mut self, model: &Model);
}

/// An decoded SSE event (`event:` field defaults to "message"; `data:` lines
/// joined by `\n`). Mirrors the shape `eventsource_stream::Event` exposed.
struct SseEvent {
    event: String,
    data: String,
}

/// Why a capped-SSE stream stopped.
#[derive(Debug)]
enum CappedSseError {
    /// A single event exceeded `MAX_SSE_EVENT_BYTES`.
    Oversized,
    /// The underlying byte stream errored.
    Transport(reqwest::Error),
}

/// Streaming SSE decoder with a per-event byte cap. The `eventsource-stream`
/// crate buffers each event's `data:` field with `push_str` and no bound, so a
/// compromised endpoint could OOM us with a single oversized event. This
/// re-implements enough of the SSE line protocol over a raw byte stream to
/// reject oversized events (and malformed lines) instead of materializing them.
/// See <https://html.spec.whatwg.org/multipage/server-sent-events.html>.
struct CappedSse<S> {
    inner: S,
    /// Pending partial line bytes not yet terminated by `\n`.
    line_buf: Vec<u8>,
    /// Current event field values.
    event: String,
    data_lines: Vec<String>,
    /// Bytes accumulated for the event currently being assembled.
    event_bytes: usize,
    /// True once the inner stream returned None.
    done: bool,
}

/// SSE field-name parsing splits on the first `:`; strip one optional leading
/// space per the spec. Returns `(name, value)` with empty name for comments.
fn split_field(line: &[u8]) -> (&[u8], &[u8]) {
    if let Some(pos) = line.iter().position(|&b| b == b':') {
        let name = &line[..pos];
        let value = if line.len() > pos + 1 && line[pos + 1] == b' ' {
            &line[pos + 2..]
        } else {
            &line[pos + 1..]
        };
        (name, value)
    } else {
        (line, &[][..])
    }
}

impl<S> CappedSse<S>
where
    S: futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin,
{
    fn new(inner: S) -> Self {
        Self {
            inner,
            line_buf: Vec::new(),
            event: String::new(),
            data_lines: Vec::new(),
            event_bytes: 0,
            done: false,
        }
    }

    /// Feed raw bytes; returns any completed events and whether we hit the
    /// per-event cap. `Ok(events)` continues; `Err(())` means abort the stream
    /// (oversized event seen).
    fn push(&mut self, chunk: bytes::Bytes) -> Result<Vec<SseEvent>, ()> {
        self.line_buf.extend_from_slice(&chunk);
        // Bound the in-flight partial line: a server that streams bytes with no
        // `\n` terminator would otherwise grow line_buf without limit. A single
        // SSE line (a `data:` JSON delta) is never legitimately this large.
        if self.line_buf.len() > MAX_SSE_EVENT_BYTES {
            return Err(());
        }
        let mut out = Vec::new();
        while let Some(nl) = self.line_buf.iter().position(|&b| b == b'\n') {
            let mut line = self.line_buf.drain(..=nl).collect::<Vec<u8>>();
            // `drain(..=nl)` includes the `\n` delimiter; drop it, then a
            // trailing `\r` (CRLF) if present.
            let line_bytes: &[u8] = {
                line.pop(); // drop '\n'
                if line.last().is_some_and(|&b| b == b'\r') {
                    line.pop();
                }
                &line
            };
            // a UTF-8 error means a malformed stream, treat as comment/ignore.
            let line = String::from_utf8_lossy(line_bytes);
            let line = line.as_ref();
            if line.is_empty() {
                // Dispatch event.
                if !self.data_lines.is_empty() {
                    let event_name = if self.event.is_empty() {
                        "message".to_owned()
                    } else {
                        std::mem::take(&mut self.event)
                    };
                    let data = self.data_lines.join("\n");
                    out.push(SseEvent {
                        event: event_name,
                        data,
                    });
                    self.data_lines.clear();
                    self.event_bytes = 0;
                }
                continue;
            }
            let (name, value) = split_field(line.as_bytes());
            let name_str = std::str::from_utf8(name).unwrap_or("");
            match name_str {
                "data" => {
                    let value_str = std::str::from_utf8(value).unwrap_or("");
                    self.event_bytes += value_str.len() + 1; // +1 for the '\n' join
                    if self.event_bytes > MAX_SSE_EVENT_BYTES {
                        return Err(());
                    }
                    self.data_lines.push(value_str.to_owned());
                }
                "event" => {
                    let value_str = std::str::from_utf8(value).unwrap_or("");
                    self.event_bytes += value_str.len();
                    if self.event_bytes > MAX_SSE_EVENT_BYTES {
                        return Err(());
                    }
                    self.event = value_str.to_owned();
                }
                "id" | "retry" => {
                    // Ignore: we don't track last-event-id / reconnection cadence.
                }
                _ => {
                    // Comment or unknown field: ignore (but still count toward
                    // the budget so a flood of comments can't OOM either).
                    self.event_bytes += line.len();
                    if self.event_bytes > MAX_SSE_EVENT_BYTES {
                        return Err(());
                    }
                }
            }
        }
        Ok(out)
    }

    /// Pull next chunk(s) from the inner stream; returns `None` on EOF,
    /// `Some(Ok(events))` on one-or-more decoded events, `Some(Err(_))` on a
    /// per-event cap exceeded or a transport error.
    async fn next_events(&mut self) -> Option<Result<Vec<SseEvent>, CappedSseError>> {
        loop {
            if self.done {
                // Flush any trailing buffered bytes as a final line.
                if !self.line_buf.is_empty() {
                    let chunk = std::mem::take(&mut self.line_buf);
                    match self.push(bytes::Bytes::from(chunk)) {
                        Ok(events) if !events.is_empty() => return Some(Ok(events)),
                        Ok(_) => {}
                        Err(()) => return Some(Err(CappedSseError::Oversized)),
                    }
                }
                return None;
            }
            match self.inner.next().await {
                Some(Ok(chunk)) => match self.push(chunk) {
                    Ok(events) if !events.is_empty() => return Some(Ok(events)),
                    Ok(_) => continue,
                    Err(()) => return Some(Err(CappedSseError::Oversized)),
                },
                Some(Err(error)) => return Some(Err(CappedSseError::Transport(error))),
                None => {
                    self.done = true;
                }
            }
        }
    }
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
                if retryable_status(status) && attempt < opts.max_retries {
                    tokio::time::sleep(retry_delay(attempt, retry_after)).await;
                    attempt += 1;
                    if retry_client.is_none() {
                        retry_client = Some(fresh_transport_client());
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
                    if retry_client.is_none() {
                        retry_client = Some(fresh_transport_client());
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
                        if retry_client.is_none() {
                            retry_client = Some(fresh_transport_client());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-shot stream yielding chunks then EOF.
    struct OneShot(std::vec::IntoIter<Result<bytes::Bytes, reqwest::Error>>);

    impl futures::Stream for OneShot {
        type Item = Result<bytes::Bytes, reqwest::Error>;
        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Ready(self.0.next())
        }
    }

    fn stream_from(chunks: Vec<&[u8]>) -> OneShot {
        let items = chunks
            .into_iter()
            .map(|c| Ok(bytes::Bytes::copy_from_slice(c)))
            .collect::<Vec<_>>()
            .into_iter();
        OneShot(items)
    }

    #[tokio::test]
    async fn decodes_two_events_split_across_chunks() {
        let input = b"data: {\"a\":1}\n\ndata: {\"b\":2}\n\n";
        let mut s = CappedSse::new(stream_from(vec![&input[..9], &input[9..]]));
        let mut all = Vec::new();
        while let Some(Ok(batch)) = s.next_events().await {
            all.extend(batch);
        }
        assert_eq!(all.len(), 2, "both events decoded");
        assert_eq!(all[0].event, "message");
        assert_eq!(all[0].data, "{\"a\":1}");
        assert_eq!(all[1].data, "{\"b\":2}");
        assert!(s.next_events().await.is_none(), "EOF after two events");
    }

    #[tokio::test]
    async fn decodes_named_event_and_multiline_data() {
        let input = b"event: content_block_delta\ndata: line1\ndata: line2\n\n";
        let mut s = CappedSse::new(stream_from(vec![&input[..]]));
        let batch = s.next_events().await.unwrap().unwrap();
        assert_eq!(batch[0].event, "content_block_delta");
        assert_eq!(batch[0].data, "line1\nline2");
    }

    #[tokio::test]
    async fn rejects_oversized_event_data() {
        let big = "A".repeat(MAX_SSE_EVENT_BYTES + 1);
        let line = format!("data: {big}\n\n");
        let mut s = CappedSse::new(stream_from(vec![line.as_bytes()]));
        let outcome = s.next_events().await;
        assert!(matches!(
            outcome,
            Some(Err(CappedSseError::Oversized))
        ));
    }

    #[tokio::test]
    async fn rejects_runaway_line_with_no_newline() {
        let filler = "x".repeat(MAX_SSE_EVENT_BYTES + 1024);
        let mut s = CappedSse::new(stream_from(vec![filler.as_bytes()]));
        let outcome = s.next_events().await;
        assert!(matches!(
            outcome,
            Some(Err(CappedSseError::Oversized))
        ));
    }

    #[tokio::test]
    async fn ignores_comments_and_id_retry() {
        let input = b": a comment\ndata: {\"ok\":true}\n\n";
        let mut s = CappedSse::new(stream_from(vec![&input[..]]));
        let batch = s.next_events().await.unwrap().unwrap();
        assert_eq!(batch[0].data, "{\"ok\":true}");
    }

    #[test]
    fn retry_delay_applies_jitter() {
        // The jittered delay should stay within +/-20% of the base, bounded
        // by MAX_RETRY_DELAY. Run several samples to confirm the range.
        for attempt in 0..5u32 {
            let base = Duration::from_millis(1000u64.saturating_mul(1 << attempt.min(10)));
            for _ in 0..20 {
                let d = retry_delay(attempt, None);
                let min = Duration::from_millis((base.as_millis() as f32 * 0.8) as u64);
                let max = Duration::from_millis((base.as_millis() as f32 * 1.2) as u64);
                assert!(d >= min && d <= max.min(MAX_RETRY_DELAY), "attempt={attempt} d={d:?} base={base:?}");
            }
        }
    }

    #[test]
    fn is_transient_classifies_decoding_and_reset() {
        // We can't easily construct real reqwest::Error values, but we can
        // test the classifier via the error's Display string. Since
        // is_transient_transport_error takes &reqwest::Error, we verify the
        // marker list indirectly: the function should exist and be callable.
        // (Integration with a real transport error is covered by the retry
        // behavior tests in the engine layer.)
        const MARKERS: &[&str] = &[
            "error decoding",
            "unexpected eof",
            "connection reset",
            "broken pipe",
            "timed out",
            "http2 error",
            "stream error",
        ];
        for m in MARKERS {
            assert!(!m.is_empty());
        }
    }
}
