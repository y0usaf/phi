use super::*;

impl MuxApp {
    pub(crate) async fn run_action(&mut self, vid: u64, action: AppAction) {
        match action {
            AppAction::PreviousProject | AppAction::NextProject => {
                let count = self.workspace.projects.len();
                if count > 0 {
                    let delta: i32 = if action == AppAction::NextProject {
                        1
                    } else {
                        -1
                    };
                    if let Some(view) = self.views.get_mut(&vid) {
                        let current = view.focus.project as i32;
                        view.focus.project = (current + delta).rem_euclid(count as i32) as usize;
                    }
                    self.on_focus_changed(vid);
                    self.ensure_focused_session(vid).await;
                }
            }
            AppAction::PreviousSession | AppAction::NextSession => {
                let delta: i32 = if action == AppAction::NextSession {
                    1
                } else {
                    -1
                };
                let Some(view) = self.views.get_mut(&vid) else {
                    return;
                };
                let project_index = view.focus.project;
                if view
                    .focus
                    .move_visible(&self.workspace, project_index, delta)
                {
                    self.on_focus_changed(vid);
                    self.ensure_focused_session(vid).await;
                }
            }
            AppAction::NewSession => {
                let Some(project_index) = self.views.get(&vid).map(|v| v.focus.project) else {
                    return;
                };
                // The draft inserts at the top of the session list; other
                // views' selections are re-resolved by key around it.
                let draft =
                    self.with_focus_preserved(project_index, |ws| ws.new_draft(project_index));
                if let Some(draft) = draft
                    && let Some(view) = self.views.get_mut(&vid)
                {
                    view.focus.set_selected_session(project_index, draft);
                }
                self.on_focus_changed(vid);
                self.ensure_focused_session(vid).await;
            }
            AppAction::RefreshSession => self.refresh_focused_session(vid),
            AppAction::RefreshAllSessions => {
                for index in 0..self.workspace.projects.len() {
                    let _ = self.refresh_project_all_views(index);
                }
                self.set_note_on(vid, "rescanned all projects", StatusNoteKind::Ok);
            }
            AppAction::ArchiveSession => self.archive_focused_session(vid).await,
            AppAction::RemoveProject => {
                let Some(index) = self.views.get(&vid).map(|v| v.focus.project) else {
                    return;
                };
                if let Some(project) = self.workspace.projects.get(index) {
                    let title = project_sidebar_title(project);
                    self.workspace.remove_project(index);
                    // Indices shift for every view, not just the acting one.
                    let remaining = self.workspace.projects.len();
                    for view in self.views.values_mut() {
                        view.focus.project_removed(index, remaining);
                    }
                    self.sync_watchers();
                    self.set_note(format!("removed {title}"), StatusNoteKind::Ok);
                    self.on_focus_changed(vid);
                    self.ensure_focused_session(vid).await;
                }
            }
            AppAction::CopySelection => self.copy_selection(vid, true),
            AppAction::PasteClipboard => {
                if let Some(registry) = self.registry() {
                    // Daemon mode: the system clipboard lives on the client's
                    // machine. Ask the pasting client to read it; the text
                    // comes back as a ClipboardText event.
                    if !registry.send_to(vid, ServerMsg::ClipboardRequest) {
                        self.set_note_on(
                            vid,
                            "clipboard unavailable: no client",
                            StatusNoteKind::Error,
                        );
                    }
                } else if let Some(text) = self.clipboard.read_text() {
                    self.forward_to_session(vid, Event::Paste(text));
                } else {
                    self.set_note_on(vid, "clipboard empty", StatusNoteKind::Error);
                }
            }
            AppAction::Help => self.toggle_help_overlay(vid),
            AppAction::Quit => self.quit = true,
        }
    }

