//! Cursor native API provider: streams from Cursor's connect+proto endpoint.
//!
//! Unlike other providers that use run_sse (SSE over HTTP/2), Cursor uses a
//! custom connect+proto protocol over HTTP/2 with gzipped protobuf frames.
//! We spawn `curl` for HTTP/2 + gzip support and parse the raw byte stream.

mod auth;
mod proto;
mod think_router;

use anyhow::{Context, Result};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use uuid::Uuid;

use crate::registry::ApiProvider;
use crate::stream::{AssistantEvent, AssistantEventStream, EventSink};
use crate::types::{Api, Context as LmContext, Model, StreamOptions};

use auth::{checksum_for_access_token, client_key_for_access_token, cursor_direct_client_version};
use proto::{
    build_native_request_body, decode_next_frame, decode_protobuf_events, parse_json_error_frame,
};
use think_router::ThinkRouter;

const DIRECT_CHAT_URL: &str =
    "https://api2.cursor.sh/aiserver.v1.ChatService/StreamUnifiedChatWithTools";
const MAX_PROMPT_CHARS: usize = 120_000;

pub struct CursorProvider {
    client: reqwest::Client,
}

impl CursorProvider {
    pub fn new() -> Self {
        Self {
            client: crate::providers::http_client(),
        }
    }
}

impl Default for CursorProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiProvider for CursorProvider {
    fn api(&self) -> Api {
        Api::CursorNative
    }

    fn stream(&self, model: &Model, ctx: &LmContext, opts: StreamOptions) -> AssistantEventStream {
        let (sink, stream) = AssistantEventStream::channel();
        let client = self.client.clone();
        let model = model.clone();
        let ctx = ctx.clone();
        let auth_secret = opts.auth.secret().to_owned();
        tokio::spawn(async move {
            run(client, model, ctx, auth_secret, sink).await;
        });
        stream
    }
}

/// Flatten messages into a single prompt string for Cursor's single-turn API.
fn build_prompt(system: &str, messages: &[crate::types::Message]) -> String {
    use crate::types::Message;

    let mut out = String::new();

    if !system.trim().is_empty() {
        out.push_str("System:\n");
        out.push_str(system.trim());
        out.push_str("\n\n");
    }

    out.push_str("Conversation:\n");

    for message in messages {
        match message {
            Message::User(user) => {
                out.push_str("User:\n");
                out.push_str(&user.content);
                out.push('\n');
            }
            Message::Assistant(assistant) => {
                out.push_str("Assistant:\n");
                for block in &assistant.content {
                    match block {
                        crate::types::AssistantContent::Text { text } => {
                            out.push_str(text);
                            out.push('\n');
                        }
                        crate::types::AssistantContent::Thinking { .. } => {}
                        crate::types::AssistantContent::ToolCall(call) => {
                            out.push_str("[tool_use ");
                            out.push_str(&call.name);
                            out.push_str(" input=");
                            out.push_str(&call.arguments.to_string());
                            out.push_str("]\n");
                        }
                    }
                }
                out.push('\n');
            }
            Message::ToolResult(result) => {
                out.push_str("[tool_result ");
                out.push_str(&result.tool_use_id);
                out.push_str(" is_error=");
                out.push_str(if result.is_error { "true" } else { "false" });
                out.push_str("]\n");
                out.push_str(&result.content);
                out.push('\n');
            }
        }
    }

    out.push_str("Assistant:\n");

    if out.chars().count() <= MAX_PROMPT_CHARS {
        return out;
    }

    let mut kept: Vec<char> = out.chars().rev().take(MAX_PROMPT_CHARS).collect();
    kept.reverse();
    let tail: String = kept.into_iter().collect();
    format!(
        "[Earlier conversation truncated to fit prompt limits]\n\n{}",
        tail
    )
}

async fn run(
    client: reqwest::Client,
    model: Model,
    ctx: LmContext,
    auth_secret: String,
    sink: EventSink,
) {
    let system = ctx.system_prompt.as_deref().unwrap_or("");
    let prompt = build_prompt(system, &ctx.messages);
    let body = build_native_request_body(&prompt, &model.id);

    sink.push(AssistantEvent::Start);

    let result =
        run_native_text_command(&client, &sink, &auth_secret, &prompt, &model.id, body).await;

    match result {
        Ok(()) => {
            // The streaming loop already pushed all text/thinking events
            // via the ThinkRouter. Emit a terminal Done with an empty message
            // (the consumer accumulates from deltas).
            sink.push(AssistantEvent::Done {
                message: crate::types::AssistantMessage::empty(&model),
            });
        }
        Err(err) => {
            sink.error(&model, crate::types::StopReason::Error, format!("{err:#}"));
        }
    }
}

