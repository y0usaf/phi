use super::*;

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
    pub(super) async fn handle_input(&mut self, event: Event, cmd_tx: &mpsc::UnboundedSender<Cmd>) -> bool {
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
                    if self.running.is_some() {
                        self.restore_queued();
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
                    if self.running.is_some() {
                        self.cancel_run();
                    } else if self.login.is_some() {
                        self.cancel_login();
                    } else {
                        self.editor.clear();
                        self.popup = None;
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
                    // handle_popup_key consumed Tab if a popup was open;
                    // otherwise Tab force-triggers path completion (pi-style).
                    self.open_path_popup();
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
                        self.pending.push(format!("{DIM}[code submitted]{RESET}"));
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
            Event::Resize(width, height) => self.term.resize(width, height),
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

    pub(super) fn submit(&mut self, text: &str, cmd_tx: &mpsc::UnboundedSender<Cmd>) -> bool {
        let trimmed = text.trim();
        match trimmed {
            "/quit" | "/exit" => return true,
            "/help" => {
                for spec in COMMANDS {
                    let usage = format!("/{} {}", spec.name, spec.args);
                    self.pending
                        .push(format!("{DIM}{:<20}{}{RESET}", usage.trim_end(), spec.desc));
                }
                self.pending.push(format!(
                    "{DIM}!cmd                run bash(cmd) in the REPL; the model sees it next turn{RESET}"
                ));
                self.pending.push(format!(
                    "{DIM}!!cmd               run bash(cmd) without showing the model{RESET}"
                ));
                self.pending.push(format!(
                    "{DIM}keys: enter send (queues mid-run) · shift+enter newline · tab complete · up/down history{RESET}"
                ));
                self.pending.push(format!(
                    "{DIM}      ctrl-y yank · ctrl-_ undo · ctrl-p cycle model · ctrl-l models · shift+tab thinking level{RESET}"
                ));
                self.pending.push(format!(
                    "{DIM}      ctrl-t show/hide thinking · ctrl-o expand tool output · ctrl-g $EDITOR · ctrl-z suspend{RESET}"
                ));
                self.pending
                    .push(format!("{DIM}      esc cancel · ctrl-c ctrl-c quit{RESET}"));
                self.pending.push(format!(
                    "{DIM}      esc esc session tree (empty editor){RESET}"
                ));
            }
            "/new" => {
                let _ = cmd_tx.send(Cmd::Reset);
                self.clear_transcript();
                let _ = self.term.clear_all();
                match SessionStore::new(&self.cwd) {
                    Ok(store) => self.store = store,
                    Err(e) => self.pending.push(self.err(&e)),
                }
                self.pending.push(format!("{DIM}[session reset]{RESET}"));
            }
            "/resume" => self.open_resume_selector(),
            "/tree" => self.open_tree_selector(),
            "/fork" => self.open_fork_selector(),
            "/usage" => {
                if !self.request_host_usage_modal() {
                    self.open_usage_selector();
                }
            }
            _ if trimmed == "/model" || trimmed.starts_with("/model ") => {
                let query = trimmed.strip_prefix("/model").unwrap().trim();
                if query.is_empty() {
                    if !self.request_host_model_modal(true) {
                        self.open_model_selector(true);
                    }
                } else {
                    self.switch_model(query, true);
                }
            }
            _ if trimmed == "/submodel" || trimmed.starts_with("/submodel ") => {
                let query = trimmed.strip_prefix("/submodel").unwrap().trim();
                if query.is_empty() {
                    if !self.request_host_model_modal(false) {
                        self.open_model_selector(false);
                    }
                } else {
                    self.switch_model(query, false);
                }
            }
            _ if trimmed == "/login" || trimmed.starts_with("/login ") => {
                let provider = trimmed.strip_prefix("/login").unwrap().trim().to_owned();
                self.start_login(&provider);
            }
            _ if trimmed == "/logout" || trimmed.starts_with("/logout ") => {
                let provider = trimmed.strip_prefix("/logout").unwrap().trim();
                let provider = if provider.is_empty() {
                    self.root_lm.model().provider
                } else {
                    provider.to_owned()
                };
                match self.storage.logout(&provider) {
                    Ok(()) => self
                        .pending
                        .push(format!("{DIM}[logged out of {provider}]{RESET}")),
                    Err(e) => self.pending.push(self.err(&e)),
                }
            }
            _ if trimmed.starts_with('!') && trimmed.len() > 1 => {
                // Shell escapes queue mid-run like prompts so they execute
                // against the REPL in submission order.
                if self.running.is_some() {
                    self.queued.push(trimmed.to_owned());
                } else {
                    self.send_shell(trimmed, cmd_tx);
                }
            }
            // Extension slash commands: dispatch to the registered handler
            // via the runtime (e.g. /recall, /capture, /explore from
            // builtin.memory). Anything that isn't a built-in or shell
            // escape and starts with '/' lands here.
            _ if trimmed.starts_with('/') => {
                let (cmd_name, args) = match trimmed.split_once(' ') {
                    Some((name, rest)) => (name, rest),
                    None => (trimmed, ""),
                };
                // Normalize a leading run of slashes (e.g. `//memories`) down
                // to a single slash so the command is looked up by the name
                // it was registered with.
                let cmd_name = {
                    let bare = cmd_name.trim_start_matches('/');
                    format!("/{bare}")
                };
                let args = args.to_owned();
                let runtime = Arc::clone(&self.runtime);
                let result = tokio::task::block_in_place(|| {
                    let rt = runtime;
                    let cn = cmd_name.clone();
                    let ar = args.clone();
                    tokio::runtime::Handle::current()
                        .block_on(async move { rt.invoke_command(&cn, &ar).await })
                });
                match result {
                    Ok(Some(output)) => {
                        if let Some(msg) = output.message {
                            self.pending.push(String::new());
                            self.pending.push(msg);
                        }
                    }
                    Ok(None) => {
                        self.pending
                            .push(self.warn(&format!("unknown command: {cmd_name}")));
                    }
                    Err(e) => {
                        self.pending.push(self.err(&e));
                    }
                }
            }
            prompt => {
                // Plain prompts submitted mid-run queue as follow-ups (slash
                // commands above run immediately).
                if self.running.is_some() {
                    self.queued.push(prompt.to_owned());
                } else {
                    self.send_prompt(prompt, cmd_tx);
                }
            }
        }
        false
    }

    pub(super) fn send_shell(&mut self, escape: &str, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        let (record, cmd) = match escape.strip_prefix("!!") {
            Some(rest) => (false, rest.trim()),
            None => (true, escape[1..].trim()),
        };
        if cmd.is_empty() {
            return;
        }
        let code = format!("print(bash({}))", python_quote(cmd));
        if record {
            self.begin_block();
            self.push_repl_block(0, &code, self.code_limit());
        } else {
            self.pending.push(String::new());
            self.pending
                .push(format!("{tool}${RESET} {DIM}{cmd}{RESET}", tool = &self.palette.tool_accent));
            self.last_activity = false;
        }
        let _ = cmd_tx.send(Cmd::Shell { code, record });
    }

    pub(super) fn send_prompt(&mut self, prompt: &str, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        self.push_user_message(prompt);
        if let Err(e) = self.store.append(session::Role::User, prompt) {
            self.pending
                .push(format!("{}session not saved:{RESET} {e}", &self.palette.warning_fg));
        }
        // Durable turn boundary: an exchange_start without its exchange_end
        // is how a crash mid-turn stays detectable after restart.
        let _ = self.store.begin_exchange();
        let cancel = CancellationToken::new();
        self.running = Some(cancel.clone());
        self.turn = 0;
        self.stream.clear();
        self.thinking.clear();
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
            self.pending
                .push(format!("{DIM}[a login is already in progress]{RESET}"));
            return;
        }
        let name = if provider.is_empty() {
            self.root_lm.model().provider
        } else {
            provider.to_owned()
        };
        let Some(provider) = OAuthProviderId::parse(&name) else {
            self.pending.push(self.err(
                &format!("unknown provider '{name}' (expected: anthropic, openai)")
            ));
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
                    self.pending.push(format!("{DIM}{line}{RESET}"));
                }
            }
            LoginMsg::Done { provider, result } => {
                self.login = None;
                match result {
                    Ok(()) => self
                        .pending
                        .push(format!("{BOLD}logged in to {provider}{RESET}")),
                    Err(e) if e.to_lowercase().contains("cancel") => {
                        self.pending.push(format!("{DIM}[login cancelled]{RESET}"))
                    }
                    Err(e) => self
                        .pending
                        .push(format!("{}login failed:{RESET} {e}", &self.palette.error_fg)),
                }
            }
        }
    }

    pub(super) fn handle_rlm_event(&mut self, event: RlmEvent) {
        match event {
            RlmEvent::IterationStart {
                depth: 0,
                iteration,
            } => {
                self.turn = iteration + 1;
                self.status = "thinking...".to_owned();
                self.stream.clear();
                self.thinking.clear();
                if iteration == 0 {
                    self.thinking_log.clear();
                } else if !self.thinking_log.trim().is_empty() {
                    self.thinking_log.push_str("\n\n");
                }
                self.streamed = false;
            }
            RlmEvent::RootDelta {
                depth: 0, delta, ..
            } => {
                self.stream.push_str(&delta);
                self.streamed = true;
                // Keep streamed assistant prose live-only. The durable JSONL
                // assistant message is appended at run completion, so the
                // retained transcript must also commit prose at that message
                // boundary instead of turning paragraph-stable deltas into
                // separate Assistant cards.
            }
            RlmEvent::RootThinkingDelta {
                depth: 0, delta, ..
            } => {
                self.thinking.push_str(&delta);
                self.thinking_log.push_str(&delta);
            }
            RlmEvent::RootText { depth: 0, text, .. } => {
                self.flush_thinking();
                // Commit the iteration's prose as durable cards, splitting on
                // fences so each becomes one assistant card (apart from the
                // explicit REPL/code cards). Prose preceding each executed
                // block was already committed eagerly by `CodeExecStart`, which
                // consumed it from `stream` to keep it ordered above the block;
                // for a streamed message only the uncommitted tail left in
                // `stream` may be flushed here. Re-splitting the full `text`
                // would re-render those earlier segments a second time, leaving
                // duplicated prose trailing each code block. When the model did
                // not stream, `stream` is empty and `RootText` carries the only
                // copy, so fall back to `text`.
                let source = if self.streamed {
                    std::mem::take(&mut self.stream)
                } else {
                    text
                };
                for segment in split_prose(&source) {
                    self.push_prose(segment);
                }
                self.stream.clear();
                self.streamed = false;
            }
            RlmEvent::CodeExecStart { depth, code, .. } => {
                if depth == 0 {
                    // The fence just closed mid-stream: flush the prose before
                    // it and consume both from the live tail.
                    if let Some((prose, rest)) = split_at_first_block(&self.stream) {
                        let (prose, rest) = (prose.to_owned(), rest.to_owned());
                        self.push_prose(&prose);
                        self.stream = rest;
                    }
                    self.status = "running code...".to_owned();
                    self.live_tools.clear();
                    self.live_subcalls.clear();
                    self.open_tool_args.clear();
                    self.open_tool_rows.clear();
                    self.open_subcall_rows.clear();
                    self.begin_block();
                    self.live_code = Some(code.clone());
                }
                self.push_repl_block(depth, &code, self.code_limit());
            }
            RlmEvent::CodeExecEnd {
                depth,
                stdout,
                stderr,
                duration,
                ..
            } => {
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
                        let _ = self.store.append_repl(session::ReplRecord {
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
                        // rendering path.
                        self.block.clear();
                        self.block_active = false;
                        self.after_block.clear();
                        self.push_repl_replay(
                            &code,
                            &stdout,
                            &stderr,
                            duration_secs,
                            &tools,
                            &subcalls,
                        );
                    } else {
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
            // Prelude tools run inside the REPL block at `depth`, so they
            // render one nested box level deeper, nested in that block.
            RlmEvent::ToolStart {
                depth,
                call_id,
                name,
                args,
            } => {
                let key = (depth, call_id);
                if self.live_code.is_some() {
                    self.open_tool_args.insert(key, args.clone());
                }
                let label = tool_title(&name, &args, &self.palette);
                if let Some((start, len)) =
                    self.push_status(depth + 1, running_status_text(&label, self.spinner.frame()))
                {
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
            RlmEvent::ToolEnd {
                depth,
                call_id,
                name,
                result,
                error,
                duration,
            } => {
                let key = (depth, call_id);
                let args = self.open_tool_args.remove(&key).unwrap_or_default();
                if self.live_code.is_some() {
                    // Replay only ever renders a short preview, and the full
                    // text already lands in the repl stdout payload, so cap
                    // what gets persisted per tool call.
                    self.live_tools.push(session::ToolCall {
                        name: name.clone(),
                        args: args.clone(),
                        result: truncate_for_log(&result),
                        error: error.as_deref().map(truncate_for_log),
                        duration_secs: duration.as_secs_f64(),
                        depth,
                    });
                }
                let output = error.as_deref().unwrap_or(&result);
                let lang = if error.is_some() {
                    None
                } else {
                    tool_preview_lang(&name, &args)
                };
                let rows = self.tool_result_rows(
                    depth + 1,
                    &name,
                    &args,
                    output,
                    self.output_limit(),
                    lang,
                    error.is_some(),
                    self.committed_ts().as_deref(),
                );
                if let Some(row) = self.open_tool_rows.remove(&key) {
                    self.replace_block_rows(row.start, row.len, rows);
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
            RlmEvent::SubcallStart {
                depth,
                model,
                prompt_preview,
            } => {
                let label = subcall_title(&model, &prompt_preview, &self.palette);
                if let Some((start, len)) =
                    self.push_status(depth, running_status_text(&label, self.spinner.frame()))
                {
                    self.open_subcall_rows.push_back(OpenSubcallRow {
                        depth,
                        prompt_preview: prompt_preview.clone(),
                        label,
                        start,
                        len,
                    });
                }
                if depth == 0 {
                    self.status = format!("waiting on {model}...");
                }
            }
            RlmEvent::SubcallEnd {
                depth,
                model,
                result_preview,
                duration,
                error,
            } => {
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
                        error: error.clone(),
                        duration_secs: duration.as_secs_f64(),
                        depth,
                    });
                }
                // Reuse the label formatted once at `SubcallStart` (stored on
                // the open row) instead of recomputing it for the border.
                let label = open
                    .as_ref()
                    .map(|row| row.label.clone())
                    .unwrap_or_else(|| subcall_title(&model, "", &self.palette));
                let content = error.as_deref().or(result_preview.as_deref());
                let rows = self.subcall_result_rows(
                    depth,
                    &label,
                    content,
                    self.output_limit(),
                    error.is_some(),
                    self.committed_ts().as_deref(),
                );
                if let Some(row) = open {
                    self.replace_block_rows(row.start, row.len, rows);
                    self.last_activity = true;
                } else {
                    if depth == 0 && !self.last_activity && !self.block_active {
                        self.pending.push(String::new());
                    }
                    for row in rows {
                        self.push_row(row);
                    }
                    self.last_activity = true;
                }
            }
            RlmEvent::RunEnd {
                depth,
                stop,
                iterations,
                duration,
            } if depth > 0 => {
                // A recursive subcall's header is the `SubcallStart` card; skip
                // the redundant `RunStart` "rlm" card (it would double-mark the
                // same nesting level). The run's outcome is summarized below.
                self.push_bar(
                    depth,
                    format!(
                        "{} · {} turn{} · {:.1}s",
                        format!("{stop:?}").to_lowercase(),
                        iterations,
                        if iterations == 1 { "" } else { "s" },
                        duration.as_secs_f64()
                    ),
                );
            }
            _ => {}
        }
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
                    self.pending
                        .push(format!("{}restore failed:{RESET} {e}", &self.palette.error_fg));
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
                if output.trim().is_empty() {
                    self.pending.push(format!("{DIM}(no output){RESET}"));
                } else {
                    self.pending
                        .extend(output.trim_end().lines().map(str::to_owned));
                }
                self.last_activity = false;
            }
            Err(e) => {
                if self.block_active {
                    self.push_bar(0, format!("failed: {e}"));
                    self.commit_block();
                } else {
                    self.pending.push(self.err(&e));
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
        self.running = None;
        self.stream.clear();
        self.thinking.clear();
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
                model: "__total__".to_owned(),
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
            let _ = self.store.append_with_usage_breakdown_and_display(
                session::Role::Assistant,
                &completion.response,
                display_text,
                Some(usage),
                usage_by_model,
            );
        }
        self.pending.push(String::new());
        if completion.stop == StopCause::Failed {
            let error = completion.error.unwrap_or_else(|| "run failed".to_owned());
            self.pending.push(self.err(&error));
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
        self.usage = completion.usage;
        self.pending.push(String::new());
        self.last_activity = false;
    }
}
