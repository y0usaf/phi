use super::*;

impl Ui {
    pub(super) fn open_selector(&mut self, kind: SelectorKind, widget: Selector) {
        self.popup = None;
        self.selector = Some((kind, widget));
    }

    pub(super) fn session_idle(&self) -> bool {
        self.running.is_none() && self.login.is_none()
    }

    fn require_idle(&mut self) -> bool {
        if !self.session_idle() {
            self.pending.push(TranscriptEntry::RawLines(vec![
                self.note(&"[wait for the current run to finish]"),
            ]));
            return false;
        }
        true
    }

    pub(super) fn open_resume_selector(&mut self) {
        if !self.require_idle() {
            return;
        }
        let current = self.store.path().to_owned();
        let items: Vec<SelectorItem> = SessionStore::list(&self.cwd)
            .into_iter()
            .filter(|s| s.path != current)
            .map(|s| {
                SelectorItem::new(s.path.display().to_string(), single_line(&s.first_message))
                    .detail(format!(
                        "{} msgs · {}",
                        s.message_count,
                        session::age(s.modified)
                    ))
            })
            .collect();
        if items.is_empty() {
            self.pending.push(TranscriptEntry::RawLines(vec![
                self.note(&"[no saved sessions for this directory]"),
            ]));
            return;
        }
        self.open_selector(
            SelectorKind::Resume,
            Selector::new("Resume Session", items, true).with_style(self.selector_style()),
        );
    }

    pub(super) fn open_fork_selector(&mut self) {
        if !self.require_idle() {
            return;
        }
        let messages = self.store.user_messages();
        if messages.is_empty() {
            self.pending.push(TranscriptEntry::RawLines(vec![
                self.note(&"[no messages to fork from]"),
            ]));
            return;
        }
        let total = messages.len();
        let items = messages
            .iter()
            .enumerate()
            .map(|(i, e)| {
                SelectorItem::new(e.id.clone(), single_line(&e.text))
                    .detail(format!("{}/{total}", i + 1))
            })
            .collect();
        let last = messages.last().expect("non-empty").id.clone();
        let mut widget =
            Selector::new("Fork from Message", items, false).with_style(self.selector_style());
        widget.preselect(&last);
        self.open_selector(SelectorKind::Fork, widget);
    }

    pub(super) fn open_tree_selector(&mut self) {
        if !self.require_idle() {
            return;
        }
        let leaf = self.store.leaf().map(str::to_owned);
        let items: Vec<SelectorItem> = self
            .store
            .tree_rows()
            .into_iter()
            .map(|(depth, entry)| {
                let marker = match entry.role {
                    session::Role::User => "•",
                    session::Role::Assistant => "·",
                };
                SelectorItem::new(
                    entry.id.clone(),
                    format!(
                        "{}{marker} {}",
                        "  ".repeat(depth),
                        single_line(&entry.text)
                    ),
                )
                .current(Some(entry.id.as_str()) == leaf.as_deref())
            })
            .collect();
        if items.is_empty() {
            self.pending.push(TranscriptEntry::RawLines(vec![
                self.note(&"[no entries in this session]"),
            ]));
            return;
        }
        let mut widget =
            Selector::new("Session Tree", items, false).with_style(self.selector_style());
        if let Some(leaf) = &leaf {
            widget.preselect(leaf);
        }
        self.open_selector(SelectorKind::Tree, widget);
    }

    pub(super) fn handle_selector_key(
        &mut self,
        key: phi_tui::crossterm::event::KeyEvent,
        cmd_tx: &mpsc::UnboundedSender<Cmd>,
    ) {
        let Some((_, widget)) = self.selector.as_mut() else {
            return;
        };
        match widget.handle_key(key) {
            SelectorOutcome::Pending => {}
            SelectorOutcome::Cancelled => {
                // A dropped Prompt reply sender reads as "dismissed" on the
                // asking extension's side — no explicit send needed.
                self.selector = None;
                self.service_ui_prompts();
            }
            SelectorOutcome::Confirmed(index) => {
                let (kind, widget) = self.selector.take().expect("selector open");
                let value = widget.items()[index].value.clone();
                match kind {
                    SelectorKind::Model { root } => self.switch_model(&value, root),
                    SelectorKind::Usage => {}
                    SelectorKind::Resume => self.resume_session(&value, cmd_tx),
                    SelectorKind::Fork => self.fork_from(&value, cmd_tx),
                    SelectorKind::Tree => self.navigate_tree(&value, cmd_tx),
                    SelectorKind::Prompt { reply } => {
                        // Buffered (capacity 1): never blocks the UI loop.
                        let _ = reply.send(Some(index));
                    }
                }
                self.service_ui_prompts();
            }
        }
    }