async fn run_native_text_command(
    client: &reqwest::Client,
    sink: &EventSink,
    auth_secret: &str,
    _prompt: &str,
    model: &str,
    body: Vec<u8>,
) -> Result<()> {
    // If the auth_secret from Phi's login/storage is a non-empty JWT,
    // use it directly. Otherwise, fall back to resolving tokens from
    // env/IDE/auth.json (jcode-style discovery).
    let tokens = if !auth_secret.is_empty() && auth_secret.starts_with("eyJ") {
        auth::CursorDirectTokens {
            access_token: auth_secret.to_owned(),
            refresh_token: None,
            source: "phi_oauth",
        }
    } else {
        auth::resolve_direct_tokens(client).await?
    };
    let access_token = &tokens.access_token;
    let session_id = auth::session_id_for_access_token(access_token);
    let request_id = Uuid::new_v4().to_string();
    let config_version = Uuid::new_v4().to_string();

    let first_result = run_native_text_command_via_curl(
        sink,
        access_token,
        &session_id,
        &request_id,
        &config_version,
        body.clone(),
        model,
    )
    .await;

    match first_result {
        Ok(()) => Ok(()),
        Err(err) if auth::error_indicates_not_logged_in(&err) => {
            let refreshed = auth::refresh_resolved_tokens(client, &tokens)
                .await
                .with_context(|| {
                    format!("Cursor token was rejected and refresh also failed after: {err:#}")
                })?;
            run_native_text_command_via_curl(
                sink,
                &refreshed.access_token,
                &auth::session_id_for_access_token(&refreshed.access_token),
                &Uuid::new_v4().to_string(),
                &Uuid::new_v4().to_string(),
                body,
                model,
            )
            .await
        }
        Err(err) => Err(err),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_native_text_command_via_curl(
    sink: &EventSink,
    access_token: &str,
    session_id: &str,
    request_id: &str,
    config_version: &str,
    body: Vec<u8>,
    _model: &str,
) -> Result<()> {
    let checksum = checksum_for_access_token(access_token);
    let client_key = client_key_for_access_token(access_token);
    let client_version = cursor_direct_client_version();

    let body_path = std::env::temp_dir().join(format!("phi-cursor-{}.bin", Uuid::new_v4()));
    std::fs::write(&body_path, &body).context("Failed writing Cursor request body temp file")?;
    let body_path_str = body_path.to_string_lossy().to_string();

    let mut cmd = Command::new("curl");
    cmd.kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .arg("--http2")
        .arg("--no-progress-meter")
        .arg("-sS")
        .arg("-X")
        .arg("POST")
        .arg(DIRECT_CHAT_URL)
        .arg("-H")
        .arg(format!("authorization: Bearer {access_token}"))
        .arg("-H")
        .arg("accept-encoding: gzip")
        .arg("-H")
        .arg("connect-accept-encoding: gzip")
        .arg("-H")
        .arg("connect-protocol-version: 1")
        .arg("-H")
        .arg("content-type: application/connect+proto")
        .arg("-H")
        .arg("user-agent: connect-es/1.6.1")
        .arg("-H")
        .arg(format!("x-amzn-trace-id: Root={}", Uuid::new_v4()))
        .arg("-H")
        .arg(format!("x-client-key: {client_key}"))
        .arg("-H")
        .arg(format!("x-cursor-checksum: {checksum}"))
        .arg("-H")
        .arg(format!("x-cursor-client-version: {client_version}"))
        .arg("-H")
        .arg(format!("x-cursor-config-version: {config_version}"))
        .arg("-H")
        .arg("x-cursor-client-type: ide")
        .arg("-H")
        .arg(format!("x-cursor-client-os: {}", std::env::consts::OS))
        .arg("-H")
        .arg(format!("x-cursor-client-arch: {}", std::env::consts::ARCH))
        .arg("-H")
        .arg("x-cursor-client-device-type: desktop")
        .arg("-H")
        .arg("x-cursor-timezone: UTC")
        .arg("-H")
        .arg("x-ghost-mode: true")
        .arg("-H")
        .arg(format!("x-request-id: {request_id}"))
        .arg("-H")
        .arg(format!("x-session-id: {session_id}"))
        .arg("--data-binary")
        .arg(format!("@{body_path_str}"));

    let mut child = cmd
        .spawn()
        .context("Failed to spawn curl for Cursor native API")?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture curl stdout"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture curl stderr"))?;

    let stderr_task = tokio::spawn(async move {
        let mut collected = Vec::new();
        let _ = stderr.read_to_end(&mut collected).await;
        String::from_utf8_lossy(&collected).to_string()
    });

    let mut router = ThinkRouter::new();
    let mut pending = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let read = stdout
            .read(&mut buf)
            .await
            .context("Failed to read curl Cursor response stream")?;
        if read == 0 {
            break;
        }
        pending.extend_from_slice(&buf[..read]);
        drain_native_frames(sink, &mut pending, &mut router)?;
    }

    let status = child
        .wait()
        .await
        .context("Failed waiting for curl process")?;
    let _ = std::fs::remove_file(&body_path);
    let stderr_text = stderr_task.await.unwrap_or_default();
    if !status.success() {
        anyhow::bail!(
            "curl Cursor native API failed with status {}: {}",
            status,
            stderr_text.trim()
        );
    }

    if !pending.is_empty() {
        drain_native_frames(sink, &mut pending, &mut router)?;
    }
    router.finish(sink);

    Ok(())
}

