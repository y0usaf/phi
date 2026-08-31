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
            AppAction::OpenCommandLine => {
                if let Some(view) = self.views.get_mut(&vid) {
                    view.command_line = Some(CommandLineState::new());
                }
            }
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
        // Registry-driven command listing: Mux-context commands from the
        // visual runtime (empty without one, e.g. a bare-core build).
        let commands = self
            .visual_runtime
            .as_ref()
            .map(|rt| rt.list_commands_in(phi_ext::CommandContext::Mux))
            .unwrap_or_default();
        let shortcuts = self
            .visual_runtime
            .as_ref()
            .map(|rt| rt.list_shortcuts())
            .unwrap_or_default();
        let payload = phi_ext::HelpPayload {
            entries: help_entries(&self.keymap, &commands, &shortcuts),
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
        match phi_session::archive::archive_session(&path) {
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

    /// Dispatch a `:` command line through the `CommandSpec` registry
    /// (Mux-context commands, aliases included) and interpret the returned
    /// declarative `UiAction`s as host effects.
    pub(crate) async fn run_command_input(&mut self, vid: u64, input: &str) {
        let input = input.trim();
        if input.is_empty() {
            return;
        }
        let input = input.strip_prefix(':').unwrap_or(input).trim();
        if input.is_empty() {
            self.set_note_on(vid, "empty command", StatusNoteKind::Error);
            return;
        }
        let name_end = input.find(char::is_whitespace).unwrap_or(input.len());
        let name = input[..name_end].to_ascii_lowercase();
        let args = input[name_end..].trim();
        let result = self
            .visual_runtime
            .as_ref()
            .map(|rt| rt.invoke_command_sync_in(phi_ext::CommandContext::Mux, &name, args));
        let output = match result {
            None | Some(Ok(None)) => {
                self.set_note_on(
                    vid,
                    format!("unknown command: :{name}"),
                    StatusNoteKind::Error,
                );
                return;
            }
            Some(Err(error)) => {
                // Handler errors carry user-facing text (e.g. the `:open`
                // usage strings) — surface them verbatim.
                self.set_note_on(vid, format!("{error:#}"), StatusNoteKind::Error);
                return;
            }
            Some(Ok(Some(output))) => output,
        };
        if let Some(message) = output.message {
            self.set_note_on(vid, message, StatusNoteKind::Ok);
        }
        for action in output.actions {
            self.apply_mux_ui_action(vid, action).await;
        }
    }

    /// Interpret one declarative [`phi_ext::UiAction`] returned by a
    /// Mux-context command handler or an overlay's `Complete` outcome.
    pub(crate) async fn apply_mux_ui_action(&mut self, vid: u64, action: phi_ext::UiAction) {
        use phi_ext::UiAction;
        match action {
            UiAction::Quit => self.quit = true,
            UiAction::OpenProject(path) => self.open_project_path(vid, path),
            UiAction::OpenOverlay { name, payload } => match name.as_str() {
                phi_ext::OVERLAY_HELP => self.toggle_help_overlay(vid),
                phi_ext::OVERLAY_USAGE => self.open_usage_overlay(vid),
                _ => {
                    let payload = payload.map(|value| Box::new(value) as phi_ext::OverlayPayload);
                    if !self.open_extension_overlay(vid, &name, payload) {
                        self.set_note_on(
                            vid,
                            format!("overlay {name} not available"),
                            StatusNoteKind::Error,
                        );
                    }
                }
            },
            UiAction::RunHostAction { name } => match name.as_str() {
                "cleanup" => match phi_session::archive::cleanup_archive(ARCHIVE_MAX_AGE) {
                    Ok(count) => {
                        self.set_note_on(
                            vid,
                            format!("{count} archives removed"),
                            StatusNoteKind::Ok,
                        );
                    }
                    Err(error) => {
                        self.set_note_on(vid, format!("cleanup: {error:#}"), StatusNoteKind::Error);
                    }
                },
                "refresh" => self.run_action(vid, AppAction::RefreshSession).await,
                "reload" => self.run_action(vid, AppAction::RefreshAllSessions).await,
                "session-prev" => self.run_action(vid, AppAction::PreviousSession).await,
                "session-next" => self.run_action(vid, AppAction::NextSession).await,
                "project-prev" => self.run_action(vid, AppAction::PreviousProject).await,
                "project-next" => self.run_action(vid, AppAction::NextProject).await,
                _ => {
                    self.set_note_on(
                        vid,
                        format!("unknown host action: {name}"),
                        StatusNoteKind::Error,
                    );
                }
            },
            UiAction::SwitchStyle { name } => self.switch_style(vid, name.trim()),
            UiAction::NewSession
            | UiAction::OpenPicker(_)
            | UiAction::SwitchModel { .. }
            | UiAction::StartLogin { .. }
            | UiAction::Logout { .. }
            | UiAction::TrustProject { .. } => {
                self.set_note_on(
                    vid,
                    "command action not supported in mux",
                    StatusNoteKind::Error,
                );
            }
        }
    }

    /// The `:style` effect (WS10): activate a style pack on the mux's
    /// visual runtime, persist the choice, and re-derive the chrome
    /// palette. Sessions own their own runtimes — a running session
    /// re-skins via its `/style`, or on its next build. An empty name
    /// reports the available styles.
    fn switch_style(&mut self, vid: u64, name: &str) {
        let Some(rt) = self.visual_runtime.as_deref() else {
            self.set_note_on(vid, "no styles registered", StatusNoteKind::Error);
            return;
        };
        if name.is_empty() {
            let active = rt.active_style();
            let styles: Vec<String> = rt
                .list_styles()
                .into_iter()
                .map(|s| {
                    if active.as_deref() == Some(s.as_str()) {
                        format!("[{s}]")
                    } else {
                        s
                    }
                })
                .collect();
            let listing = if styles.is_empty() {
                "no styles registered".to_owned()
            } else {
                format!("styles: {}", styles.join(" "))
            };
            self.set_note_on(vid, listing, StatusNoteKind::Ok);
            return;
        }
        if let Err(e) = rt.set_active_style(Some(name)) {
            self.set_note_on(vid, format!("{e:#}"), StatusNoteKind::Error);
            return;
        }
        if let Err(e) = phi_session::style::StyleStore::load_default().set(Some(name)) {
            self.set_note_on(
                vid,
                format!("style not persisted: {e:#}"),
                StatusNoteKind::Error,
            );
        }
        self.palette =
            super::resolve_palette(self.visual_runtime.as_deref(), &self.terminal_colors);
        // Committed frames have the old palette baked in: force every view
        // to repaint from scratch (the alt-screen-transition mechanics).
        for view in self.views.values_mut() {
            view.surface.invalidate();
            view.renderer.invalidate();
        }
        self.set_note_on(vid, format!("style: {name}"), StatusNoteKind::Ok);
    }

    /// The `:open` effect: add `path` as a project and focus it. In daemon
    /// mode a relative path would resolve against the daemon's own cwd;
    /// resolve against the typing client's cwd (carried in its Attach
    /// handshake) instead.
    fn open_project_path(&mut self, vid: u64, path: PathBuf) {
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

    /// Open an overlay by name on behalf of an embedded session. Well-known
    /// names get their native payloads attached here ([`phi_ext::OVERLAY_MODEL`]
    /// gets the model list and the responder wired to the session's reply
    /// channel); everything else routes straight through the
    /// open-overlay-by-name path.
    pub(crate) fn handle_host_modal_request(&mut self, key: SessionKey, request: HostModalRequest) {
        match request.name.as_str() {
            phi_ext::OVERLAY_MODEL => {
                let root = request
                    .payload
                    .as_ref()
                    .and_then(|p| p.get("root"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                let current = request
                    .payload
                    .as_ref()
                    .and_then(|p| p.get("current"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
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
                // The shared registry is the base builtin one; sessions
                // extend it per-build with catalog/extension providers, so
                // chain the bundled catalog here the same way `/model`
                // completion does — otherwise catalog providers (neuralwatt,
                // vercel, ...) never appear in the mux picker.
                #[cfg(feature = "builtins")]
                let catalog_names = phi_builtins::catalog::model_names();
                #[cfg(not(feature = "builtins"))]
                let catalog_names = std::iter::empty::<String>();
                let models = self
                    .shared
                    .registry
                    .supported_models()
                    .map(|m| format!("{}/{}", m.provider, m.id))
                    .chain(catalog_names)
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
            name => {
                // No reply payload: open on every view focused on the
                // requesting session, forwarding the JSON payload (if any)
                // to the overlay's `init_state`.
                let viewers = self.views_focused_on(key);
                if viewers.is_empty() {
                    let _ = request
                        .response_tx
                        .send(Err("session is not focused by any client".to_owned()));
                    return;
                }
                let name = name.to_owned();
                let mut opened = false;
                for vid in viewers {
                    let payload = request
                        .payload
                        .clone()
                        .map(|value| Box::new(value) as phi_ext::OverlayPayload);
                    opened |= self.open_extension_overlay(vid, &name, payload);
                }
                let _ = request.response_tx.send(if opened {
                    Ok(None)
                } else {
                    Err(format!("overlay {name} not available"))
                });
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

/// Build the help overlay's content from the live keymap and the registered
/// Mux-context commands. The content is host data (mux commands + current
/// keybindings); the overlay itself is registered by `phi-builtins` and
/// renders whatever entries it's handed.
fn help_entries(
    keymap: &Keymap<AppAction>,
    commands: &[phi_ext::CommandInfo],
    shortcuts: &[(String, String)],
) -> Vec<phi_ext::HelpEntry> {
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
    ];

    if !commands.is_empty() {
        entries.extend([HelpEntry::Blank, HelpEntry::Heading("Commands".to_owned())]);
        for cmd in commands {
            let keys = if cmd.args_hint.is_empty() {
                format!(":{}", cmd.name)
            } else {
                format!(":{} {}", cmd.name, cmd.args_hint)
            };
            entries.push(item(&keys, &cmd.description));
        }
    }

    entries.extend([
        HelpEntry::Blank,
        HelpEntry::Heading("Navigation & actions".to_owned()),
    ]);

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
    // Extension-registered shortcuts (WS6.3): surface `list_shortcuts`
    // so registered shortcuts appear in the help overlay rather than
    // being silently dropped — the API no longer falsely signals coverage
    // with zero consumers.
    if !shortcuts.is_empty() {
        entries.push(HelpEntry::Blank);
        entries.push(HelpEntry::Heading("Extension shortcuts".to_owned()));
        for (key, desc) in shortcuts {
            entries.push(item(key, desc));
        }
    }
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
        AppAction::OpenCommandLine => "open the command line",
    }
}
