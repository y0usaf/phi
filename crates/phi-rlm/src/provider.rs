//! Provider-backed `LanguageModel`: completions through the phi-ai registry,
//! with auth resolved per call (so OAuth tokens refresh mid-run).

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use phi_ai::{
    AssistantContent, AssistantEvent, AssistantMessage, AuthStorage, Context, Message, Model,
    Registry, StreamOptions, ThinkingLevel, ToolCall, ToolDefinition, ToolResultMessage,
    UserMessage,
};
use tokio_util::sync::CancellationToken;

use crate::lm_core::{ChatMessage, Role};
use crate::lm_core::{
    Completion, DeltaSink, LanguageModel, ThinkingSink, ToolCallEvent, ToolCallSink,
};

pub struct ProviderLm {
    registry: Arc<Registry>,
    model: RwLock<Model>,
    auth: Arc<AuthStorage>,
    thinking: RwLock<ThinkingLevel>,
    /// Optional per-request output-token cap; overrides `model.max_tokens`
    /// (which is sourced from upstream model listings that are sometimes
    /// wrong — e.g. neuralwatt advertises 1M for glm-5.2 but only serves
    /// 200k). `None` falls back to `model.max_tokens`.
    max_tokens: Option<u64>,
    /// Tool definitions sent to the API so the model can call them natively.
    tools: RwLock<Vec<ToolDefinition>>,
    /// Optional extension runtime for provider request/response interception
    /// and message streaming events. Set after construction via `set_runtime`.
    runtime: RwLock<Option<Arc<phi_ext::AppRuntime>>>,
}

/// Transient-looking provider failures are retried; auth/4xx errors are not.
fn is_transient(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    // A quota/billing 429 is terminal — waiting cannot fix it, so don't
    // burn the backoff schedule before telling the user.
    if phi_ai::providers::is_terminal_rate_limit(&e) {
        return false;
    }
    let codes = ["429", "500", "502", "503", "504", "529"];
    codes.iter().any(|c| e.contains(c))
        || e.contains("overloaded")
        || e.contains("rate limit")
        || e.contains("rate_limit")
        || e.contains("timed out")
        || e.contains("timeout")
        || e.contains("connection")
        || e.contains("temporarily")
        || e.contains("stream ended without a terminal event")
}

const RETRIES: u32 = 3;
const BASE_BACKOFF_MS: u64 = 500;

impl ProviderLm {
    pub fn new(
        registry: Arc<Registry>,
        model: Model,
        auth: Arc<AuthStorage>,
        thinking: ThinkingLevel,
        max_tokens: Option<u64>,
    ) -> Self {
        Self {
            registry,
            model: RwLock::new(model),
            auth,
            thinking: RwLock::new(thinking),
            max_tokens,
            tools: RwLock::new(Vec::new()),
            runtime: RwLock::new(None),
        }
    }

    /// Set the extension runtime for provider interception and message
    /// streaming events. Called after construction once the runtime is built.
    pub fn set_runtime(&self, runtime: Arc<phi_ext::AppRuntime>) {
        *self.runtime.write().unwrap() = Some(runtime);
    }

    /// Set the tool definitions the model can call natively.
    pub fn set_tools(&self, tools: Vec<ToolDefinition>) {
        *self.tools.write().unwrap() = tools;
    }

    /// Whether any tool definitions are registered.
    pub fn has_tools(&self) -> bool {
        !self.tools.read().unwrap().is_empty()
    }

    pub fn model(&self) -> Model {
        self.model.read().unwrap().clone()
    }

    pub fn thinking(&self) -> ThinkingLevel {
        *self.thinking.read().unwrap()
    }

    /// Swap the thinking level; takes effect on the next completion.
    pub fn set_thinking(&self, level: ThinkingLevel) {
        *self.thinking.write().unwrap() = level;
    }

    /// Swap the model; takes effect on the next completion (auth is
    /// resolved per call, so no other state needs rebuilding).
    pub fn set_model(&self, model: Model) {
        *self.model.write().unwrap() = model;
    }

    fn context(model: &Model, prompt: &[ChatMessage]) -> Context {
        let mut ctx = Context::default();
        for message in prompt {
            match message.role {
                Role::System => match &mut ctx.system_prompt {
                    Some(existing) => {
                        existing.push_str("\n\n");
                        existing.push_str(&message.content);
                    }
                    None => ctx.system_prompt = Some(message.content.clone()),
                },
                Role::User => ctx
                    .messages
                    .push(Message::User(UserMessage::new(message.content.clone()))),
                Role::Assistant => {
                    let mut assistant = AssistantMessage::empty(model);
                    if !message.content.is_empty() {
                        assistant.content.push(AssistantContent::Text {
                            text: message.content.clone(),
                        });
                    }
                    for tc in &message.tool_calls {
                        assistant.content.push(AssistantContent::ToolCall(ToolCall {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            arguments: tc.arguments.clone(),
                        }));
                    }
                    ctx.messages.push(Message::Assistant(assistant));
                }
                Role::Tool => {
                    if let Some(tool_call_id) = &message.tool_call_id {
                        let mut result =
                            ToolResultMessage::new(tool_call_id.clone(), message.content.clone());
                        if message.content.starts_with("Error:") {
                            result = result.with_error(true);
                        }
                        ctx.messages.push(Message::ToolResult(result));
                    }
                }
            }
        }
        ctx
    }
}

