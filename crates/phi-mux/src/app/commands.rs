use super::*;

impl MuxApp {
    pub(crate) async fn run_action(&mut self, action: AppAction) {
        match action {
            AppAction::PreviousProject | AppAction::NextProject => {
                let count = self.workspace.projects.len();
                if count > 0 {
                    let delta: i32 = if action == AppAction::NextProject {
                        1
                    } else {
                        -1
                    };
                    let current = self.workspace.selected_project as i32;
                    self.workspace.selected_project =
                        (current + delta).rem_euclid(count as i32) as usize;
                    self.on_focus_changed();
                    self.ensure_focused_session().await;
                }
            }
            AppAction::PreviousSession | AppAction::NextSession => {
                let project_index = self.workspace.selected_project;
                let delta: i32 = if action == AppAction::NextSession {
                    1
                } else {
                    -1
                };
                if self
                    .workspace
                    .move_selected_visible_session(project_index, delta)
                {
                    self.on_focus_changed();
                    self.ensure_focused_session().await;
                }
            }
            AppAction::NewSession => {
                let project_index = self.workspace.selected_project;
                self.workspace.new_draft(project_index);
                self.on_focus_changed();
                self.ensure_focused_session().await;
            }
            AppAction::RefreshSession => self.refresh_focused_session(),
            AppAction::RefreshAllSessions => {
                for index in 0..self.workspace.projects.len() {
                    let _ = self.workspace.refresh_project(index);
                }
                self.set_note("rescanned all projects", StatusNoteKind::Ok);
            }
            AppAction::ArchiveSession => self.archive_focused_session().await,
            AppAction::RemoveProject => {
                let index = self.workspace.selected_project;
                if let Some(project) = self.workspace.projects.get(index) {
                    let title = project_sidebar_title(project);
                    self.workspace.remove_project(index);
                    self.sync_watchers();
                    self.set_note(format!("removed {title}"), StatusNoteKind::Ok);
                    self.on_focus_changed();
                    self.ensure_focused_session().await;
                }
            }
            AppAction::CopySelection => self.copy_selection(true),
            AppAction::PasteClipboard => {
                if let Some(text) = self.clipboard.read_text() {
                    self.forward_to_session(Event::Paste(text));
                } else {
                    self.set_note("clipboard empty", StatusNoteKind::Error);
                }
            }
            AppAction::Help => {
                if self.help_overlay.is_some() {
                    self.help_overlay = None;
                } else {
                    self.help_overlay = Some(HelpOverlayState::default());
                }
            }
            AppAction::Quit => self.quit = true,
        }
    }