fn drain_native_frames(
    sink: &EventSink,
    pending: &mut Vec<u8>,
    router: &mut ThinkRouter,
) -> Result<()> {
    loop {
        let Some((frame_type, payload, consumed)) = decode_next_frame(pending)? else {
            break;
        };
        pending.drain(..consumed);
        match frame_type {
            0 | 1 => {
                let result = decode_protobuf_events(&payload)?;
                if let Some(err) = result.error {
                    anyhow::bail!("{}", err);
                }
                for chunk in result.text_chunks {
                    router.push_chunk(&chunk, sink);
                }
            }
            2 | 3 => {
                if payload != b"{}"
                    && let Some(err_msg) = parse_json_error_frame(&payload)?
                {
                    anyhow::bail!("{}", err_msg);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

// Suppress unused import when Accumulator is only used in tests.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::Accumulator;
    use crate::types::{Api, CostRates, Model as TestModel};

    fn test_model() -> TestModel {
        TestModel {
            id: "composer-2.5".into(),
            name: "Composer 2.5".into(),
            api: Api::CursorNative,
            provider: "cursor".into(),
            base_url: "https://api2.cursor.sh".into(),
            reasoning: true,
            input: vec![],
            cost: CostRates::default(),
            context_window: 200_000,
            max_tokens: 8192,
            headers: Default::default(),
            thinking_levels: None,
            adaptive_thinking: false,
        }
    }

    #[test]
    fn build_prompt_formats_system_and_messages() {
        use crate::types::{Message, UserMessage};
        let messages = vec![Message::User(UserMessage::new("Hello"))];
        let prompt = build_prompt("You are helpful", &messages);
        assert!(prompt.contains("System:\nYou are helpful"));
        assert!(prompt.contains("User:\nHello"));
        assert!(prompt.contains("Assistant:\n"));
    }

    #[test]
    fn build_prompt_truncates_long_input() {
        let long = "x".repeat(MAX_PROMPT_CHARS + 1000);
        use crate::types::{Message, UserMessage};
        let messages = vec![Message::User(UserMessage::new(long))];
        let prompt = build_prompt("", &messages);
        assert!(prompt.contains("[Earlier conversation truncated"));
    }

    #[test]
    fn cursor_model_in_registry() {
        let registry = crate::registry::Registry::builtin();
        // composer-2.5 should be findable.
        let model = registry.find("composer-2.5");
        assert!(model.is_some(), "composer-2.5 should be in models.json");
        let model = model.unwrap();
        assert_eq!(model.api, Api::CursorNative);
        assert_eq!(model.provider, "cursor");
        assert!(registry.is_supported(model));
    }

    #[test]
    fn cursor_provider_registered() {
        let registry = crate::registry::Registry::builtin();
        let model = registry.first_for_provider("cursor");
        assert!(model.is_some(), "cursor provider should have models");
        let model = model.unwrap();
        assert!(registry.is_supported(model));
    }

    #[test]
    fn accumulator_works_with_cursor_model() {
        let model = test_model();
        let mut acc = Accumulator::new(&model);
        acc.apply(&AssistantEvent::TextStart { index: 0 });
        acc.apply(&AssistantEvent::TextDelta {
            index: 0,
            delta: "hi".into(),
        });
        let msg = acc.into_message();
        assert_eq!(msg.text(), "hi");
    }
}