    /// Open an extension-registered overlay by name on one client's view,
    /// with an optional payload for its `init_state`. Replaces any overlay
    /// active on that view (whose state is dropped — pending responders fire
    /// their default). Returns `false` when no overlay is registered under
    /// `name` or the view is gone.
    pub(crate) fn open_extension_overlay(
        &mut self,
        vid: u64,
        name: &str,
        payload: Option<phi_ext::OverlayPayload>,
    ) -> bool {
        let Some(spec) = self
            .extension_overlays
            .iter()
            .find(|spec| spec.name == name)
            .cloned()
        else {
            return false;
        };
        let Some(view) = self.views.get_mut(&vid) else {
            return false;
        };
        let state = (spec.init_state)(payload);
        view.active_extension_overlay = Some(ActiveExtensionOverlay {
            name: spec.name.clone(),
            spec,
            state,
        });
        true
    }

    /// Toggle an overlay by name on one client's view: close it if it's the
    /// active one, open it otherwise. Posts a status note when the overlay
    /// isn't registered (e.g. a build without `phi-builtins`).
    pub(crate) fn toggle_extension_overlay(
        &mut self,
        vid: u64,
        name: &str,
        payload: Option<phi_ext::OverlayPayload>,
    ) {
        if self
            .views
            .get(&vid)
            .and_then(|view| view.active_extension_overlay.as_ref())
            .is_some_and(|active| active.name == name)
        {
            if let Some(view) = self.views.get_mut(&vid) {
                view.active_extension_overlay = None;
            }
        } else if !self.open_extension_overlay(vid, name, payload) {
            self.set_note_on(
                vid,
                format!("overlay {name} not available"),
                StatusNoteKind::Error,
            );
        }
    }

    pub(crate) fn toggle_help_overlay(&mut self, vid: u64) {
        let payload = phi_ext::HelpPayload {
            entries: help_entries(&self.keymap),
        };
        self.toggle_extension_overlay(vid, phi_ext::OVERLAY_HELP, Some(Box::new(payload)));
    }

    pub(crate) fn open_usage_overlay(&mut self, vid: u64) {
        if !self.open_extension_overlay(vid, phi_ext::OVERLAY_USAGE, None) {
            self.set_note_on(vid, "usage overlay not available", StatusNoteKind::Error);
        }
    }

    /// Rescan the focused project; a crashed/exited focused session is
    /// restarted in place against the same JSONL.
    pub(crate) fn refresh_focused_session(&mut self, vid: u64) {
        let restart = self
            .focused_session(vid)
            .is_some_and(|s| matches!(s.status, SessionStatus::Crashed | SessionStatus::Done));
        if restart {
            if let Some(session) = self.focused_session_mut(vid) {
                if session.store_path.is_none()
                    && let Some(handle) = &session.handle
                {
                    let path = handle.store_path.clone();
                    if path.exists() {
                        session.store_path = Some(path);
                    }
                }
                session.handle = None;
                session.status = SessionStatus::Idle;
            }
            self.set_note_on(vid, "session restarted", StatusNoteKind::Ok);
        } else {
            if let Some(index) = self.views.get(&vid).map(|v| v.focus.project) {
                let _ = self.refresh_project_all_views(index);
            }
            self.set_note_on(vid, "project rescanned", StatusNoteKind::Ok);
        }
    }