    /// Rescan the focused project; a crashed/exited focused session is
    /// restarted in place against the same JSONL.
    pub(crate) fn refresh_focused_session(&mut self) {
        let restart = self
            .focused_session()
            .is_some_and(|s| matches!(s.status, SessionStatus::Crashed | SessionStatus::Done));
        if restart {
            if let Some(session) = self.focused_session_mut() {
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
            self.set_note("session restarted", StatusNoteKind::Ok);
        } else {
            let index = self.workspace.selected_project;
            let _ = self.workspace.refresh_project(index);
            self.set_note("project rescanned", StatusNoteKind::Ok);
        }
    }

    pub(crate) async fn archive_focused_session(&mut self) {
        let project_index = self.workspace.selected_project;
        if self.workspace.discard_unmaterialized_draft(project_index) {
            self.workspace.new_draft(project_index);
            self.set_note("draft deleted", StatusNoteKind::Ok);
            self.on_focus_changed();
            return;
        }

        let Some(path) = self.focused_session().and_then(|s| {
            s.store_path.clone().or_else(|| {
                s.handle
                    .as_ref()
                    .map(|h| h.store_path.clone())
                    .filter(|p| p.exists())
            })
        }) else {
            self.set_note("draft has no file to archive", StatusNoteKind::Error);
            return;
        };
        match workspace::archive_session(&path) {
            Ok(_) => {
                self.selection.clear();
                if let Some(session) = self.focused_session_mut() {
                    session.handle = None;
                    session.store_path = None;
                }
                let index = self.workspace.selected_project;
                let _ = self.workspace.refresh_project(index);
                self.workspace.normalize_visible_selection(index);
                crate::driver::drain_pending_terminal_events();
                self.ensure_focused_session().await;
                self.set_note("session archived", StatusNoteKind::Ok);
                self.on_focus_changed();
            }
            Err(_error) => {
                // Archive I/O is copy+delete fallback — an error here means
                // the session file is still safely in place. Stay silent.
            }
        }
    }

    pub(crate) fn restore_archived(&mut self, archived: &std::path::Path) {
        match workspace::restore_session(archived) {
            Ok(target) => {
                self.archive_viewer = None;
                for index in 0..self.workspace.projects.len() {
                    let _ = self.workspace.refresh_project(index);
                }
                self.set_note(format!("restored {}", target.display()), StatusNoteKind::Ok);
            }
            Err(_error) => {
                // Restore I/O is copy+delete fallback — the archived file is
                // still safely in place. Reload the viewer silently.
                if let Some(viewer) = &mut self.archive_viewer {
                    viewer.reload_sessions();
                }
            }
        }
    }

    pub(crate) fn copy_selection(&mut self, note_on_empty: bool) {
        let Some(range) = self.selection.normalized() else {
            if note_on_empty {
                self.set_note("nothing selected", StatusNoteKind::Error);
            }
            return;
        };
        let Some(handle) = self.focused_session().and_then(|s| s.handle.as_ref()) else {
            return;
        };
        let text = {
            let parser = driver::lock_parser(&handle.parser);
            selected_text(parser.screen(), range)
        };
        if text.trim().is_empty() {
            return;
        }
        if let Err(note) = self.clipboard.copy_text(&text) {
            self.set_note(note, StatusNoteKind::Error);
        }
    }

    pub(crate) async fn run_command_input(&mut self, input: &str) {
        let input = input.trim();
        if input.is_empty() {
            return;
        }
        match parse_command(input) {
            Ok(MuxCommand::Open(path)) => {
                if path.is_dir() {
                    let path = PathBuf::from(persisted::normalize_path(&path));
                    let index = self.workspace.add_project(path);
                    self.workspace.selected_project = index;
                    let _ = self.workspace.ensure_project_session(index);
                    self.sync_watchers();
                    self.on_focus_changed();
                } else {
                    self.set_note(
                        format!("not a directory: {}", path.display()),
                        StatusNoteKind::Error,
                    );
                }
            }
            Ok(MuxCommand::Archive) => self.archive_viewer = Some(ArchiveViewerState::load()),
            Ok(MuxCommand::Cleanup) => match workspace::cleanup_archive(ARCHIVE_MAX_AGE) {
                Ok(count) => {
                    self.set_note(format!("{count} archives removed"), StatusNoteKind::Ok);
                }
                Err(error) => self.set_note(format!("cleanup: {error:#}"), StatusNoteKind::Error),
            },
            Ok(MuxCommand::Usage) => self.usage_overlay = Some(UsageOverlayState::load()),
            Ok(MuxCommand::Refresh) => self.run_action(AppAction::RefreshSession).await,
            Ok(MuxCommand::Reload) => self.run_action(AppAction::RefreshAllSessions).await,
            Ok(MuxCommand::Quit) => self.quit = true,
            Ok(MuxCommand::Action(action)) => self.run_action(action).await,
            Ok(MuxCommand::Help) => self.help_overlay = Some(HelpOverlayState::default()),
            Err(note) => self.set_note(note, StatusNoteKind::Error),
        }
    }

    pub(crate) fn handle_host_modal_request(&mut self, key: SessionKey, request: HostModalRequest) {
        match request.kind {
            HostModalKind::Usage => {
                self.usage_overlay = Some(UsageOverlayState::load());
                let _ = request.response_tx.send(Ok(None));
            }
            HostModalKind::Model { root, current } => {
                if self
                    .focused_session()
                    .is_none_or(|session| session.key != key)
                {
                    let _ = request.response_tx.send(Err(
                        "session lost focus before opening model picker".to_owned(),
                    ));
                    return;
                }
                if let Some(mut existing) = self.model_overlay.take()
                    && let Some(tx) = existing.response_tx.take()
                {
                    let _ = tx.send(Ok(None));
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
                self.model_overlay = Some(ModelOverlayState::new(
                    key,
                    root,
                    current,
                    models,
                    request.response_tx,
                ));
            }
        }
    }

    pub(crate) fn finish_model_overlay(&mut self, selected: Option<String>) {
        if let Some(mut model) = self.model_overlay.take()
            && let Some(tx) = model.response_tx.take()
        {
            let _ = tx.send(Ok(selected));
        }
    }

    pub(crate) fn handle_suspend(&mut self, guard: &RawModeGuard, request: tui::SuspendRequest) {
        let mut stdout = std::io::stdout();
        let _ = phi_tui::crossterm::execute!(stdout, DisableMouseCapture);
        let result = guard.with_suspended(|| tui::external_editor(&request.text));
        let _ = phi_tui::crossterm::execute!(stdout, EnableMouseCapture);
        let _ = request.response_tx.send(result);
        self.renderer.invalidate();
    }
}
