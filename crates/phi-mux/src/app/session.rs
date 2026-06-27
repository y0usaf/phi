use super::*;

impl MuxApp {
    pub(crate) async fn main_loop(
        &mut self,
        guard: &RawModeGuard,
        mut status_rx: mpsc::UnboundedReceiver<(SessionKey, SessionStatus)>,
        mut redraw_rx: mpsc::UnboundedReceiver<()>,
        mut suspend_rx: mpsc::UnboundedReceiver<(SessionKey, tui::SuspendRequest)>,
        mut host_modal_rx: mpsc::UnboundedReceiver<(SessionKey, HostModalRequest)>,
        mut watcher_rx: mpsc::UnboundedReceiver<usize>,
    ) -> Result<()> {
        use futures::StreamExt as _;
        let mut events = EventStream::new();
        let mut ticker = tokio::time::interval(ANIMATION_TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        self.ensure_all_sessions().await;
        self.draw();

        while !self.quit {
            let mut dirty = false;
            // Principle: whatever the user is looking at must not change
            // unless the user explicitly requests it (key, mouse, command).
            // `focus_changed` is only set by *user* input, never by background
            // events (watcher, status, redraw, ticker). Background session
            // updates must refresh the sidebar/screen without touching focus.
            // `refresh_project` likewise preserves the selected session by
            // key and falls back to the original index (not 0) if the session
            // was removed, so external file changes never steal focus.
            let mut focus_changed = false;
            tokio::select! {
                event = events.next() => {
                    match event {
                        Some(Ok(event)) => {
                            self.handle_event(event).await;
                            dirty = true;
                            focus_changed = true;
                        }
                        Some(Err(_)) | None => break,
                    }
                }
                Some((key, status)) = status_rx.recv() => {
                    self.apply_status(key, status);
                    dirty = true;
                }
                Some(()) = redraw_rx.recv() => {
                    dirty = true;
                }
                Some((_key, request)) = suspend_rx.recv() => {
                    self.handle_suspend(guard, request);
                    dirty = true;
                }
                Some((key, request)) = host_modal_rx.recv() => {
                    self.handle_host_modal_request(key, request);
                    dirty = true;
                }
                Some(project_index) = watcher_rx.recv() => {
                    // Coalesce all pending watcher events: collect the set
                    // of project indices that changed, then refresh each.
                    // This naturally debounces bursts of writes.
                    let mut changed = std::collections::HashSet::new();
                    changed.insert(project_index);
                    while let Ok(idx) = watcher_rx.try_recv() {
                        changed.insert(idx);
                    }
                    let mut notes = Vec::new();
                    for index in changed {
                        let delta = self.workspace.refresh_project(index);
                        notes.extend(delta.archived);
                    }
                    if !notes.is_empty() {
                        self.set_note(
                            format!("session archived externally: {}", notes.join(", ")),
                            StatusNoteKind::Ok,
                        );
                    }
                    dirty = true;
                }
                _ = ticker.tick() => {
                    if let Some((_, _, at)) = &self.note
                        && at.elapsed() > NOTE_TTL {
                            self.note = None;
                            dirty = true;
                        }
                    if sidebar_has_spinner(&self.workspace.projects) {
                        dirty = true;
                    }
                }
            }

            // Coalesce queued redraw nudges into this frame.
            while redraw_rx.try_recv().is_ok() {
                dirty = true;
            }
            while let Ok((key, status)) = status_rx.try_recv() {
                self.apply_status(key, status);
                dirty = true;
            }
            // Drain any queued watcher events and refresh affected projects.
            {
                let mut changed = std::collections::HashSet::new();
                while let Ok(idx) = watcher_rx.try_recv() {
                    changed.insert(idx);
                }
                if !changed.is_empty() {
                    let mut notes = Vec::new();
                    for index in changed {
                        let delta = self.workspace.refresh_project(index);
                        notes.extend(delta.archived);
                    }
                    if !notes.is_empty() {
                        self.set_note(
                            format!("session archived externally: {}", notes.join(", ")),
                            StatusNoteKind::Ok,
                        );
                    }
                    dirty = true;
                }
            }

            if dirty {
                if focus_changed {
                    self.ensure_focused_session().await;
                }
                self.draw();
            }
        }
        Ok(())
    }

    pub(crate) fn layout(&self) -> CellLayout {
        compute_cell_layout(self.size.0, self.size.1, self.sidebar_width)
    }

    pub(crate) fn pane_size(&self) -> (u16, u16) {
        let terminal = self.layout().terminal;
        (terminal.cols.max(1) as u16, terminal.rows.max(1) as u16)
    }

    pub(crate) fn set_note(&mut self, text: impl Into<String>, kind: StatusNoteKind) {
        self.note = Some((text.into(), kind, Instant::now()));
    }

    pub(crate) fn overlay_active(&self) -> bool {
        self.help_overlay.is_some()
            || self.usage_overlay.is_some()
            || self.model_overlay.is_some()
            || self.archive_viewer.is_some()
    }

    /// Sync filesystem watchers to match the current workspace projects.
    /// Adds watchers for new projects, removes watchers for dropped ones.
    pub(crate) fn sync_watchers(&mut self) {
        let projects: Vec<(usize, &PathBuf)> = self
            .workspace
            .projects
            .iter()
            .enumerate()
            .map(|(i, p)| (i, &p.cwd))
            .collect();
        self.session_watcher.sync(&projects);
    }

    pub(crate) fn persist_now(&self) {
        let state = PersistedState {
            projects: self
                .workspace
                .projects
                .iter()
                .map(|p| p.cwd.display().to_string())
                .collect(),
            selected_project: self
                .workspace
                .projects
                .get(self.workspace.selected_project)
                .map(|p| p.cwd.display().to_string()),
        };
        state.enqueue_save();
    }

    pub(crate) fn apply_status(&mut self, key: SessionKey, status: SessionStatus) {
        let mut materialized_project = None;
        for (project_index, project) in self.workspace.projects.iter_mut().enumerate() {
            if let Some(session) = project.sessions.iter_mut().find(|s| s.key == key) {
                if session.is_draft() && session.handle.is_some() {
                    materialized_project = Some(project_index);
                }
                session.status = status;
                break;
            }
        }
        // A draft's JSONL appears with its first message; rescan to pick the
        // file up (the running handle keeps its identity).
        if let Some(project_index) = materialized_project {
            let _ = self.workspace.refresh_project(project_index);
        }
    }

    /// Spawn every session across all projects in parallel, so all are
    /// warm immediately at startup. Sessions created later are still
    /// loaded lazily by [`ensure_focused_session`] on focus.
    pub(crate) async fn ensure_all_sessions(&mut self) {
        let pane = self.pane_size();

        // Ensure every project has at least one (possibly draft) session.
        for i in 0..self.workspace.projects.len() {
            let _ = self.workspace.ensure_project_session(i);
        }

        // Collect sessions that still need an embedded handle.
        let mut specs = Vec::new();
        for project in &self.workspace.projects {
            for session in &project.sessions {
                if session.handle.is_none() {
                    specs.push((session.key, project.cwd.clone(), session.store_path.clone()));
                }
            }
        }

        if specs.is_empty() {
            return;
        }

        // Spawn all concurrently -- each `driver::spawn` launches an
        // independent tokio task; `join_all` just awaits them in parallel.
        let shared = &self.shared;
        let channels = &self.channels;
        let futures = specs.iter().map(|(key, cwd, resume)| {
            driver::spawn(*key, cwd, resume.as_deref(), pane, shared, channels)
        });
        let results = futures::future::join_all(futures).await;

        // Assign handles (or crash status) back to the workspace.
        let mut errors = Vec::new();
        for ((key, _, _), result) in specs.into_iter().zip(results) {
            let session = self
                .workspace
                .projects
                .iter_mut()
                .find_map(|p| p.sessions.iter_mut().find(|s| s.key == key));
            if let Some(session) = session {
                match result {
                    Ok(handle) => {
                        session.handle = Some(handle);
                        session.status = SessionStatus::Idle;
                    }
                    Err(error) => {
                        session.status = SessionStatus::Crashed;
                        errors.push(format!("session failed: {error:#}"));
                    }
                }
            }
        }

        for msg in errors {
            self.set_note(msg, StatusNoteKind::Error);
        }
    }

    /// Spawn the focused session's embedded loop if it isn't running yet.
    pub(crate) async fn ensure_focused_session(&mut self) {
        let pane = self.pane_size();
        let project_index = self.workspace.selected_project;
        let _ = self.workspace.ensure_project_session(project_index);
        let Some(project) = self.workspace.projects.get(project_index) else {
            return;
        };
        let cwd = project.cwd.clone();
        let Some(session) = project.sessions.get(project.selected) else {
            return;
        };
        if session.handle.is_some() {
            return;
        }
        let key = session.key;
        let resume = session.store_path.clone();
        let spawned = driver::spawn(
            key,
            &cwd,
            resume.as_deref(),
            pane,
            &self.shared,
            &self.channels,
        )
        .await;
        let Some(project) = self.workspace.projects.get_mut(project_index) else {
            return;
        };
        let Some(session) = project.sessions.iter_mut().find(|s| s.key == key) else {
            return;
        };
        match spawned {
            Ok(handle) => {
                session.handle = Some(handle);
                session.status = SessionStatus::Idle;
            }
            Err(error) => {
                session.status = SessionStatus::Crashed;
                self.set_note(format!("session failed: {error:#}"), StatusNoteKind::Error);
            }
        }
    }

    pub(crate) fn focused_session(&self) -> Option<&workspace::MuxSession> {
        let project = self
            .workspace
            .projects
            .get(self.workspace.selected_project)?;
        project.sessions.get(project.selected)
    }

    pub(crate) fn focused_session_mut(&mut self) -> Option<&mut workspace::MuxSession> {
        let project = self
            .workspace
            .projects
            .get_mut(self.workspace.selected_project)?;
        project.sessions.get_mut(project.selected)
    }

    pub(crate) fn forward_to_session(&mut self, event: Event) {
        if let Some(handle) = self.focused_session().and_then(|s| s.handle.as_ref()) {
            // Snap the terminal back to the bottom on any key input, matching
            // pi where typing in a scrolled-up terminal jumps to the cursor.
            if matches!(event, Event::Key(_)) {
                driver::lock_parser(&handle.parser)
                    .screen_mut()
                    .set_scrollback(0);
            }
            let _ = handle.input_tx.send(event);
        }
    }

    pub(crate) fn on_focus_changed(&mut self) {
        self.selection.clear();
        if let Some(handle) = self.focused_session().and_then(|s| s.handle.as_ref()) {
            driver::lock_parser(&handle.parser)
                .screen_mut()
                .set_scrollback(0);
        }
        // The new focused session's content is unrelated to the previous
        // frame's row hashes; drop them so scroll detection can't mistake the
        // session switch for a scroll, and force a clean repaint.
        self.renderer.invalidate();
        self.surface.invalidate();
        self.alt_screen_active = false;
        self.persist_now();
    }
}
