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
/// Parses provider-specific SSE events while `run_sse` owns transport,
/// retries, cancellation and the never-fail-out-of-band contract.
pub(crate) trait SseHandler {
    fn on_event(&mut self, event_name: &str, data: &str, sink: &EventSink);
    /// Best-effort snapshot of the message assembled so far (used when the
    /// stream dies mid-flight).
    fn partial(&self) -> AssistantMessage;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-shot stream yielding a single chunk then EOF.
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
        // Collect every event across all batches until EOF.
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
        // Build one event whose data field exceeds the cap.
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
        // A flood of bytes with no newline terminator must not grow line_buf
        // without bound; it should abort at MAX_SSE_EVENT_BYTES.
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
}