#[async_trait]
impl LanguageModel for ProviderLm {
    // Provider-qualified so usage buckets don't collide across providers.
    fn name(&self) -> String {
        let model = self.model.read().unwrap();
        format!("{}/{}", model.provider, model.id)
    }

    async fn complete(
        &self,
        prompt: &[ChatMessage],
        cancel: &CancellationToken,
    ) -> Result<Completion, String> {
        self.complete_streaming(
            prompt,
            std::sync::Arc::new(|_: &str| true),
            None,
            None,
            cancel,
        )
        .await
    }

    fn set_tools(&self, tools: Vec<ToolDefinition>) {
        ProviderLm::set_tools(self, tools);
    }

    fn context_window(&self) -> Option<u64> {
        let window = self.model.read().unwrap().context_window;
        (window > 0).then_some(window)
    }

    async fn complete_streaming(
        &self,
        prompt: &[ChatMessage],
        on_delta: DeltaSink,
        on_thinking: Option<ThinkingSink>,
        on_tool_call: Option<ToolCallSink>,
        cancel: &CancellationToken,
    ) -> Result<Completion, String> {
        // Retry-with-backoff on transient faults (429/5xx/transport), but
        // only while nothing has streamed: once deltas reached the caller,
        // retrying would duplicate visible output, so the error surfaces.
        let mut attempt = 0u32;
        loop {
            let streamed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = streamed.clone();
            let inner = on_delta.clone();
            let wrapped: DeltaSink = std::sync::Arc::new(move |delta: &str| {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
                inner(delta)
            });
            match self
                .attempt_streaming(
                    prompt,
                    wrapped,
                    on_thinking.clone(),
                    on_tool_call.clone(),
                    cancel,
                )
                .await
            {
                Ok(completion) => return Ok(completion),
                Err(e)
                    if attempt < RETRIES
                        && is_transient(&e)
                        && !streamed.load(std::sync::atomic::Ordering::Relaxed) =>
                {
                    // Race backoff against cancel so a stop request
                    // doesn't wait out the retry sleep. +/-20% jitter
                    // spreads clients out during a correlated outage.
                    let backoff = BASE_BACKOFF_MS << attempt;
                    let backoff = (backoff as f32 * rand::random_range(0.8f32..1.2f32)) as u64;
                    attempt += 1;
                    tokio::select! {
                        () = tokio::time::sleep(std::time::Duration::from_millis(backoff)) => {}
                        () = cancel.cancelled() => return Err("aborted".to_owned()),
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl ProviderLm {
    async fn attempt_streaming(
        &self,
        prompt: &[ChatMessage],
        on_delta: DeltaSink,
        on_thinking: Option<ThinkingSink>,
        on_tool_call: Option<ToolCallSink>,
        cancel: &CancellationToken,
    ) -> Result<Completion, String> {
        let model = self.model();
        let auth = tokio::select! {
            auth = self.auth.resolve(&model.provider) => {
                auth.map_err(|e| format!("auth resolution failed for '{}': {e}", model.provider))?
                    .ok_or_else(|| {
                        format!(
                            "no credentials for provider '{}' (use --login, --api-key, or env)",
                            model.provider
                        )
                    })?
            }
            () = cancel.cancelled() => return Err("aborted".to_owned()),
        };

        let mut opts = StreamOptions::new(auth);
        opts.thinking = self.thinking();
        opts.max_tokens = self.max_tokens;
        // Wire the session's cancel token into the provider stream so
        // `run_sse` can tear down the HTTP connection instantly.
        opts.cancel = cancel.clone();

        let mut context = Self::context(&model, prompt);
        // Copy registered tool definitions into the context so the API
        // request includes the native `tools` array.
        context.tools = self.tools.read().unwrap().clone();

        // ── Extension: BeforeProviderRequest + MessageStart ────────────────
        // Fire-and-forget: these are observational notifications (their returns
        // are discarded), so detaching them keeps a slow extension from stalling
        // the start of the stream. The dispatch primitive bounds each handler.
        let ext_rt = self.runtime.read().unwrap().clone();
        if let Some(rt) = &ext_rt {
            let rt = Arc::clone(rt);
            let model_label = format!("{}/{}", model.provider, model.id);
            let messages = prompt.len();
            let tools = context.tools.len();
            tokio::spawn(async move {
                let _ = rt
                    .before_provider_request(serde_json::json!({
                        "model": model_label,
                        "messages": messages,
                        "tools": tools,
                    }))
                    .await;
                let _ = rt
                    .on_message_start(serde_json::json!({ "model": model_label }))
                    .await;
            });
        }

        let mut stream = self.registry.stream(&model, &context, opts);
        let mut terminal = None;
        let mut accumulated = String::new();
        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    // Dropping `stream` closes the channel; the spawned
                    // `run_sse` task sees `sink.is_closed()` and returns.
                    return Err("aborted".to_owned());
                }
                event = stream.next() => {
                    let event = match event {
                        Some(event) => event,
                        None => break,
                    };
                    match event {
                        AssistantEvent::TextDelta { delta, .. } => {
                            accumulated.push_str(&delta);
                            if !on_delta(&delta) {
                                // Early stop: dropping the stream aborts the request.
                                // Usage is unknown for aborted completions.
                                return Ok(Completion {
                                    text: accumulated,
                                    usage: phi_ai::Usage::default(),
                                    ..Default::default()
                                });
                            }
                        }
                        AssistantEvent::ThinkingDelta { delta, .. } => {
                            if let Some(sink) = &on_thinking {
                                sink(&delta);
                            }
                        }
                        AssistantEvent::ToolCallStart { id, name, .. } => {
                            if let Some(sink) = &on_tool_call {
                                sink(ToolCallEvent::Start { id: id.clone(), name: name.clone() });
                            }
                            // ── Extension: MessageUpdate (tool call start) ──────
                            // Fire-and-forget so a slow observer can't block
                            // stream consumption (and keep `cancel` from being
                            // polled at this level). `id`/`name` are still owned
                            // here — the sink call above used clones.
                            if let Some(rt) = &ext_rt {
                                let rt = Arc::clone(rt);
                                tokio::spawn(async move {
                                    let _ = rt
                                        .on_message_update(
                                            serde_json::json!({}),
                                            serde_json::json!({
                                                "type": "tool_call_start",
                                                "id": id,
                                                "name": name,
                                            }),
                                        )
                                        .await;
                                });
                            }
                        }
                        AssistantEvent::ToolCallDelta { delta, .. } => {
                            // We don't have the id here in the generic event;
                            // providers that need it include it in End.
                            // Delta is best-effort for live streaming UI.
                            if let Some(sink) = &on_tool_call {
                                sink(ToolCallEvent::Delta { id: String::new(), delta });
                            }
                        }
                        AssistantEvent::ToolCallEnd { id, name, arguments, .. } => {
                            if let Some(sink) = &on_tool_call {
                                sink(ToolCallEvent::End {
                                    id: id.clone(),
                                    name: name.clone(),
                                    arguments: arguments.clone(),
                                });
                            }
                            // ToolExecutionStart is fired by the engine
                            // (core.rs) right before `env.exec`, with a
                            // meaningful call_id — firing it here too would
                            // double-dispatch the event per tool call.
                        }
                        AssistantEvent::Done { message } | AssistantEvent::Error { message } => {
                            terminal = Some(message);
                        }
                        _ => {}
                    }
                }
            }
        }
        let message = terminal.ok_or_else(|| "stream ended without a terminal event".to_owned())?;
        if message.is_error() {
            return Err(message
                .error_message
                .unwrap_or_else(|| "provider error".to_owned()));
        }
        // ── Extension: AfterProviderResponse + MessageEnd ────────────────
        // Fire-and-forget: observational notifications with discarded returns,
        // detached so they can't delay returning the completion.
        if let Some(rt) = &ext_rt {
            let rt = Arc::clone(rt);
            let msg_json = serde_json::json!({
                "text": message.text(),
                "stop_reason": format!("{:?}", message.stop_reason),
                "tool_calls": message.content.iter().filter_map(|block| {
                    if let AssistantContent::ToolCall(call) = block {
                        Some(serde_json::json!({
                            "id": call.id,
                            "name": call.name,
                            "arguments": call.arguments,
                        }))
                    } else {
                        None
                    }
                }).collect::<Vec<_>>(),
            });
            tokio::spawn(async move {
                let _ = rt.after_provider_response(200, serde_json::json!({})).await;
                let _ = rt.on_message_end(msg_json).await;
            });
        }

        // Extract tool calls from the assistant message content blocks.
        let tool_calls: Vec<ToolCall> = message
            .content
            .iter()
            .filter_map(|block| {
                if let AssistantContent::ToolCall(call) = block {
                    Some(ToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    })
                } else {
                    None
                }
            })
            .collect();
        Ok(Completion {
            text: message.text(),
            usage: message.usage,
            tool_calls,
            stop_reason: message.stop_reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::is_transient;

    #[test]
    fn transient_classification() {
        assert!(is_transient("HTTP 429 rate limit exceeded"));
        assert!(is_transient("server error: 503 Service Unavailable"));
        assert!(is_transient("Overloaded"));
        assert!(is_transient("connection reset by peer"));
        assert!(is_transient("request timed out"));
        assert!(is_transient("stream ended without a terminal event"));
        assert!(!is_transient("HTTP 401 unauthorized"));
        assert!(!is_transient("no credentials for provider 'anthropic'"));
        assert!(!is_transient("HTTP 400 invalid request"));
        // Quota/billing 429s are terminal, not transient.
        assert!(!is_transient("HTTP 429: insufficient_quota"));
        assert!(!is_transient(
            "HTTP 429: monthly usage limit reached, billing required"
        ));
    }
}
