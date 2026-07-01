//! Routes streaming text chunks into thinking vs. text segments based on
//! `<think>`/`</think>` markers in Cursor's output.

use crate::stream::{AssistantEvent, EventSink};

const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";

#[derive(Default)]
pub struct ThinkRouter {
    in_think: bool,
    carry: String,
    /// Content-block index for the current text or thinking block.
    index: usize,
}

impl ThinkRouter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_chunk(&mut self, chunk: &str, sink: &EventSink) {
        self.route(Some(chunk), sink);
    }

    pub fn finish(&mut self, sink: &EventSink) {
        self.route(None, sink);
    }

    fn route(&mut self, chunk: Option<&str>, sink: &EventSink) {
        if let Some(chunk) = chunk {
            self.carry.push_str(chunk);
        }
        loop {
            if self.in_think {
                if let Some(idx) = self.carry.find(THINK_CLOSE) {
                    let text = self.carry[..idx].to_string();
                    if !text.is_empty() {
                        sink.push(AssistantEvent::ThinkingDelta {
                            index: self.index,
                            delta: text,
                        });
                    }
                    sink.push(AssistantEvent::ThinkingEnd {
                        index: self.index,
                        content: String::new(),
                    });
                    self.carry = self.carry[idx + THINK_CLOSE.len()..].to_string();
                    self.in_think = false;
                    self.index += 1;
                    continue;
                }
                let split = carry_boundary(&self.carry, THINK_CLOSE);
                if split > 0 {
                    let text = self.carry[..split].to_string();
                    if !text.is_empty() {
                        sink.push(AssistantEvent::ThinkingDelta {
                            index: self.index,
                            delta: text,
                        });
                    }
                    self.carry = self.carry[split..].to_string();
                }
                break;
            }

            if let Some(idx) = self.carry.find(THINK_OPEN) {
                let text = self.carry[..idx].to_string();
                if !text.is_empty() {
                    sink.push(AssistantEvent::TextStart { index: self.index });
                    sink.push(AssistantEvent::TextDelta {
                        index: self.index,
                        delta: text,
                    });
                    sink.push(AssistantEvent::TextEnd {
                        index: self.index,
                        content: String::new(),
                    });
                    self.index += 1;
                }
                sink.push(AssistantEvent::ThinkingStart { index: self.index });
                self.carry = self.carry[idx + THINK_OPEN.len()..].to_string();
                self.in_think = true;
                continue;
            }

            let split = carry_boundary(&self.carry, THINK_OPEN);
            if split > 0 {
                let text = self.carry[..split].to_string();
                if !text.is_empty() {
                    sink.push(AssistantEvent::TextStart { index: self.index });
                    sink.push(AssistantEvent::TextDelta {
                        index: self.index,
                        delta: text,
                    });
                    sink.push(AssistantEvent::TextEnd {
                        index: self.index,
                        content: String::new(),
                    });
                    self.carry = self.carry[split..].to_string();
                }
            }
            break;
        }

        if chunk.is_none() && !self.carry.is_empty() {
            if self.in_think {
                sink.push(AssistantEvent::ThinkingDelta {
                    index: self.index,
                    delta: std::mem::take(&mut self.carry),
                });
                sink.push(AssistantEvent::ThinkingEnd {
                    index: self.index,
                    content: String::new(),
                });
                self.in_think = false;
            } else {
                sink.push(AssistantEvent::TextStart { index: self.index });
                sink.push(AssistantEvent::TextDelta {
                    index: self.index,
                    delta: std::mem::take(&mut self.carry),
                });
                sink.push(AssistantEvent::TextEnd {
                    index: self.index,
                    content: String::new(),
                });
            }
        }
    }
}

fn carry_boundary(text: &str, marker: &str) -> usize {
    let max = marker.len().saturating_sub(1).min(text.len());
    for keep in (1..=max).rev() {
        if text.ends_with(&marker[..keep]) {
            return text.len() - keep;
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_no_markers() {
        let (sink, _stream) = crate::stream::AssistantEventStream::channel();
        let mut router = ThinkRouter::new();
        router.push_chunk("Hello, world!", &sink);
        router.finish(&sink);
    }

    #[test]
    fn think_block_crosses_chunks() {
        let (sink, _stream) = crate::stream::AssistantEventStream::channel();
        let mut router = ThinkRouter::new();
        router.push_chunk("Hmm <thi", &sink);
        router.push_chunk("nk>secret</thi", &sink);
        router.push_chunk("nk>visible", &sink);
        router.finish(&sink);
    }

    #[test]
    fn leading_think_block() {
        let (sink, _stream) = crate::stream::AssistantEventStream::channel();
        let mut router = ThinkRouter::new();
        router.push_chunk("<think>reasoning</think>answer", &sink);
        router.finish(&sink);
    }
}