    pub(crate) async fn archive_focused_session(&mut self, vid: u64) {
        let Some(view) = self.views.get(&vid) else {
            return;
        };
        let project_index = view.focus.project;
        let selected = view.focus.selected_session(project_index);
        let discarded = self.with_focus_preserved(project_index, |ws| {
            ws.discard_unmaterialized_draft(project_index, selected)
        });
        if discarded {
            let draft = self.with_focus_preserved(project_index, |ws| ws.new_draft(project_index));
            if let Some(draft) = draft
                && let Some(view) = self.views.get_mut(&vid)
            {
                view.focus.set_selected_session(project_index, draft);
            }
            self.set_note_on(vid, "draft deleted", StatusNoteKind::Ok);
            self.on_focus_changed(vid);
            return;
        }

        let Some(path) = self.focused_session(vid).and_then(|s| {
            s.store_path.clone().or_else(|| {
                s.handle
                    .as_ref()
                    .map(|h| h.store_path.clone())
                    .filter(|p| p.exists())
            })
        }) else {
            self.set_note_on(vid, "draft has no file to archive", StatusNoteKind::Error);
            return;
        };
        match workspace::archive_session(&path) {
            Ok(_) => {
                if let Some(view) = self.views.get_mut(&vid) {
                    view.selection.clear();
                }
                if let Some(session) = self.focused_session_mut(vid) {
                    session.handle = None;
                    session.store_path = None;
                }
                let _ = self.refresh_project_all_views(project_index);
                if let Some(view) = self.views.get_mut(&vid) {
                    view.focus.normalize_visible(&self.workspace, project_index);
                }
                // Local mode only: the daemon has no tty to drain.
                if self.registry().is_none() {
                    crate::driver::drain_pending_terminal_events();
                }
                self.ensure_focused_session(vid).await;
                self.set_note_on(vid, "session archived", StatusNoteKind::Ok);
                self.on_focus_changed(vid);
            }
            Err(_error) => {
                // Archive I/O is copy+delete fallback — an error here means
                // the session file is still safely in place. Stay silent.
            }
        }
    }

    pub(crate) fn restore_archived(&mut self, vid: u64, archived: &std::path::Path) {
        match workspace::restore_session(archived) {
            Ok(target) => {
                if let Some(view) = self.views.get_mut(&vid) {
                    view.archive_viewer = None;
                }
                for index in 0..self.workspace.projects.len() {
                    let _ = self.refresh_project_all_views(index);
                }
                self.set_note_on(
                    vid,
                    format!("restored {}", target.display()),
                    StatusNoteKind::Ok,
                );
            }
            Err(_error) => {
                // Restore I/O is copy+delete fallback — the archived file is
                // still safely in place. Reload the viewer silently.
                if let Some(viewer) = self
                    .views
                    .get_mut(&vid)
                    .and_then(|view| view.archive_viewer.as_mut())
                {
                    viewer.reload_sessions();
                }
            }
        }
    }

    pub(crate) fn copy_selection(&mut self, vid: u64, note_on_empty: bool) {
        let Some(range) = self
            .views
            .get(&vid)
            .and_then(|view| view.selection.normalized())
        else {
            if note_on_empty {
                self.set_note_on(vid, "nothing selected", StatusNoteKind::Error);
            }
            return;
        };
        let Some(handle) = self.focused_session(vid).and_then(|s| s.handle.as_ref()) else {
            return;
        };
        let text = {
            let parser = driver::lock_parser(&handle.parser);
            selected_text(parser.screen(), range)
        };
        if text.trim().is_empty() {
            return;
        }
        if let Some(registry) = self.registry() {
            // Daemon mode: OSC 52 makes the selecting CLIENT's terminal
            // emulator write its local clipboard — the daemon never touches
            // a display server. Sent as an out-of-band frame (written
            // verbatim).
            use base64::Engine as _;
            let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
            let osc52 = format!("\x1b]52;c;{encoded}\x07");
            registry.send_to(vid, ServerMsg::Frame(osc52));
        } else if let Err(note) = self.clipboard.copy_text(&text) {
            self.set_note_on(vid, note, StatusNoteKind::Error);
        }
    }

