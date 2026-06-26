//! Server-side session: wraps the RLM [`RlmSession`] and bridges
//! [`RlmEvent`]s to wire [`ServerEvent`]s.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use phi_ai::{AuthStorage, Registry};
use phi_cli::Args;
use phi_lm_core::Role;
use phi_protocol::{HistoryMessage, ServerEvent};
use phi_rlm::{RlmCompletion, RlmEvent, RlmSession};
use phi_session_tui::session_builder;
use phi_session_tui::tui::TuiSetup;

/// A handle to a server session.
pub type SessionHandle = Arc<tokio::sync::Mutex<ServerSession>>;

/// A server-side RLM session with event bridging.
///
/// `events_rx` is kept *outside* the session mutex so a concurrent
/// forwarder task can drain events while `send()` runs inside the
/// mutex — this is what makes events stream to clients in real time
/// rather than being flushed in a burst after the turn completes.
pub struct ServerSession {
    pub session: RlmSession,
    pub events_rx: Option<mpsc::UnboundedReceiver<RlmEvent>>,
    /// Cancel token for the current generation (if any).
    pub cancel: CancellationToken,
}

/// Build a server session from a working directory, mirroring
/// `session_builder::build_session` but without the UI setup.
pub async fn build_server_session(cwd: &Path) -> Result<(String, ServerSession)> {
    let storage = Arc::new(AuthStorage::new(default_auth_path()?));
    let registry = Arc::new(Registry::builtin());
    let args = default_args();

    let shared = session_builder::SharedResources {
        args,
        storage,
        registry,
    };

    let setup: TuiSetup = session_builder::build_session(cwd, &shared, None).await?;

    let TuiSetup {
        rlm,
        payload,
        events,
        ..
    } = setup;

    let session_id = phi_session::session::SessionStore::new(cwd)
        .map(|s| s.id().to_string())
        .unwrap_or_else(|_| "default".to_string());

    let session = RlmSession::new(rlm, payload);
    Ok((
        session_id,
        ServerSession {
            session,
            events_rx: Some(events),
            cancel: CancellationToken::new(),
        },
    ))
}

fn default_args() -> Args {
    Args {
        prompt: Vec::new(),
        context: None,
        model: None,
        sub_model: None,
        thinking: None,
        sub_model_thinking: None,
        max_iterations: None,
        max_depth: None,
        budget: None,
        max_tokens: None,
        timeout: None,
        max_errors: None,
        max_concurrent_subcalls: None,
        python: None,
        login: None,
        device_code: false,
        logout: None,
        list_models: None,
        mux: false,
        no_mux: false,
        project: Vec::new(),
        verbose: false,
        quiet: false,
        output: None,
        command: None,
    }
}

impl ServerSession {
    /// Send a message to the RLM.
    ///
    /// The caller is responsible for concurrently reading `events_rx`
    /// (via [`take_events_rx`]) to bridge events to the client while this
    /// call runs — this is what makes events stream in real time.
    pub async fn send(&mut self, prompt: &str) -> RlmCompletion {
        let cancel = self.cancel.clone();
        let completion = self.session.send(prompt, &cancel).await;
        // Reset the cancel token for the next turn.
        self.cancel = CancellationToken::new();
        completion
    }

    /// Take the event receiver out of the session so a concurrent task
    /// can poll it while `send()` runs. Use [`return_events_rx`] to put
    /// it back after the turn.
    pub fn take_events_rx(&mut self) -> Option<mpsc::UnboundedReceiver<RlmEvent>> {
        self.events_rx.take()
    }

    /// Return the event receiver to the session after a turn completes.
    pub fn return_events_rx(&mut self, rx: mpsc::UnboundedReceiver<RlmEvent>) {
        self.events_rx = Some(rx);
    }

    /// Set the cancel token for the upcoming turn (wired from the server's
    /// shared `current_cancel` so cancel works without holding the session
    /// mutex).
    pub fn set_cancel(&mut self, token: CancellationToken) {
        self.cancel = token;
    }

    /// Clear conversation history.
    pub async fn clear(&mut self) {
        self.session.reset().await;
    }

    /// Return conversation history as wire messages.
    pub fn history(&self) -> Vec<HistoryMessage> {
        self.session
            .transcript
            .iter()
            .map(|m| HistoryMessage {
                role: match m.role {
                    Role::System => "system".to_string(),
                    Role::User => "user".to_string(),
                    Role::Assistant => "assistant".to_string(),
                },
                content: m.content.clone(),
            })
            .collect()
    }
}

/// Bridge an [`RlmEvent`] to a [`ServerEvent`].
pub fn bridge_event(event: &RlmEvent) -> Option<ServerEvent> {
    use RlmEvent::*;
    Some(match event {
        RootDelta { delta, .. } => ServerEvent::TextDelta { text: delta.clone() },
        RootThinkingDelta { delta, .. } => ServerEvent::ReasoningDelta { text: delta.clone() },
        RootText { text, .. } => ServerEvent::RootText { text: text.clone() },
        CodeExecStart { code, .. } => ServerEvent::CodeExecStart { code: code.clone() },
        CodeExecEnd { stdout, stderr, duration, .. } => ServerEvent::CodeExecEnd {
            stdout: stdout.clone(),
            stderr: stderr.clone(),
            duration_secs: duration.as_secs_f64(),
        },
        ToolStart { call_id, name, args, .. } => ServerEvent::ToolStart {
            call_id: *call_id,
            name: name.clone(),
            args: args.clone(),
        },
        ToolEnd { call_id, name, result, error, duration, .. } => ServerEvent::ToolEnd {
            call_id: *call_id,
            name: name.clone(),
            result: result.clone(),
            error: error.clone(),
            duration_secs: duration.as_secs_f64(),
        },
        SubcallStart { model, prompt_preview, .. } => ServerEvent::SubcallStart {
            model: model.clone(),
            prompt_preview: prompt_preview.clone(),
        },
        SubcallEnd { model, duration, error, .. } => ServerEvent::SubcallEnd {
            model: model.clone(),
            duration_secs: duration.as_secs_f64(),
            error: error.clone(),
        },
        AnswerSubmitted { content, .. } => ServerEvent::AnswerSubmitted { content: content.clone() },
        LimitHit { limit, detail, .. } => ServerEvent::LimitHit {
            limit: format!("{limit:?}").to_lowercase(),
            detail: detail.clone(),
        },
        RunStart { .. } | RunEnd { .. } | IterationStart { .. } | IterationEnd { .. } => return None,
    })
}

fn default_auth_path() -> Result<std::path::PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "phi")
        .ok_or_else(|| anyhow::anyhow!("could not determine a home directory"))?;
    Ok(dirs.data_dir().join("auth.json"))
}