    /// Queue an extension's ask-the-user request and show it as soon as no
    /// other selector is open. Requests dropped unanswered (quit, run end,
    /// replacement) read as "dismissed" on the asking side, which gating
    /// extensions treat as deny — always fail-safe.
    pub(super) fn handle_ui_prompt(&mut self, req: phi_ext::UiPromptRequest) {
        self.ui_prompts.push_back(req);
        self.service_ui_prompts();
    }

    pub(super) fn service_ui_prompts(&mut self) {
        if self.selector.is_some() {
            return;
        }
        let Some(phi_ext::UiPromptRequest { prompt, reply }) = self.ui_prompts.pop_front() else {
            return;
        };
        // The gated subject (command/code/path) goes into the transcript —
        // the selector itself has a one-line title, and the user must see
        // what they are approving. Interception fires before the tool's own
        // transcript card exists, so this is the only copy at prompt time.
        const BODY_PREVIEW_LINES: usize = 12;
        self.ensure_pending_gap();
        let mut lines = vec![format!(
            "{}approval:{RESET} {}",
            &self.palette.warning_fg, prompt.title
        )];
        let body_lines: Vec<&str> = prompt.body.lines().collect();
        for line in body_lines.iter().take(BODY_PREVIEW_LINES) {
            lines.push(format!(
                "{}  {}{RESET}",
                &self.palette.text_tertiary,
                phi_tui::truncate_line_with_ellipsis_to_width(
                    line,
                    self.panel_width().saturating_sub(2)
                )
            ));
        }
        if body_lines.len() > BODY_PREVIEW_LINES {
            lines.push(self.note(&format!(
                "  … ({} more lines)",
                body_lines.len() - BODY_PREVIEW_LINES
            )));
        }
        self.pending.push(TranscriptEntry::RawLines(lines));
        self.last_activity = false;

        let items = prompt
            .options
            .iter()
            .enumerate()
            .map(|(i, opt)| {
                let mut item = SelectorItem::new(i.to_string(), opt.label.clone());
                if let Some(detail) = &opt.detail {
                    item = item.detail(detail.clone());
                }
                item
            })
            .collect();
        let widget = Selector::new(prompt.title, items, false).with_style(self.selector_style());
        self.open_selector(SelectorKind::Prompt { reply }, widget);
    }

    /// Drop the active prompt selector and any queued requests (each reads
    /// as "dismissed" → deny on the asking side). Called when a run ends:
    /// the engine that was waiting on the answer is gone.
    pub(super) fn dismiss_ui_prompts(&mut self) {
        if matches!(self.selector, Some((SelectorKind::Prompt { .. }, _))) {
            self.selector = None;
        }
        self.ui_prompts.clear();
    }

    pub(super) fn resume_session(&mut self, path: &str, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        match SessionStore::load(std::path::Path::new(path)) {
            Ok(mut store) => {
                // Best-effort dead-line reclaim at the open-for-use moment.
                let _ = store.compact();
                self.store = store;
                self.pending
                    .push(TranscriptEntry::RawLines(vec![String::new()]));
                self.pending.push(TranscriptEntry::RawLines(vec![
                    self.note(&format!("[resumed session {}]", self.store.id())),
                ]));
                self.replay_branch();
                self.restore_rlm(cmd_tx);
            }
            Err(e) => self
                .pending
                .push(TranscriptEntry::RawLines(vec![self.err(&e)])),
        }
    }

    pub(super) fn fork_from(&mut self, entry_id: &str, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        let text = self
            .store
            .entry(entry_id)
            .map(|e| e.text.clone())
            .unwrap_or_default();
        match self.store.fork(entry_id) {
            Ok(store) => {
                self.store = store;
                self.pending
                    .push(TranscriptEntry::RawLines(vec![String::new()]));
                self.pending.push(TranscriptEntry::RawLines(vec![
                    self.note(&"[forked to new session]"),
                ]));
                self.replay_branch();
                self.restore_rlm(cmd_tx);
                self.editor.set_text(&text);
            }
            Err(e) => self
                .pending
                .push(TranscriptEntry::RawLines(vec![self.err(&e)])),
        }
    }

