//! Minimal MCP client: JSON-RPC 2.0 over stdio (child process, one message
//! per line) or streamable HTTP (POST per message; responses arrive as JSON
//! or as an SSE stream). Just enough protocol for the tool surface —
//! initialize, tools/list, tools/call — plus ping replies so well-behaved
//! servers don't drop us.

use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use eventsource_stream::Eventsource as _;
use futures::StreamExt as _;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::sync::{mpsc, oneshot};

/// Latest protocol revision this client speaks; servers negotiate down in
/// the `initialize` response and we accept whatever they answer.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// How many trailing stderr lines from a stdio server to keep for error
/// reports (connect failures, mid-session death).
const STDERR_TAIL_LINES: usize = 20;

/// A tool advertised by a server's `tools/list`.
#[derive(Clone, Debug)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Transport parameters after config resolution (shell expansion done,
/// empty headers dropped).
#[derive(Debug)]
pub enum TransportConfig {
    Stdio {
        command: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
    },
    Http {
        url: String,
        headers: Vec<(String, String)>,
    },
}

pub struct McpClient {
    server: String,
    transport: Transport,
    next_id: AtomicI64,
}

impl McpClient {
    /// Spawn/dial the server, run the `initialize` handshake, and list its
    /// tools. `timeout` bounds each request in the sequence. The stdio child
    /// process lives as long as the returned client (`kill_on_drop`).
    pub async fn connect(
        server: &str,
        config: TransportConfig,
        timeout: Duration,
    ) -> Result<(Arc<Self>, Vec<McpTool>)> {
        let transport = match config {
            TransportConfig::Stdio { command, args, env } => {
                Transport::Stdio(StdioTransport::spawn(&command, &args, &env)?)
            }
            TransportConfig::Http { url, headers } => Transport::Http(HttpTransport {
                client: reqwest::Client::new(),
                url,
                headers,
                session_id: StdMutex::new(None),
                protocol_version: StdMutex::new(None),
            }),
        };
        let client = Arc::new(Self {
            server: server.to_owned(),
            transport,
            next_id: AtomicI64::new(1),
        });

        let init = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "phi", "version": env!("CARGO_PKG_VERSION")},
                }),
                timeout,
            )
            .await
            .context("initialize handshake")?;
        if let Transport::Http(http) = &client.transport
            && let Some(version) = init.get("protocolVersion").and_then(Value::as_str)
        {
            *http.protocol_version.lock().unwrap() = Some(version.to_owned());
        }
        client
            .notify("notifications/initialized", json!({}))
            .await
            .context("initialized notification")?;

        let tools = client.list_tools(timeout).await.context("tools/list")?;
        Ok((client, tools))
    }

    async fn list_tools(&self, timeout: Duration) -> Result<Vec<McpTool>> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let result = self.request("tools/list", params, timeout).await?;
            for tool in result
                .get("tools")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(name) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                tools.push(McpTool {
                    name: name.to_owned(),
                    description: tool
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    input_schema: tool
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                });
            }
            match result.get("nextCursor").and_then(Value::as_str) {
                Some(next) => cursor = Some(next.to_owned()),
                None => return Ok(tools),
            }
        }
    }

    /// Call a tool and flatten its result content to text. A result flagged
    /// `isError` becomes an `Err`, so REPL stubs raise and native dispatch
    /// reports a failed tool call.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Map<String, Value>,
        timeout: Duration,
    ) -> Result<String> {
        let result = self
            .request(
                "tools/call",
                json!({"name": name, "arguments": arguments}),
                timeout,
            )
            .await?;
        let text = content_text(&result);
        if result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            bail!(
                "{}",
                if text.is_empty() {
                    "tool reported an error".to_owned()
                } else {
                    text
                }
            );
        }
        Ok(text)
    }

    async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let message = self
            .transport
            .request(id, body, timeout)
            .await
            .with_context(|| format!("server '{}', method '{method}'", self.server))?;
        if let Some(error) = message.get("error").filter(|e| !e.is_null()) {
            let detail = error
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| error.to_string());
            bail!("server '{}': {method} failed: {detail}", self.server);
        }
        Ok(message.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let body = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.transport.notify(body).await
    }
}

