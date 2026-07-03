use super::*;

use phi_ext::{PickerKind, UiAction};

pub(super) fn python_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub(super) struct LoginState {
    pub(super) code_tx: mpsc::UnboundedSender<String>,
    pub(super) cancel: CancellationToken,
}

pub(super) struct TuiLoginCallbacks {
    msg_tx: mpsc::UnboundedSender<LoginMsg>,
    code_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<String>>,
}

impl LoginCallbacks for TuiLoginCallbacks {
    fn on_auth_url(&self, url: &str, instructions: &str) {
        let _ = self.msg_tx.send(LoginMsg::Info(instructions.to_owned()));
        let _ = self.msg_tx.send(LoginMsg::Info(format!("  {url}")));
    }

    fn on_device_code(&self, user_code: &str, verification_uri: &str, expires_in: Duration) {
        let _ = self.msg_tx.send(LoginMsg::Info(format!(
            "Visit {verification_uri} and enter code: {user_code} (expires in {} minutes)",
            expires_in.as_secs() / 60
        )));
    }

    fn on_progress(&self, message: &str) {
        let _ = self.msg_tx.send(LoginMsg::Info(message.to_owned()));
    }

    fn manual_code(&self) -> std::pin::Pin<Box<dyn Future<Output = Option<String>> + Send + '_>> {
        Box::pin(async move { self.code_rx.lock().await.recv().await })
    }
}