    pub(crate) async fn run_command_input(&mut self, vid: u64, input: &str) {
        let input = input.trim();
        if input.is_empty() {
            return;
        }
        match parse_command(input) {
            Ok(MuxCommand::Open(path)) => {
                // In daemon mode a relative path would resolve against the
                // daemon's own cwd; resolve against the typing client's cwd
                // (carried in its Attach handshake) instead.
                let path = if path.is_relative() {
                    self.registry()
                        .and_then(|r| r.client_cwd(vid))
                        .map(|base| base.join(&path))
                        .unwrap_or(path)
                } else {
                    path
                };
                if path.is_dir() {
                    let path = PathBuf::from(persisted::normalize_path(&path));
                    let index = self.workspace.add_project(path);
                    if let Some(view) = self.views.get_mut(&vid) {
                        view.focus.project = index;
                    }
                    self.workspace.ensure_project_session(index);
                    self.sync_watchers();
                    self.on_focus_changed(vid);
                } else {
                    self.set_note_on(
                        vid,
                        format!("not a directory: {}", path.display()),
                        StatusNoteKind::Error,
                    );
                }
            }
            Ok(MuxCommand::Archive) => {
                if let Some(view) = self.views.get_mut(&vid) {
                    view.archive_viewer = Some(ArchiveViewerState::load());
                }
            }
            Ok(MuxCommand::Cleanup) => match workspace::cleanup_archive(ARCHIVE_MAX_AGE) {
                Ok(count) => {
                    self.set_note_on(vid, format!("{count} archives removed"), StatusNoteKind::Ok);
                }
                Err(error) => {
                    self.set_note_on(vid, format!("cleanup: {error:#}"), StatusNoteKind::Error);
                }
            },
            Ok(MuxCommand::Usage) => self.open_usage_overlay(vid),
            Ok(MuxCommand::Refresh) => self.run_action(vid, AppAction::RefreshSession).await,
            Ok(MuxCommand::Reload) => self.run_action(vid, AppAction::RefreshAllSessions).await,
            Ok(MuxCommand::Quit) => self.quit = true,
            Ok(MuxCommand::Action(action)) => self.run_action(vid, action).await,
            Ok(MuxCommand::Help) => self.toggle_help_overlay(vid),
            Err(note) => self.set_note_on(vid, note, StatusNoteKind::Error),
        }
    }

    pub(crate) fn handle_host_modal_request(&mut self, key: SessionKey, request: HostModalRequest) {
        match request.kind {
            HostModalKind::Usage => {
                // Usage has no reply payload, so it can open on every view
                // focused on the requesting session.
                let viewers = self.views_focused_on(key);
                if viewers.is_empty() {
                    let _ = request
                        .response_tx
                        .send(Err("session is not focused by any client".to_owned()));
                    return;
                }
                for vid in viewers {
                    self.open_usage_overlay(vid);
                }
                let _ = request.response_tx.send(Ok(None));
            }
            HostModalKind::Model { root, current } => {
                // The picker carries a single responder, so it opens on one
                // view: the last active client if it's viewing the session,
                // otherwise any viewer.
                let Some(vid) = self.route_client_for_session(key) else {
                    let _ = request.response_tx.send(Err(
                        "session lost focus before opening model picker".to_owned(),
                    ));
                    return;
                };
                if !self
                    .extension_overlays
                    .iter()
                    .any(|spec| spec.name == phi_ext::OVERLAY_MODEL)
                {
                    let _ = request
                        .response_tx
                        .send(Err("model picker overlay not available".to_owned()));
                    return;
                }
                let models = self
                    .shared
                    .registry
                    .supported_models()
                    .map(|m| format!("{}/{}", m.provider, m.id))
                    .collect::<Vec<_>>();
                if models.is_empty() {
                    let _ = request
                        .response_tx
                        .send(Err("no supported models available".to_owned()));
                    return;
                }
                // Opening replaces any overlay active on that view; a
                // superseded picker's responder fires Ok(None) when its
                // state drops.
                let response_tx = request.response_tx;
                let payload = phi_ext::ModelPickerPayload {
                    root,
                    current,
                    models,
                    responder: phi_ext::OverlayResponder::new(move |result| {
                        let _ = response_tx.send(result);
                    }),
                };
                self.open_extension_overlay(vid, phi_ext::OVERLAY_MODEL, Some(Box::new(payload)));
            }
        }
    }

