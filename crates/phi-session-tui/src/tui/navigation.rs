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
            self.pending.push(TranscriptEntry::RawLines(vec![format!("{DIM}[wait for the current run to finish]{RESET}")]));
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
            self.pending.push(TranscriptEntry::RawLines(vec![format!(
                "{DIM}[no saved sessions for this directory]{RESET}"
            )]));
            return;
        }
        self.open_selector(
            SelectorKind::Resume,
            Selector::new("Resume Session", items, true),
        );
    }

    pub(super) fn open_fork_selector(&mut self) {
        if !self.require_idle() {
            return;
        }
        let messages = self.store.user_messages();
        if messages.is_empty() {
            self.pending.push(TranscriptEntry::RawLines(vec![format!("{DIM}[no messages to fork from]{RESET}")]));
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
        let mut widget = Selector::new("Fork from Message", items, false);
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
                    session::Role::User => "›",
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
            self.pending.push(TranscriptEntry::RawLines(vec![format!("{DIM}[no entries in this session]{RESET}")]));
            return;
        }
        let mut widget = Selector::new("Session Tree", items, false);
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
            SelectorOutcome::Cancelled => self.selector = None,
            SelectorOutcome::Confirmed(index) => {
                let (kind, widget) = self.selector.take().expect("selector open");
                let value = widget.items()[index].value.clone();
                match kind {
                    SelectorKind::Model { root } => self.switch_model(&value, root),
                    SelectorKind::Usage => {}
                    SelectorKind::Resume => self.resume_session(&value, cmd_tx),
                    SelectorKind::Fork => self.fork_from(&value, cmd_tx),
                    SelectorKind::Tree => self.navigate_tree(&value, cmd_tx),
                }
            }
        }
    }

    pub(super) fn resume_session(&mut self, path: &str, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        match SessionStore::load(std::path::Path::new(path)) {
            Ok(store) => {
                self.store = store;
                self.pending.push(TranscriptEntry::RawLines(vec![String::new()]));
                self.pending.push(TranscriptEntry::RawLines(vec![format!("{DIM}[resumed session {}]{RESET}", self.store.id())]));
                self.replay_branch();
                self.restore_rlm(cmd_tx);
            }
            Err(e) => self.pending.push(TranscriptEntry::RawLines(vec![self.err(&e)])),
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
                self.pending.push(TranscriptEntry::RawLines(vec![String::new()]));
                self.pending.push(TranscriptEntry::RawLines(vec![format!("{DIM}[forked to new session]{RESET}")]));
                self.replay_branch();
                self.restore_rlm(cmd_tx);
                self.editor.set_text(&text);
            }
            Err(e) => self.pending.push(TranscriptEntry::RawLines(vec![self.err(&e)])),
        }
    }

    pub(super) fn navigate_tree(&mut self, entry_id: &str, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
        if self.store.leaf() == Some(entry_id) {
            self.pending.push(TranscriptEntry::RawLines(vec![format!("{DIM}[already at this point]{RESET}")]));
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
        self.pending.push(TranscriptEntry::RawLines(vec![String::new()]));
        self.pending.push(TranscriptEntry::RawLines(vec![format!("{DIM}[navigated to selected point]{RESET}")]));
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
                    code,
                    stdout,
                    stderr,
                    duration_secs,
                    tools,
                    subcalls,
                } => {
                    self.push_repl_replay(&code, &stdout, &stderr, duration_secs, &tools, &subcalls)
                }
            }
        }
        // A previous run died mid-turn (exchange_start without its
        // exchange_end): say so instead of silently presenting a transcript
        // that ends earlier than the user remembers, then durably close the
        // exchange so the notice appears once.
        if self.store.interrupted_exchange().is_some() {
            self.pending.push(TranscriptEntry::RawLines(vec![String::new()]));
            self.pending.push(TranscriptEntry::RawLines(vec![format!(
                "{}previous run was interrupted mid-turn; completed work up to the last checkpoint is shown above{RESET}",
                &self.palette.warning_fg
            )]));
            self.last_activity = false;
            let _ = self.store.acknowledge_interrupted();
        }
    }
}