/// Flatten a `tools/call` result's content blocks to text. Non-text blocks
/// are noted, not dropped silently; a result with no content blocks falls
/// back to its `structuredContent`, if any.
fn content_text(result: &Value) -> String {
    let mut parts = Vec::new();
    for block in result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                parts.push(
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                );
            }
            Some(other) => parts.push(format!("[{other} content omitted]")),
            None => {}
        }
    }
    if parts.is_empty()
        && let Some(structured) = result.get("structuredContent").filter(|v| !v.is_null())
    {
        return structured.to_string();
    }
    parts.join("\n")
}

enum Transport {
    Stdio(StdioTransport),
    Http(HttpTransport),
}

impl Transport {
    async fn request(&self, id: i64, body: Value, timeout: Duration) -> Result<Value> {
        match self {
            Self::Stdio(t) => t.request(id, body, timeout).await,
            Self::Http(t) => tokio::time::timeout(timeout, t.request(id, body))
                .await
                .map_err(|_| anyhow!("timed out after {timeout:?}"))?,
        }
    }

    async fn notify(&self, body: Value) -> Result<()> {
        match self {
            Self::Stdio(t) => t.send(body.to_string()),
            Self::Http(t) => t.notify(body).await,
        }
    }
}

// ── stdio ───────────────────────────────────────────────────────────────────

struct StdioTransport {
    /// Held for `kill_on_drop`: the child dies when the last tool handler
    /// holding this client is dropped.
    _child: tokio::process::Child,
    writer: mpsc::UnboundedSender<String>,
    pending: Arc<StdMutex<HashMap<i64, oneshot::Sender<Value>>>>,
    stderr_tail: Arc<StdMutex<VecDeque<String>>>,
}

impl StdioTransport {
    fn spawn(command: &str, args: &[String], env: &[(String, String)]) -> Result<Self> {
        let mut child = tokio::process::Command::new(command)
            .args(args)
            .envs(env.iter().cloned())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn '{command}'"))?;

        let stdin = child.stdin.take().context("child stdin")?;
        let stdout = child.stdout.take().context("child stdout")?;
        let stderr = child.stderr.take().context("child stderr")?;

        // Writer task: sole owner of stdin; both our requests and the
        // reader's ping replies go through this channel.
        let (writer, mut writer_rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = writer_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err()
                    || stdin.write_all(b"\n").await.is_err()
                    || stdin.flush().await.is_err()
                {
                    break;
                }
            }
        });

        // Reader task: routes responses to pending requests, answers pings,
        // refuses other server-initiated requests, ignores notifications.
        let pending: Arc<StdMutex<HashMap<i64, oneshot::Sender<Value>>>> =
            Arc::new(StdMutex::new(HashMap::new()));
        let reader_pending = pending.clone();
        let reader_writer = writer.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if message.get("result").is_some() || message.get("error").is_some() {
                    if let Some(id) = message.get("id").and_then(Value::as_i64)
                        && let Some(tx) = reader_pending.lock().unwrap().remove(&id)
                    {
                        let _ = tx.send(message);
                    }
                } else if let Some(method) = message.get("method").and_then(Value::as_str)
                    && let Some(id) = message.get("id").filter(|v| !v.is_null())
                {
                    let reply = if method == "ping" {
                        json!({"jsonrpc": "2.0", "id": id, "result": {}})
                    } else {
                        json!({"jsonrpc": "2.0", "id": id, "error": {
                            "code": -32601, "message": format!("method '{method}' not supported"),
                        }})
                    };
                    let _ = reader_writer.send(reply.to_string());
                }
            }
            // EOF: dropping the senders wakes every in-flight request with a
            // "server closed the connection" error.
            reader_pending.lock().unwrap().clear();
        });

        // Stderr drain: keep a short tail for error reports; never let a
        // chatty server block on a full pipe.
        let stderr_tail: Arc<StdMutex<VecDeque<String>>> = Arc::new(StdMutex::new(VecDeque::new()));
        let drain_tail = stderr_tail.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut tail = drain_tail.lock().unwrap();
                if tail.len() >= STDERR_TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        });

        Ok(Self {
            _child: child,
            writer,
            pending,
            stderr_tail,
        })
    }

    fn send(&self, line: String) -> Result<()> {
        self.writer
            .send(line)
            .map_err(|_| anyhow!("server process is gone{}", self.stderr_context()))
    }

    async fn request(&self, id: i64, body: Value, timeout: Duration) -> Result<Value> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if let Err(e) = self.send(body.to_string()) {
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(message)) => Ok(message),
            Ok(Err(_closed)) => {
                bail!("server closed the connection{}", self.stderr_context())
            }
            Err(_elapsed) => {
                self.pending.lock().unwrap().remove(&id);
                bail!("timed out after {timeout:?}")
            }
        }
    }

    fn stderr_context(&self) -> String {
        let tail = self.stderr_tail.lock().unwrap();
        if tail.is_empty() {
            String::new()
        } else {
            format!(
                " (stderr: {})",
                tail.iter().cloned().collect::<Vec<_>>().join(" | ")
            )
        }
    }
}