    pub(crate) fn handle_suspend(
        &mut self,
        host: &HostIo<'_>,
        key: SessionKey,
        request: tui::SuspendRequest,
    ) {
        match host {
            HostIo::Local(guard) => {
                let mut stdout = std::io::stdout();
                let _ = phi_tui::crossterm::execute!(stdout, DisableMouseCapture);
                let result = guard.with_suspended(|| tui::external_editor(&request.text));
                let _ = phi_tui::crossterm::execute!(stdout, EnableMouseCapture);
                let _ = request.response_tx.send(result);
                for view in self.views.values_mut() {
                    view.renderer.invalidate();
                }
            }
            // The daemon has no tty: forward to a client viewing the
            // requesting session, which runs $EDITOR on its own terminal and
            // replies with an EditResult.
            HostIo::Remote => {
                let target = self.route_client_for_session(key);
                let sent = target.is_some_and(|id| {
                    self.registry().is_some_and(|registry| {
                        registry.send_to(
                            id,
                            ServerMsg::EditRequest {
                                text: request.text.clone(),
                            },
                        )
                    })
                });
                match (target, sent) {
                    (Some(id), true) => self.pending_edits.push((id, request.response_tx)),
                    _ => {
                        let _ = request
                            .response_tx
                            .send(Err("no attached client is viewing this session".to_owned()));
                    }
                }
            }
        }
    }
}

/// Build the help overlay's content from the live keymap. The content is
/// host data (mux commands + current keybindings); the overlay itself is
/// registered by `phi-builtins` and renders whatever entries it's handed.
fn help_entries(keymap: &Keymap) -> Vec<phi_ext::HelpEntry> {
    use phi_ext::HelpEntry;
    fn item(keys: &str, description: &str) -> HelpEntry {
        HelpEntry::Item {
            keys: keys.to_owned(),
            description: description.to_owned(),
        }
    }

    let mut entries = vec![
        HelpEntry::Heading("Global".to_owned()),
        item("ctrl+g / :help", "toggle this help"),
        item(":open <dir>", "open project"),
        item(":archive", "browse archived sessions"),
        item(":usage", "show usage totals"),
        item(":refresh / :reload", "refresh current / all sessions"),
        item(":up / :down", "previous / next session"),
        item(":left / :right", "previous / next project"),
        item(":quit / ctrl+q", "quit"),
        HelpEntry::Blank,
        HelpEntry::Heading("Navigation & actions".to_owned()),
    ];

    for action in [
        AppAction::PreviousProject,
        AppAction::NextProject,
        AppAction::PreviousSession,
        AppAction::NextSession,
        AppAction::NewSession,
        AppAction::RefreshSession,
        AppAction::RefreshAllSessions,
        AppAction::ArchiveSession,
        AppAction::RemoveProject,
        AppAction::CopySelection,
        AppAction::PasteClipboard,
    ] {
        let bindings = keymap.bindings_for(action).join(" / ");
        if !bindings.is_empty() {
            entries.push(item(&bindings, action_help_text(action)));
        }
    }

    entries.extend([
        HelpEntry::Blank,
        HelpEntry::Heading("Terminal".to_owned()),
        item("mouse drag", "select text"),
        item("wheel / scroll keys", "scroll terminal"),
        item("::", "send literal ':' to terminal"),
        HelpEntry::Blank,
        HelpEntry::Heading("Command line".to_owned()),
        item("ctrl+a/e", "start / end"),
        item("ctrl+w/u", "delete word / clear"),
        item("backspace", "delete; empty input closes"),
        HelpEntry::Blank,
        HelpEntry::Heading("Help / archive dialogs".to_owned()),
        item("↑/↓ j/k wheel", "scroll / select"),
        item("pgup/pgdn", "page up / down"),
        item("home/end g/G", "jump top / bottom"),
        item("esc q ctrl+c", "close"),
    ]);
    entries
}

fn action_help_text(action: AppAction) -> &'static str {
    match action {
        AppAction::PreviousProject => "previous project",
        AppAction::NextProject => "next project",
        AppAction::PreviousSession => "previous session",
        AppAction::NextSession => "next session",
        AppAction::NewSession => "new session",
        AppAction::RefreshSession => "refresh session",
        AppAction::RefreshAllSessions => "refresh all sessions",
        AppAction::ArchiveSession => "archive selected session",
        AppAction::RemoveProject => "remove project from sidebar",
        AppAction::CopySelection => "copy terminal selection",
        AppAction::PasteClipboard => "paste clipboard",
        AppAction::Help => "toggle help",
        AppAction::Quit => "quit",
    }
}
