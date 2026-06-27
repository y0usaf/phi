//! SSE (Server-Sent Events) decoder with a per-event byte cap.

use futures::StreamExt;

use crate::stream::EventSink;
use crate::types::{AssistantMessage, Model};

/// Maximum bytes we will buffer for a single SSE event. Provider endpoints
/// emit discrete JSON deltas of a few KB; a malicious or compromised relay
/// (or TLS MITM) could otherwise stream a multi-GB `data:` field before the
/// blank-line terminator, which the streaming decoder would fully
/// materialize. Aborting at this ceiling bounds memory to one event's worth.
pub(crate) const MAX_SSE_EVENT_BYTES: usize = 1 << 20; // 1 MiB

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
pub(crate) struct SseEvent {
    pub event: String,
    pub data: String,
}

/// Why a capped-SSE stream stopped.
#[derive(Debug)]
pub(crate) enum CappedSseError {
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
pub(crate) struct CappedSse<S> {
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
    pub(crate) fn new(inner: S) -> Self {
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
    pub(crate) async fn next_events(&mut self) -> Option<Result<Vec<SseEvent>, CappedSseError>> {
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
        assert!(matches!(outcome, Some(Err(CappedSseError::Oversized))));
    }

    #[tokio::test]
    async fn rejects_runaway_line_with_no_newline() {
        let filler = "x".repeat(MAX_SSE_EVENT_BYTES + 1024);
        let mut s = CappedSse::new(stream_from(vec![filler.as_bytes()]));
        let outcome = s.next_events().await;
        assert!(matches!(outcome, Some(Err(CappedSseError::Oversized))));
    }

    #[tokio::test]
    async fn ignores_comments_and_id_retry() {
        let input = b": a comment\ndata: {\"ok\":true}\n\n";
        let mut s = CappedSse::new(stream_from(vec![&input[..]]));
        let batch = s.next_events().await.unwrap().unwrap();
        assert_eq!(batch[0].data, "{\"ok\":true}");
    }
}
