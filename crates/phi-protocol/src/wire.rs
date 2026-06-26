//! Wire protocol types: `Request` (client -> server) and `ServerEvent`
//! (server -> client), serialized as newline-delimited JSON.
//!
//! Mirrors `ref/jcode`'s `jcode-protocol` crate. The server streams
//! `ServerEvent`s back to clients during message processing; clients send
//! `Request`s to drive the conversation.

use serde::{Deserialize, Serialize};

/// A message in conversation history (for sync on connect/reconnect).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryMessage {
    pub role: String,
    pub content: String,
}

/// Client request to the server.
///
/// Tagged with `"type"` so the server can `serde_json::from_str` into the
/// enum and dispatch on variant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum Request {
    /// Send a user message to the agent.
    #[serde(rename = "message")]
    Message { id: u64, content: String },

    /// Cancel the current generation.
    #[serde(rename = "cancel")]
    Cancel { id: u64 },

    /// Clear conversation history.
    #[serde(rename = "clear")]
    Clear { id: u64 },

    /// Health check.
    #[serde(rename = "ping")]
    Ping { id: u64 },

    /// Get current server state (debug).
    #[serde(rename = "state")]
    GetState { id: u64 },

    /// Subscribe to streamed events (TUI clients).
    #[serde(rename = "subscribe")]
    Subscribe {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_session_id: Option<String>,
    },

    /// Get full conversation history (for TUI sync on connect).
    #[serde(rename = "get_history")]
    GetHistory { id: u64 },

    /// Resume a specific session by id.
    #[serde(rename = "resume_session")]
    ResumeSession { id: u64, session_id: String },

    /// Shutdown the server.
    #[serde(rename = "shutdown")]
    Shutdown { id: u64 },
}

/// Server event streamed to clients.
///
/// Derived from the RLM engine's `RlmEvent` stream: the server translates
/// internal engine events into these wire events so clients can render
/// progress without knowing about the RLM internals.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ServerEvent {
    /// Acknowledgment of a request.
    #[serde(rename = "ack")]
    Ack { id: u64 },

    /// Pong response to a ping.
    #[serde(rename = "pong")]
    Pong { id: u64 },

    /// Streaming text delta from the root model.
    #[serde(rename = "text_delta")]
    TextDelta { text: String },

    /// Streaming reasoning/thinking delta.
    #[serde(rename = "reasoning_delta")]
    ReasoningDelta { text: String },

    /// Full root-model text for the current iteration.
    #[serde(rename = "root_text")]
    RootText { text: String },

    /// A REPL code block started executing.
    #[serde(rename = "code_exec_start")]
    CodeExecStart { code: String },

    /// A REPL code block finished executing.
    #[serde(rename = "code_exec_end")]
    CodeExecEnd {
        stdout: String,
        stderr: String,
        duration_secs: f64,
    },

    /// A tool call started.
    #[serde(rename = "tool_start")]
    ToolStart {
        call_id: u64,
        name: String,
        args: serde_json::Value,
    },

    /// A tool call ended.
    #[serde(rename = "tool_end")]
    ToolEnd {
        call_id: u64,
        name: String,
        result: String,
        error: Option<String>,
        duration_secs: f64,
    },

    /// A sub-LLM call (llm_query/rlm_query) started.
    #[serde(rename = "subcall_start")]
    SubcallStart {
        model: String,
        prompt_preview: String,
    },

    /// A sub-LLM call finished.
    #[serde(rename = "subcall_end")]
    SubcallEnd {
        model: String,
        duration_secs: f64,
        error: Option<String>,
    },

    /// Token usage update.
    #[serde(rename = "tokens")]
    TokenUsage { input: u64, output: u64 },

    /// The final answer was submitted.
    #[serde(rename = "answer_submitted")]
    AnswerSubmitted { content: String },

    /// A limit was hit (iteration/depth/budget/etc.).
    #[serde(rename = "limit_hit")]
    LimitHit { limit: String, detail: String },

    /// The current turn completed successfully.
    #[serde(rename = "done")]
    Done { id: u64 },

    /// The current turn failed.
    #[serde(rename = "error")]
    Error {
        id: u64,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        retry_after_secs: Option<u64>,
    },

    /// Conversation history snapshot (response to GetHistory).
    #[serde(rename = "history")]
    History { messages: Vec<HistoryMessage> },

    /// Server state snapshot (response to GetState).
    #[serde(rename = "state")]
    State {
        is_processing: bool,
        session_id: String,
        client_count: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_message_round_trip() {
        let req = Request::Message { id: 1, content: "hello".to_string() };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"type":"message","id":1,"content":"hello"}"#);
        let back: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn request_ping_round_trip() {
        let req = Request::Ping { id: 5 };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"type":"ping","id":5}"#);
        let back: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn request_subscribe_omits_none_fields() {
        let req = Request::Subscribe {
            id: 2,
            working_dir: None,
            target_session_id: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"type":"subscribe","id":2}"#);
    }

    #[test]
    fn server_event_pong_round_trip() {
        let ev = ServerEvent::Pong { id: 42 };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(json, r#"{"type":"pong","id":42}"#);
        let back: ServerEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(ev, back);
    }

    #[test]
    fn server_event_text_delta_round_trip() {
        let ev = ServerEvent::TextDelta { text: "chunk".to_string() };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(json, r#"{"type":"text_delta","text":"chunk"}"#);
    }

    #[test]
    fn server_event_done_round_trip() {
        let ev = ServerEvent::Done { id: 1 };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(json, r#"{"type":"done","id":1}"#);
        let back: ServerEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(ev, back);
    }

    #[test]
    fn server_event_error_omits_retry_when_none() {
        let ev = ServerEvent::Error {
            id: 1,
            message: "boom".to_string(),
            retry_after_secs: None,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(json, r#"{"type":"error","id":1,"message":"boom"}"#);
    }

    #[test]
    fn server_event_error_includes_retry() {
        let ev = ServerEvent::Error {
            id: 1,
            message: "rate limited".to_string(),
            retry_after_secs: Some(30),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(
            json,
            r#"{"type":"error","id":1,"message":"rate limited","retry_after_secs":30}"#
        );
    }

    #[test]
    fn decode_request_from_ndjson_line() {
        let line = r#"{"type":"message","id":7,"content":"hi"}"#;
        let req = crate::decode_request(line).unwrap();
        match req {
            Request::Message { id, content } => {
                assert_eq!(id, 7);
                assert_eq!(content, "hi");
            }
            _ => panic!("expected Message"),
        }
    }

    #[test]
    fn decode_request_ignores_trailing_whitespace() {
        let line = r#"{"type":"ping","id":1}"#;
        let req = crate::decode_request(line).unwrap();
        assert_eq!(req, Request::Ping { id: 1 });
    }

    #[test]
    fn decode_request_rejects_garbage() {
        assert!(crate::decode_request("not json").is_err());
    }

    #[test]
    fn encode_event_produces_ndjson() {
        let ev = ServerEvent::Ack { id: 3 };
        let line = crate::encode_event(&ev).unwrap();
        assert!(line.ends_with("
"));
        assert_eq!(line.trim(), r#"{"type":"ack","id":3}"#);
    }

    #[test]
    fn history_message_round_trip() {
        let msg = HistoryMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: HistoryMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }
}