// ── streamable HTTP ─────────────────────────────────────────────────────────

struct HttpTransport {
    client: reqwest::Client,
    url: String,
    headers: Vec<(String, String)>,
    /// `Mcp-Session-Id` issued by the server on initialize, echoed on every
    /// subsequent request.
    session_id: StdMutex<Option<String>>,
    /// Negotiated protocol revision, sent as `MCP-Protocol-Version` after
    /// initialize.
    protocol_version: StdMutex<Option<String>>,
}

impl HttpTransport {
    fn post(&self, body: &Value) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        for (name, value) in &self.headers {
            req = req.header(name, value);
        }
        if let Some(sid) = self.session_id.lock().unwrap().clone() {
            req = req.header("mcp-session-id", sid);
        }
        if let Some(version) = self.protocol_version.lock().unwrap().clone() {
            req = req.header("mcp-protocol-version", version);
        }
        req.json(body)
    }

    fn capture_session_id(&self, response: &reqwest::Response) {
        if let Some(sid) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session_id.lock().unwrap() = Some(sid.to_owned());
        }
    }

    async fn request(&self, id: i64, body: Value) -> Result<Value> {
        let response = self.post(&body).send().await?;
        self.capture_session_id(&response);
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!(
                "HTTP {status}: {}",
                text.chars().take(200).collect::<String>()
            );
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        if content_type.starts_with("text/event-stream") {
            // The server streams; our response is one SSE event among
            // possibly many (progress notifications etc.).
            let mut events = response.bytes_stream().eventsource();
            while let Some(event) = events.next().await {
                let event = event.map_err(|e| anyhow!("event stream error: {e}"))?;
                let Ok(message) = serde_json::from_str::<Value>(&event.data) else {
                    continue;
                };
                if message.get("id").and_then(Value::as_i64) == Some(id)
                    && (message.get("result").is_some() || message.get("error").is_some())
                {
                    return Ok(message);
                }
            }
            bail!("event stream ended without a response")
        } else {
            Ok(response.json().await?)
        }
    }

    async fn notify(&self, body: Value) -> Result<()> {
        let response = self.post(&body).send().await?;
        self.capture_session_id(&response);
        let status = response.status();
        if !status.is_success() {
            bail!("HTTP {status} on notification");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::content_text;
    use serde_json::json;

    #[test]
    fn content_text_concatenates_text_blocks() {
        let result = json!({"content": [
            {"type": "text", "text": "line one"},
            {"type": "image", "data": "...", "mimeType": "image/png"},
            {"type": "text", "text": "line two"},
        ]});
        assert_eq!(
            content_text(&result),
            "line one\n[image content omitted]\nline two"
        );
    }

    #[test]
    fn content_text_falls_back_to_structured_content() {
        let result = json!({"content": [], "structuredContent": {"answer": 42}});
        assert_eq!(content_text(&result), r#"{"answer":42}"#);
    }

    #[test]
    fn content_text_empty_when_nothing_usable() {
        assert_eq!(content_text(&json!({})), "");
    }
}