    pub(super) fn navigate_tree(&mut self, entry_id: &str, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        if self.store.leaf() == Some(entry_id) {
            self.pending.push(TranscriptEntry::RawLines(vec![
                self.note(&"[already at this point]"),
            ]));
            return;
        }
        let Some(entry) = self.store.entry(entry_id) else {
            return;
        };
        let (leaf, editor_text) = match entry.role {
            session::Role::User => (entry.parent_id.clone(), Some(entry.text.clone())),
            session::Role::Assistant => (Some(entry.id.clone()), None),
        };
        let _ = self.store.set_leaf(leaf);
        self.pending
            .push(TranscriptEntry::RawLines(vec![String::new()]));
        self.pending.push(TranscriptEntry::RawLines(vec![
            self.note(&"[navigated to selected point]"),
        ]));
        self.replay_branch();
        self.restore_rlm(cmd_tx);
        if let Some(text) = editor_text
            && self.editor.text().trim().is_empty()
        {
            self.editor.set_text(&text);
        }
    }

    pub(super) fn restore_rlm(&mut self, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        let (exchanges, pending) = self.store.branch_restore();
        let _ = cmd_tx.send(Cmd::Restore { exchanges, pending });
    }

    pub(super) fn replay_branch(&mut self) {
        self.clear_transcript();
        let _ = self.term.clear_all();
        enum ReplayItem {
            Message(session::Role, String),
            Repl {
                prose: String,
                code: String,
                stdout: String,
                stderr: String,
                duration_secs: f64,
                tools: Vec<session::ToolCall>,
                subcalls: Vec<session::SubcallRecord>,
            },
        }
        let items: Vec<ReplayItem> = self
            .store
            .branch_items()
            .into_iter()
            .map(|item| match item {
                session::BranchItem::Message(entry) => {
                    let text = match entry.role {
                        session::Role::Assistant => entry
                            .display_text
                            .clone()
                            .unwrap_or_else(|| entry.text.clone()),
                        session::Role::User => entry.text.clone(),
                    };
                    ReplayItem::Message(entry.role, text)
                }
                session::BranchItem::Repl(repl) => ReplayItem::Repl {
                    prose: repl.prose.clone(),
                    code: repl.code.clone(),
                    stdout: repl.stdout.clone(),
                    stderr: repl.stderr.clone(),
                    duration_secs: repl.duration_secs,
                    tools: repl.tools.clone(),
                    subcalls: repl.subcalls.clone(),
                },
            })
            .collect();
        for item in items {
            match item {
                ReplayItem::Message(role, text) => match role {
                    session::Role::User => self.push_user_message(&text),
                    session::Role::Assistant => self.push_assistant_message(&text),
                },
                ReplayItem::Repl {
                    prose,
                    code,
                    stdout,
                    stderr,
                    duration_secs,
                    tools,
                    subcalls,
                } => self.push_repl_replay(
                    &prose,
                    &code,
                    &stdout,
                    &stderr,
                    duration_secs,
                    &tools,
                    &subcalls,
                ),
            }
        }
        // A previous run died mid-turn (exchange_start without its
        // exchange_end): say so instead of silently presenting a transcript
        // that ends earlier than the user remembers, then durably close the
        // exchange so the notice appears once.
        if self.store.interrupted_exchange().is_some() {
            self.pending
                .push(TranscriptEntry::RawLines(vec![String::new()]));
            // A crash is a high-stakes state: label it in the error color so
            // it never reads like a routine navigation note, and wrap it so
            // narrow terminals don't overflow.
            let notice = format!(
                "{}interrupted:{RESET} previous run ended mid-turn — completed work up to the last checkpoint is shown above",
                &self.palette.error_fg
            );
            self.pending.push(TranscriptEntry::RawLines(wrap_ansi(
                &notice,
                self.panel_width(),
            )));
            self.last_activity = false;
            let _ = self.store.acknowledge_interrupted();
        }
    }
}