impl Ui {
    pub(super) async fn handle_input(
        &mut self,
        event: Event,
        cmd_tx: &mpsc::UnboundedSender<Cmd>,
    ) -> bool {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if self.selector.is_some() {
                    self.handle_selector_key(key, cmd_tx);
                    return false;
                }
                if self.login.is_none() && self.handle_popup_key(key) == PopupOutcome::Handled {
                    return false;
                }
                if key.code == KeyCode::Esc {
                    // Idle esc on an empty editor: a second esc within 500ms
                    // opens the session tree (pi's double-escape).
                    if self.running.is_none()
                        && self.login.is_none()
                        && self.editor.text().trim().is_empty()
                    {
                        let now = std::time::Instant::now();
                        if self
                            .last_esc
                            .replace(now)
                            .is_some_and(|t| now.duration_since(t) < Duration::from_millis(500))
                        {
                            self.last_esc = None;
                            self.open_tree_selector();
                        }
                        return false;
                    }
                    if self.running.is_some() && !self.queued.is_empty() {
                        // A message is queued: simply unqueue it back to the
                        // editor without interrupting the in-flight run.
                        self.restore_queued();
                        return false;
                    }
                    self.cancel_run();
                    self.cancel_login();
                    return false;
                }
                if key.code == KeyCode::Up && key.modifiers.contains(KeyModifiers::ALT) {
                    self.restore_queued();
                    return false;
                }
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                if key.code == KeyCode::Char('c') && ctrl {
                    let now = std::time::Instant::now();
                    if self
                        .last_ctrl_c
                        .replace(now)
                        .is_some_and(|t| now.duration_since(t) < Duration::from_millis(500))
                    {
                        return true;
                    }
                    // When ctrl+c is consumed by a more specific action
                    // (cancel a run/login, clear typed text), disarm the
                    // quit window — otherwise the "press again to quit"
                    // hint fires spuriously and a second ctrl+c meant as
                    // another cancel would exit the app. Only an idle
                    // ctrl+c on an empty editor arms quit-confirm.
                    if self.running.is_some() {
                        self.cancel_run();
                        self.last_ctrl_c = None;
                    } else if self.login.is_some() {
                        self.cancel_login();
                        self.last_ctrl_c = None;
                    } else if !self.editor.is_empty() || self.popup.is_some() {
                        self.editor.clear();
                        self.popup = None;
                        self.last_ctrl_c = None;
                    }
                    return false;
                }
                if let KeyCode::Char('p' | 'P') = key.code
                    && ctrl
                {
                    self.cycle_model(key.modifiers.contains(KeyModifiers::SHIFT));
                    return false;
                }
                if key.code == KeyCode::Char('l') && ctrl {
                    if !self.request_host_model_modal(true) {
                        self.open_model_selector(true);
                    }
                    return false;
                }
                if key.code == KeyCode::Char('t') && ctrl {
                    self.toggle_thinking();
                    return false;
                }
                if key.code == KeyCode::Char('g') && ctrl {
                    self.external_edit = true;
                    return false;
                }
                if key.code == KeyCode::Tab && self.login.is_none() {
                    // handle_popup_key consumed Tab if a popup was open
                    // (e.g. from an @-mention). Otherwise Tab does nothing —
                    // file completion only happens via @ mentions.
                    // When the agent is running and the editor is empty,
                    // Tab interrupts the in-flight run.
                    if self.running.is_some() && self.editor.is_empty() {
                        self.cancel_run();
                    }
                    return false;
                }
                if key.code == KeyCode::BackTab {
                    self.cycle_thinking();
                    return false;
                }
                if key.code == KeyCode::Char('o') && ctrl {
                    self.toggle_tools();
                    return false;
                }
                if key.code == KeyCode::Enter
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && let Some(login) = &self.login
                {
                    let code = self.editor.text();
                    if !code.trim().is_empty() {
                        let _ = login.code_tx.send(code.trim().to_owned());
                        self.editor.clear();
                        self.pending.push(TranscriptEntry::RawLines(vec![
                            self.note(&"[code submitted]"),
                        ]));
                    }
                    return false;
                }
                let before = self.editor.text();
                match self.editor.handle_key(key) {
                    EditorAction::Submit(text) => return self.submit(&text, cmd_tx),
                    EditorAction::Quit => return true,
                    EditorAction::Cancel | EditorAction::None => {}
                }
                if self.editor.text() != before {
                    self.refresh_popup();
                }
            }
            Event::Paste(text) => {
                self.editor.insert_paste(&text);
                self.refresh_popup();
            }
            Event::Resize(width, height) => {
                // resize() no-ops on same dimensions (terminals fire several
                // resize events per window adjustment). When the size really
                // changed, clear the viewport (erase-screen + cursor-home) so
                // the physical cursor is homed to row 0 — matching the
                // cursor_row = 0 that resize() just set. Saved scrollback is
                // deliberately NOT erased (no \x1b[3J): the user's history —
                // including everything from before phi started — survives a
                // window resize.
                self.term.resize(width, height);
                let _ = self.term.clear_viewport();
                // transcript_entries store raw data that re-renders at the
                // current width on every draw(), so resize naturally flexes
                // — no replay needed. Just reset committed_rows so all
                // finalized entries are re-emitted as scrollback.
                self.transcript.reset();
            }
            Event::FocusGained => {
                self.clock.set_focused(true);
            }
            Event::FocusLost => {
                self.clock.set_focused(false);
            }
            _ => {}
        }
        false
    }

    pub(super) fn cancel_run(&mut self) {
        if let Some(cancel) = &self.running {
            cancel.cancel();
            self.status = "cancelling...".to_owned();
        }
    }

    pub(super) fn cancel_login(&mut self) {
        if let Some(login) = &self.login {
            login.cancel.cancel();
        }
    }

    /// Dispatch a shell escape or other non-plain input when the agent is
    /// running. These are queued and sent FIFO once the current turn
    /// completes. Plain chat prompts bypass this and steer directly via
    /// `steer_mid_run`. To interrupt the run, the user presses Tab on an
    /// empty editor. When the agent is idle, dispatches immediately via `send`.
    pub(super) fn dispatch_mid_run(
        &mut self,
        text: &str,
        cmd_tx: &mpsc::UnboundedSender<Cmd>,
        send: impl FnOnce(&mut Self, &str, &mpsc::UnboundedSender<Cmd>),
    ) {
        if self.running.is_none() {
            send(self, text, cmd_tx);
            return;
        }
        // Shell escapes and other non-plain inputs queue as before; plain
        // prompts are steered directly via `steer_mid_run` in `submit`.
        self.queued.push(text.to_owned());
    }

    /// Steer a plain user message into the in-flight run. Pushes to the
    /// shared steer queue (drained by `run_loop`'s save-point hook at the
    /// next iteration boundary) and renders the message immediately in the
    /// chat view so the user sees their interjection without waiting for the
    /// turn to finish.
    pub(super) fn steer_mid_run(&mut self, text: &str) {
        if let Ok(mut q) = self.steer_queue.lock() {
            q.push(text.to_owned());
        }
        // Mirror the same visual treatment as a normal user message so the
        // interjection is immediately visible in the transcript.
        self.push_user_message(text);
    }

    pub(super) fn submit(&mut self, text: &str, cmd_tx: &mpsc::UnboundedSender<Cmd>) -> bool {
        let trimmed = text.trim();
        if trimmed.starts_with('!') && trimmed.len() > 1 {
            // Shell escapes dispatch according to the send mode when the
            // agent is running, same as plain prompts.
            self.dispatch_mid_run(trimmed, cmd_tx, |ui, text, tx| {
                ui.send_shell(text, tx);
            });
            return false;
        }
        if trimmed.starts_with('/') {
            // Every slash command resolves through the extension command
            // registry — built-ins (registered by `phi-builtins`) and user
            // extensions share one path.
            let (cmd_name, args) = match trimmed.split_once(' ') {
                Some((name, rest)) => (name, rest),
                None => (trimmed, ""),
            };
            // Strip leading slashes so the command is looked up by the
            // bare name it was registered with (e.g. `//pomodoro` ->
            // `pomodoro`).
            let cmd_name = cmd_name.trim_start_matches('/');
            let result = self.runtime.invoke_command_sync(cmd_name, args);
            match result {
                Ok(Some(output)) => {
                    if let Some(msg) = output.message {
                        self.ensure_pending_gap();
                        self.pending.push(TranscriptEntry::RawLines(vec![msg]));
                    }
                    for action in output.actions {
                        if self.apply_action(action, cmd_tx) {
                            return true;
                        }
                    }
                }
                Ok(None) => {
                    self.pending.push(TranscriptEntry::RawLines(vec![
                        self.warn(&format!("unknown command: {cmd_name}")),
                    ]));
                }
                Err(e) => {
                    self.pending
                        .push(TranscriptEntry::RawLines(vec![self.err(&e)]));
                }
            }
            return false;
        }
        // When a run is in flight, plain prompts steer into the in-flight
        // turn via the save-point queue rather than waiting until the turn
        // completes. Shell escapes (handled above) and other special inputs
        // keep their existing queued behaviour.
        if self.running.is_some() {
            self.steer_mid_run(trimmed);
        } else {
            self.send_prompt(trimmed, cmd_tx);
        }
        false
    }

    /// Interpret a declarative `UiAction` returned by a command handler.
    /// Returns true when the action quits the TUI. This is the host side of
    /// the command API contract: every capability a command can ask for is
    /// implemented once here, never as a per-command string match.
    pub(super) fn apply_action(
        &mut self,
        action: UiAction,
        cmd_tx: &mpsc::UnboundedSender<Cmd>,
    ) -> bool {
        match action {
            UiAction::Quit => return true,
            UiAction::NewSession => self.reset_session(cmd_tx),
            UiAction::OpenPicker(kind) => match kind {
                PickerKind::Resume => self.open_resume_selector(),
                PickerKind::Fork => self.open_fork_selector(),
                PickerKind::Tree => self.open_tree_selector(),
            },
            UiAction::OpenOverlay { name, payload } => self.open_overlay(&name, payload),
            UiAction::SwitchModel { root, query } => self.switch_model(&query, root),
            UiAction::StartLogin { provider } => self.start_login(&provider),
            UiAction::Logout { provider } => self.logout(&provider),
            UiAction::TrustProject { trusted } => self.set_project_trust(trusted),
            // Mux-host-owned effects: session commands never emit these.
            UiAction::OpenProject(_) | UiAction::RunHostAction { .. } => {
                self.push_bar(0, "command action not supported in session".to_owned());
            }
        }
        false
    }

    /// Route a by-name overlay open. Embedded (mux) sessions forward the
    /// request to the host, which owns the extension overlay registry;
    /// standalone sessions fall back to their native selectors for the
    /// well-known names and report unknown overlays.
    fn open_overlay(&mut self, name: &str, payload: Option<serde_json::Value>) {
        match name {
            phi_ext::OVERLAY_HELP => self.show_help(),
            phi_ext::OVERLAY_USAGE => {
                if !self.request_host_usage_modal() {
                    self.open_usage_selector();
                }
            }
            phi_ext::OVERLAY_MODEL => {
                let root = payload
                    .as_ref()
                    .and_then(|p| p.get("root"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                if !self.request_host_model_modal(root) {
                    self.open_model_selector(root);
                }
            }
            other => {
                if !self.request_host_overlay(other, payload) {
                    self.pending.push(TranscriptEntry::RawLines(vec![
                        self.warn(&format!("overlay {other} not available")),
                    ]));
                }
            }
        }
    }

    fn set_project_trust(&mut self, trusted: bool) {
        let mut store = phi_session::trust::TrustStore::load_default();
        match store.set(&self.cwd, trusted) {
            Ok(()) if trusted => self.pending.push(TranscriptEntry::RawLines(vec![self.note(
                &"[project trusted — .phi/ extensions load on the next session (/new or restart)]",
            )])),
            Ok(()) => self.pending.push(TranscriptEntry::RawLines(vec![self.note(
                &"[project trust revoked — .phi/ extensions stay unloaded from the next session]",
            )])),
            Err(e) => self
                .pending
                .push(TranscriptEntry::RawLines(vec![self.err(&e)])),
        }
    }

    fn show_help(&mut self) {
        for cmd in self.runtime.list_commands() {
            let usage = format!("/{} {}", cmd.name, cmd.args_hint);
            self.pending
                .push(TranscriptEntry::RawLines(vec![self.note(&format!(
                    "{:<20}{}",
                    usage.trim_end(),
                    cmd.description
                ))]));
        }
        self.pending.push(TranscriptEntry::RawLines(vec![self.note(
            &"!cmd                run bash(cmd) in the REPL; the model sees it next turn",
        )]));
        self.pending.push(TranscriptEntry::RawLines(vec![
            self.note(&"!!cmd               run bash(cmd) without showing the model"),
        ]));
        self.pending.push(TranscriptEntry::RawLines(vec![
            self.note(&"keys: enter send (mode-dependent mid-run) · shift+enter newline · tab complete/cycle send mode · up/down history"),
        ]));
        self.pending.push(TranscriptEntry::RawLines(vec![
            self.note(&"      ctrl-z undo · ctrl-y redo · ctrl-r yank · ctrl-p cycle model · ctrl-l models · shift+tab thinking level"),
        ]));
        self.pending.push(TranscriptEntry::RawLines(vec![
            self.note(&"      ctrl-t show/hide thinking · ctrl-o expand tool output · ctrl-g $EDITOR · ctrl-q suspend · ctrl-x cycle send mode"),
        ]));
        self.pending.push(TranscriptEntry::RawLines(vec![
            self.note(&"      esc cancel · ctrl-c ctrl-c quit"),
        ]));
        self.pending.push(TranscriptEntry::RawLines(vec![
            self.note(&"      esc esc session tree (empty editor)"),
        ]));
    }

    fn reset_session(&mut self, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        let _ = cmd_tx.send(Cmd::Reset);
        self.clear_transcript();
        let _ = self.term.clear_all();
        match SessionStore::new(&self.cwd) {
            Ok(store) => self.store = store,
            Err(e) => self
                .pending
                .push(TranscriptEntry::RawLines(vec![self.err(&e)])),
        }
        self.pending.push(TranscriptEntry::RawLines(vec![
            self.note(&"[session reset]"),
        ]));
    }

    fn logout(&mut self, provider: &str) {
        let provider = if provider.is_empty() {
            self.root_lm.model().provider
        } else {
            provider.to_owned()
        };
        match self.storage.logout(&provider) {
            Ok(()) => self.pending.push(TranscriptEntry::RawLines(vec![
                self.note(&format!("[logged out of {provider}]")),
            ])),
            Err(e) => self
                .pending
                .push(TranscriptEntry::RawLines(vec![self.err(&e)])),
        }
    }

    pub(super) fn send_shell(&mut self, escape: &str, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        let (record, cmd) = match escape.strip_prefix("!!") {
            Some(rest) => (false, rest.trim()),
            None => (true, escape[1..].trim()),
        };
        if cmd.is_empty() {
            return;
        }

        // ── Extension: UserBash ──────────────────────────────────────
        // Let extensions cancel or observe user-initiated bash commands.
        // Fire-and-forget: if an extension cancels, the command still runs
        // (cancel is a side-effect signal, not a blocking gate here).
        let rt = self.runtime.clone();
        let cmd_str = cmd.to_owned();
        let exclude = !record;
        let cwd_str = self.cwd.display().to_string();
        tokio::spawn(async move {
            let _ = rt.on_user_bash(&cmd_str, exclude, &cwd_str).await;
        });

        let code = format!("print(bash({}))", python_quote(cmd));
        if record {
            self.begin_block();
            self.push_repl_block(0, &code, self.code_limit());
        } else {
            self.ensure_pending_gap();
            self.pending.push(TranscriptEntry::RawLines(vec![format!(
                "{tool}${RESET} {tert}{cmd}{RESET}",
                tool = &self.palette.tool_title_fg,
                tert = &self.palette.text_tertiary
            )]));
            self.last_activity = false;
        }
        let _ = cmd_tx.send(Cmd::Shell { code, record });
    }

    pub(super) fn send_prompt(&mut self, prompt: &str, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        self.push_user_message(prompt);
        if let Err(e) = self.store.append(session::Role::User, prompt) {
            self.pending.push(TranscriptEntry::RawLines(vec![format!(
                "{}session not saved:{RESET} {e}",
                &self.palette.warning_fg
            )]));
        }
        // Durable turn boundary: an exchange_start without its exchange_end
        // is how a crash mid-turn stays detectable after restart.
        let _ = self.store.begin_exchange();
        let cancel = CancellationToken::new();
        self.running = Some(cancel.clone());
        // Add a keep-alive interest for the active run: the equalizer,
        // thinking expressions, spinner frames, and tool header shimmer
        // all derive their phase from clock.now_ms() while a run is in
        // flight. When the run ends this interest is removed, but the
        // baseline keep-alive keeps the clock alive for chrome shimmer.
        self.clock.add_keep_alive();
        self.turn = 0;
        self.stream.clear();
        self.stream_backlog.clear();
        self.thinking.clear();
        self.thinking_backlog.clear();
        self.thinking_log.clear();
        self.status = "thinking...".to_owned();
        let _ = cmd_tx.send(Cmd::Prompt {
            text: prompt.to_owned(),
            cancel,
        });
    }

    pub(super) fn warn_if_missing_credentials(&self, provider: String) {
        let storage = self.storage.clone();
        let tx = self.login_tx.clone();
        tokio::spawn(async move {
            if !matches!(storage.resolve(&provider).await, Ok(Some(_))) {
                let _ = tx.send(LoginMsg::Info(format!(
                    "no credentials for '{provider}' — type /login {provider}"
                )));
            }
        });
    }

    pub(super) fn start_login(&mut self, provider: &str) {
        if self.login.is_some() {
            self.pending.push(TranscriptEntry::RawLines(vec![
                self.note(&"[a login is already in progress]"),
            ]));
            return;
        }
        let name = if provider.is_empty() {
            self.root_lm.model().provider
        } else {
            provider.to_owned()
        };
        let Some(provider) = OAuthProviderId::parse(&name) else {
            self.pending
                .push(TranscriptEntry::RawLines(vec![self.err(&format!(
                    "unknown provider '{name}' (expected: anthropic, openai)"
                ))]));
            return;
        };
        let (code_tx, code_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        self.login = Some(LoginState {
            code_tx,
            cancel: cancel.clone(),
        });
        let storage = self.storage.clone();
        let msg_tx = self.login_tx.clone();
        tokio::spawn(async move {
            let callbacks = TuiLoginCallbacks {
                msg_tx: msg_tx.clone(),
                code_rx: tokio::sync::Mutex::new(code_rx),
            };
            let result = storage
                .login(provider, LoginMethod::Browser, &callbacks, &cancel)
                .await;
            let _ = msg_tx.send(LoginMsg::Done {
                provider: provider.storage_key(),
                result: result.map_err(|e| e.to_string()),
            });
        });
    }

    pub(super) fn handle_login_msg(&mut self, msg: LoginMsg) {
        match msg {
            LoginMsg::Info(text) => {
                for line in text.lines() {
                    self.pending
                        .push(TranscriptEntry::RawLines(vec![self.note(&line)]));
                }
            }
            LoginMsg::Done { provider, result } => {
                self.login = None;
                match result {
                    Ok(()) => self.pending.push(TranscriptEntry::RawLines(vec![format!(
                        "{BOLD}logged in to {provider}{RESET}"
                    )])),
                    Err(e) if e.to_lowercase().contains("cancel") => {
                        self.pending.push(TranscriptEntry::RawLines(vec![
                            self.note(&"[login cancelled]"),
                        ]))
                    }
                    Err(e) => self.pending.push(TranscriptEntry::RawLines(vec![format!(
                        "{}login failed:{RESET} {e}",
                        &self.palette.error_fg
                    )])),
                }
            }
        }
    }

    pub(super) fn handle_rlm_event(&mut self, event: RlmEvent) {
        match event {
            RlmEvent::IterationStart {
                depth: 0,
                iteration,
            } => self.handle_iteration_start(iteration),
            RlmEvent::RootDelta {
                depth: 0, delta, ..
            } => self.handle_root_delta(delta),
            RlmEvent::RootThinkingDelta {
                depth: 0, delta, ..
            } => self.handle_root_thinking_delta(delta),
            RlmEvent::RootText { depth: 0, text, .. } => self.handle_root_text(text),
            RlmEvent::Message { depth: 0, text } => self.handle_message(text),
            RlmEvent::CodeExecStart { depth, code, .. } => self.handle_code_exec_start(depth, code),
            RlmEvent::CodeExecEnd {
                depth,
                stdout,
                stderr,
                duration,
                ..
            } => self.handle_code_exec_end(depth, stdout, stderr, duration),
            RlmEvent::ToolStart {
                depth,
                call_id,
                name,
                args,
            } => self.handle_tool_start(depth, call_id, name, args),
            RlmEvent::ToolEnd {
                depth,
                call_id,
                name,
                result,
                error,
                duration,
            } => self.handle_tool_end(depth, call_id, name, result, error, duration),
            RlmEvent::SubcallStart {
                depth,
                model,
                prompt_preview,
            } => self.handle_subcall_start(depth, model, prompt_preview),
            RlmEvent::SubcallEnd {
                depth,
                model,
                result_preview,
                result,
                duration,
                error,
                usage,
            } => self.handle_subcall_end(
                depth,
                model,
                result_preview,
                result,
                duration,
                error,
                usage,
            ),
            RlmEvent::RunEnd {
                depth,
                stop,
                iterations,
                duration,
            } if depth > 0 => self.handle_run_end(depth, stop, iterations, duration),
            RlmEvent::Compaction {
                depth: 0,
                iteration,
                tokens_before,
                summary,
                error,
            } => self.handle_compaction(iteration, tokens_before, summary, error),
            _ => {}
        }
    }

    /// In-loop compaction fired (or failed). On success, persist the summary
    /// as an additive marker in the session JSONL — the message/repl log is
    /// never rewritten; reconstruction splices the summary back into the
    /// turn's archived history.
    fn handle_compaction(
        &mut self,
        iteration: usize,
        tokens_before: u64,
        summary: Option<String>,
        error: Option<String>,
    ) {
        match (summary, error) {
            (Some(summary), _) => {
                let _ = self
                    .store
                    .append_compaction(iteration, tokens_before, &summary);
                self.status = "context compacted".to_owned();
            }
            (None, Some(_)) => {
                self.status = "compaction failed".to_owned();
            }
            (None, None) => {}
        }
    }

    fn handle_iteration_start(&mut self, iteration: usize) {
        self.turn = iteration + 1;
        self.status = "thinking...".to_owned();
        self.stream.clear();
        self.stream_backlog.clear();
        self.thinking.clear();
        self.thinking_backlog.clear();
        if iteration == 0 {
            self.thinking_log.clear();
        } else if !self.thinking_log.trim().is_empty() {
            self.thinking_log.push_str("\n\n");
        }
        self.streamed = false;
    }

    fn handle_root_delta(&mut self, delta: String) {
        // Queued, not appended: `drain_stream_backlogs` moves a bounded
        // budget into `stream` per frame so bursts render as flowing text.
        self.stream_backlog.push_str(&delta);
        self.streamed = true;
        // Streamed prose is committed to the in-memory transcript by
        // `push_prose` (called from `handle_root_text` or
        // `handle_code_exec_start`). Intermediate prose is also
        // accumulated into `pending_prose` so it can be persisted as
        // `ReplRecord.prose` at the next code execution checkpoint,
        // surviving session resume.
    }

    fn handle_root_thinking_delta(&mut self, delta: String) {
        self.thinking_backlog.push_str(&delta);
        // The turn-wide log is the persistence source: it must carry the
        // full delta immediately, never paced.
        self.thinking_log.push_str(&delta);
    }

    /// Move one frame's budget from the pacing backlogs into the visible
    /// buffers. The budget is proportional to the backlog so a large burst
    /// catches up in a few hundred ms while a steady token flow passes
    /// through almost unbuffered; short tails drain whole to avoid a
    /// dribble at the end. Cuts fall on `char` boundaries.
    pub(super) fn drain_stream_backlogs(&mut self) {
        fn drain_budget(backlog: &mut String, into: &mut String) {
            if backlog.is_empty() {
                return;
            }
            let total = backlog.chars().count();
            let budget = if total <= 24 {
                total
            } else {
                (total / 8).clamp(24, 512)
            };
            let cut = backlog
                .char_indices()
                .nth(budget)
                .map(|(i, _)| i)
                .unwrap_or(backlog.len());
            into.push_str(&backlog[..cut]);
            backlog.drain(..cut);
        }
        drain_budget(&mut self.stream_backlog, &mut self.stream);
        drain_budget(&mut self.thinking_backlog, &mut self.thinking);
    }

    /// Flush the prose backlog whole. Every commit point that takes or
    /// clears `stream` must call this first — pacing is presentation only
    /// and must never make a commit see partial text.
    pub(super) fn flush_stream_backlog(&mut self) {
        if !self.stream_backlog.is_empty() {
            let backlog = std::mem::take(&mut self.stream_backlog);
            self.stream.push_str(&backlog);
        }
    }

    fn handle_root_text(&mut self, text: String) {
        self.flush_thinking();
        self.flush_stream_backlog();
        // Commit the iteration's prose as a single assistant card. With
        // native tool calls, the model's text stream is pure prose — code
        // arrives via `CodeExecStart`/`CodeExecEnd` events, never as
        // ```repl fences in the text. For a streamed message only the
        // uncommitted tail in `stream` is flushed; when the model did not
        // stream, `stream` is empty and `RootText` carries the only copy.
        let source = if self.streamed {
            std::mem::take(&mut self.stream)
        } else {
            text
        };
        self.push_prose(&source);
        self.stream.clear();
        self.streamed = false;
    }

    /// Chat message sent from REPL code via `message(...)`: render it as an
    /// assistant prose card immediately, mid-run, without ending the turn.
    fn handle_message(&mut self, text: String) {
        if text.trim().is_empty() {
            return;
        }
        self.push_prose(&text);
    }

    fn handle_code_exec_start(&mut self, depth: usize, code: String) {
        if depth == 0 {
            // Flush any streamed prose above the code block. With tool
            // calls, code never appears in the text stream, so the entire
            // stream buffer is prose.
            self.flush_stream_backlog();
            let prose = std::mem::take(&mut self.stream);
            if !prose.is_empty() {
                self.push_prose(&prose);
            }
            self.status = "running code...".to_owned();
            self.live_tools.clear();
            self.live_subcalls.clear();
            self.open_tool_args.clear();
            self.open_tool_rows.clear();
            self.open_subcall_rows.clear();
            self.begin_block();
            self.live_code = Some(code.clone());
            // Drain accumulated prose into live_prose so it is persisted
            // as ReplRecord.prose at CodeExecEnd. This is the model's
            // reasoning between steps — it must survive session resume.
            self.live_prose = std::mem::take(&mut self.pending_prose);
        }
        // When tool_summary is enabled, suppress live repl block rendering;
        // a consolidated summary card is rendered after all repls complete.
        if !self.tools.tool_summary {
            self.push_repl_block(depth, &code, self.code_limit());
        }
    }

    fn handle_code_exec_end(
        &mut self,
        depth: usize,
        stdout: String,
        stderr: String,
        duration: Duration,
    ) {
        if depth == 0 {
            // Checkpoint the completed step as it happens (not at
            // exchange end): a crash later in the turn must not lose
            // work already paid for. `branch_restore` replays these
            // to rebuild the REPL env.
            self.status = "thinking...".to_owned();
            self.open_tool_args.clear();
            self.open_tool_rows.clear();
            self.open_subcall_rows.clear();
            if let Some(code) = self.live_code.take() {
                let duration_secs = duration.as_secs_f64();
                let tools = std::mem::take(&mut self.live_tools);
                let subcalls = std::mem::take(&mut self.live_subcalls);
                let prose = std::mem::take(&mut self.live_prose);
                let _ = self.store.append_repl(session::ReplRecord {
                    prose,
                    code: code.clone(),
                    stdout: stdout.clone(),
                    stderr: stderr.clone(),
                    duration_secs,
                    kind: session::ReplKind::Agent,
                    tools: tools.clone(),
                    subcalls: subcalls.clone(),
                });
                // Discard the incrementally-built live block and rebuild
                // from the stored record so live and replay use the same
                // rendering path. In the live session the prose was already
                // rendered by `push_prose` during RootText/CodeExecStart,
                // so pass an empty string here to avoid duplication. On
                // resume, `replay_branch` in navigation.rs passes the
                // persisted prose field so it renders the interleaving.
                self.block.clear();
                self.block_active = false;
                self.after_block.clear();
                self.push_repl_replay(
                    "",
                    &code,
                    &stdout,
                    &stderr,
                    duration_secs,
                    &tools,
                    &subcalls,
                );
            } else {
                self.block_errored = !stderr.is_empty();
                self.live_tools.clear();
                self.live_subcalls.clear();
                self.commit_block();
            }
        } else {
            self.last_activity = true;
            let filtered = strip_tool_echoes(&stdout, &self.live_tools, depth);
            self.push_preview(depth, &filtered, self.output_limit());
            self.push_preview(depth, &stderr, self.output_limit());
        }
    }

    fn handle_tool_start(
        &mut self,
        depth: usize,
        call_id: u64,
        name: String,
        args: serde_json::Value,
    ) {
        let key = (depth, call_id);
        if self.live_code.is_some() {
            self.open_tool_args.insert(key, args.clone());
        }
        let label = tool_title(&name, &args, &self.palette);
        let elapsed = self.clock.now_ms();
        if let Some((start, len)) = self.push_status(
            depth + 1,
            running_status_text_shimmer(
                &label,
                &self.visual.spinner_frame(elapsed),
                &self.palette.working_fg.clone(),
                self.palette.working_shimmer,
                elapsed,
            ),
        ) {
            self.open_tool_rows.insert(
                key,
                OpenToolRow {
                    depth: depth + 1,
                    label,
                    start,
                    len,
                },
            );
        }
        if depth == 0 {
            self.status = format!("{name}...");
        }
    }

    fn handle_tool_end(
        &mut self,
        depth: usize,
        call_id: u64,
        name: String,
        result: String,
        error: Option<String>,
        duration: Duration,
    ) {
        let key = (depth, call_id);
        let args = self.open_tool_args.remove(&key).unwrap_or_default();
        if self.live_code.is_some() {
            // Pi-style spillover: when a tool result exceeds the log
            // budget, spill the full output to a tmp file and store only
            // the truncated tail + path in the durable session entry.
            let (result_tail, result_path) =
                match SessionStore::spill_output(&result, TOOL_RESULT_LOG_BUDGET) {
                    Ok(spill) => (spill.tail, spill.full_output_path),
                    Err(_) => (truncate_for_log(&result), None),
                };
            self.live_tools.push(session::ToolCall {
                name: name.clone(),
                args: args.clone(),
                result: result_tail,
                full_output_path: result_path,
                error: error.as_deref().map(truncate_for_log),
                duration_secs: duration.as_secs_f64(),
                depth,
            });
        }
        let output = error.as_deref().unwrap_or(&result);
        if error.is_some() {
            self.block_errored = true;
        }
        let lang = if error.is_some() {
            None
        } else {
            tool_preview_lang(&name, &args)
        };
        let rows = if self.tools.tool_summary {
            // When tool_summary is enabled, suppress individual tool cards
            // during live execution — just clear the spinner. Tools are
            // accumulated in live_tools and rendered as a consolidated
            // summary after the repl block completes.
            Vec::new()
        } else {
            self.tool_result_rows(
                depth + 1,
                &name,
                &args,
                output,
                self.output_limit(),
                lang,
                error.is_some(),
            )
        };
        if let Some(row) = self.open_tool_rows.remove(&key) {
            let new_len = self.replace_block_rows(row.start, row.len, rows.clone());
            // Completion pulse: the finished card's anchor sweeps green for
            // a beat — the magenta→green hue cross marks the state change.
            if error.is_none() && !rows.is_empty() && self.block_active {
                self.flash_rows.push(FlashRow {
                    start: row.start,
                    len: new_len,
                    expires_ms: self.clock.now_ms() + COMPLETION_FLASH_MS,
                    rows,
                });
            }
            self.last_activity = true;
        } else {
            for row in rows {
                self.push_row(row);
            }
            self.last_activity = true;
        }
        if depth == 0 {
            self.status = "running code...".to_owned();
        }
    }

    fn handle_subcall_start(&mut self, depth: usize, model: String, prompt_preview: String) {
        let label = subcall_title(&model, &prompt_preview, &self.palette);
        if self.live_code.is_some() && depth >= 1 {
            self.open_subcall_rows.push_back(OpenSubcallRow {
                depth,
                prompt_preview: prompt_preview.clone(),
                label,
                start: 0,
                len: 0,
            });
        } else {
            let elapsed = self.clock.now_ms();
            if let Some((start, len)) = self.push_status(
                depth,
                running_status_text_shimmer(
                    &label,
                    &self.visual.spinner_frame(elapsed),
                    &self.palette.working_fg.clone(),
                    self.palette.working_shimmer,
                    elapsed,
                ),
            ) {
                self.open_subcall_rows.push_back(OpenSubcallRow {
                    depth,
                    prompt_preview: prompt_preview.clone(),
                    label,
                    start,
                    len,
                });
            }
        }
        if depth == 0 {
            self.status = format!("waiting on {model}...");
        }
    }

    fn handle_subcall_end(
        &mut self,
        depth: usize,
        model: String,
        result_preview: Option<String>,
        result: Option<String>,
        duration: Duration,
        error: Option<String>,
        usage: Option<phi_ai::Usage>,
    ) {
        let open = self
            .open_subcall_rows
            .iter()
            .position(|row| row.depth == depth)
            .and_then(|idx| self.open_subcall_rows.remove(idx));
        if self.live_code.is_some() {
            let prompt_preview = open
                .as_ref()
                .map(|row| row.prompt_preview.clone())
                .unwrap_or_default();
            self.live_subcalls.push(session::SubcallRecord {
                model: model.clone(),
                prompt_preview,
                result_preview: result_preview.clone(),
                result: result.clone(),
                error: error.clone(),
                duration_secs: duration.as_secs_f64(),
                depth,
                usage: usage.map(|usage| session::SessionUsage {
                    input: usage.input,
                    output: usage.output,
                    cache_read: usage.cache_read,
                    cache_write: usage.cache_write,
                    cost_total: usage.cost.total,
                    model: model.clone(),
                }),
            });
        }
        // Reuse the label formatted once at `SubcallStart` (stored on
        // the open row) instead of recomputing it for the border.
        let label = open
            .as_ref()
            .map(|row| row.label.clone())
            .unwrap_or_else(|| subcall_title(&model, "", &self.palette));
        let content = error.as_deref().or(result_preview.as_deref());

        // If a subagent box is active at this depth, finalize it:
        // collapse the fixed box to a compact header + result card.
        let rows =
            self.subcall_result_rows(depth, &label, content, self.output_limit(), error.is_some());
        if let Some(row) = open {
            self.replace_block_rows(row.start, row.len, rows);
            self.last_activity = true;
        } else {
            if self.block_active {
                self.ensure_block_gap();
            } else if depth == 0 {
                self.ensure_pending_gap();
            }
            for row in rows {
                self.push_row(row);
            }
            self.last_activity = true;
        }
    }

    fn handle_run_end(
        &mut self,
        depth: usize,
        stop: StopCause,
        iterations: usize,
        duration: Duration,
    ) {
        // A recursive subcall's header is the `SubcallStart` card; skip
        // the redundant `RunStart` "rlm" card (it would double-mark the
        // same nesting level). The run's outcome is summarized below.
        let bar_text = format!(
            "{} · {} turn{} · {:.1}s",
            format!("{stop:?}").to_lowercase(),
            iterations,
            if iterations == 1 { "" } else { "s" },
            duration.as_secs_f64()
        );
        self.push_bar(depth, bar_text);
    }

    pub(super) fn handle_done(&mut self, done: Done, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        match done {
            Done::Run(completion) => self.handle_run_done(completion),
            Done::Shell {
                code,
                record,
                result,
            } => self.handle_shell_done(code, record, result),
            Done::Restored(result) => {
                if let Err(e) = result {
                    self.ensure_pending_gap();
                    self.pending.push(TranscriptEntry::RawLines(vec![format!(
                        "{}restore failed:{RESET} {e}",
                        &self.palette.error_fg
                    )]));
                }
            }
        }
        self.drain_queue(cmd_tx);
    }

    pub(super) fn drain_queue(&mut self, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        if self.running.is_some() || self.queued.is_empty() {
            return;
        }
        let next = self.queued.remove(0);
        if next.starts_with('!') {
            self.send_shell(&next, cmd_tx);
        } else {
            self.send_prompt(&next, cmd_tx);
        }
    }

    pub(super) fn handle_shell_done(
        &mut self,
        code: String,
        record: bool,
        result: Result<ReplResult, String>,
    ) {
        match result {
            Ok(res) if record => {
                self.last_activity = true;
                self.push_preview(0, &res.stdout, self.output_limit());
                self.push_preview(0, &res.stderr, self.output_limit());
                self.commit_block();
                let _ = self.store.append_repl(session::ReplRecord {
                    prose: String::new(),
                    code,
                    stdout: res.stdout,
                    stderr: res.stderr,
                    duration_secs: res.duration.as_secs_f64(),
                    kind: session::ReplKind::Shell,
                    tools: Vec::new(),
                    subcalls: Vec::new(),
                });
            }
            Ok(res) => {
                let output = format!("{}{}", res.stdout, res.stderr);
                self.ensure_pending_gap();
                if output.trim().is_empty() {
                    self.pending
                        .push(TranscriptEntry::RawLines(vec![self.note(&"(no output)")]));
                } else {
                    self.pending.push(TranscriptEntry::RawLines(
                        output.trim_end().lines().map(str::to_owned).collect(),
                    ));
                }
                self.last_activity = false;
            }
            Err(e) => {
                if self.block_active {
                    self.push_bar(0, format!("failed: {e}"));
                    self.commit_block();
                } else {
                    self.ensure_pending_gap();
                    self.pending
                        .push(TranscriptEntry::RawLines(vec![self.err(&e)]));
                    self.last_activity = false;
                }
            }
        }
    }

    pub(super) fn handle_run_done(&mut self, completion: RlmCompletion) {
        // A block still open here never finished (cancel/failure mid-exec).
        self.fail_open_activity_rows();
        self.commit_block();
        self.live_code = None;
        self.pending_prose.clear();
        self.live_prose.clear();
        self.running = None;
        // Remove the run's keep-alive interest: the equalizer/spinner no
        // longer need dedicated ticks. The baseline keep-alive keeps the
        // clock alive so chrome shimmer/gradient text continue animating.
        self.clock.remove_keep_alive();
        // The completion carries the full response text; whatever is still
        // queued for pacing is a stale copy of the same bytes.
        self.stream.clear();
        self.stream_backlog.clear();
        self.thinking.clear();
        self.thinking_backlog.clear();
        let display_text = assistant_display_text(&completion.response, &self.thinking_log);
        self.thinking_log.clear();
        // Mirror the env's history condition so the store and the REPL's
        // history_N variables stay aligned. Store before rendering so the
        // log is durable before the display reflects it.
        if completion.stop != StopCause::Failed && !completion.response.trim().is_empty() {
            let usage = session::SessionUsage {
                input: completion.usage.input,
                output: completion.usage.output,
                cache_read: completion.usage.cache_read,
                cache_write: completion.usage.cache_write,
                cost_total: completion.usage.cost.total,
                model: {
                    let m = self.root_lm.model();
                    format!("{}/{}", m.provider, m.id)
                },
            };
            let usage_by_model = completion
                .usage_by_model
                .iter()
                .map(|breakdown| session::SessionUsage {
                    input: breakdown.usage.input,
                    output: breakdown.usage.output,
                    cache_read: breakdown.usage.cache_read,
                    cache_write: breakdown.usage.cache_write,
                    cost_total: breakdown.usage.cost.total,
                    model: breakdown.model.clone(),
                })
                .collect();
            let _ = self.store.append_entry(
                session::Role::Assistant,
                &completion.response,
                display_text,
                Some(usage),
                usage_by_model,
            );
        }
        self.ensure_pending_gap();
        if completion.stop == StopCause::Failed {
            let error = completion.error.unwrap_or_else(|| "run failed".to_owned());
            self.pending
                .push(TranscriptEntry::RawLines(vec![self.err(&error)]));
        } else if !completion.response.is_empty() {
            self.push_assistant_message(&completion.response);
        }
        // Durably close the turn: completed, failed, or interrupted is a
        // recorded fact, not an inference from a missing assistant message.
        let status = match completion.stop {
            StopCause::Failed => session::ExchangeStatus::Failed,
            StopCause::Cancelled => session::ExchangeStatus::Interrupted,
            _ => session::ExchangeStatus::Completed,
        };
        let _ = self.store.end_exchange(status);
        // `completion.usage` is the per-turn delta (`complete_run_since`);
        // accumulate it so the chrome shows session totals rather than
        // silently replacing them with the last turn's numbers.
        self.usage.add(&completion.usage);
        self.pending
            .push(TranscriptEntry::RawLines(vec![String::new()]));
        self.last_activity = false;
    }
}
