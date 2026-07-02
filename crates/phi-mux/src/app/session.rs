use super::*;

/// Minimum interval between non-user-initiated renders (background redraw
/// nudges, status updates, spinner ticks). Mirrors pi's
/// `MIN_RENDER_INTERVAL_MS = 16` — bursts of background events collapse into
/// one draw per 16ms, preventing double rendering from desynchronized clocks.
const MIN_RENDER_INTERVAL: Duration = Duration::from_millis(16);

/// Duration to sleep when the animation clock is paused (no keep-alive
/// interests). This is a fallback wake-up so the event loop can still
/// process background events (status updates, watcher notifications)
/// even when no animations are running. Using a longer interval here
/// reduces idle CPU while keeping responsiveness.
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(1);

impl MuxApp {
    pub(crate) async fn main_loop(
        &mut self,
        host: HostIo<'_>,
        mut input: InputSource,
        receivers: MuxReceivers,
    ) -> Result<()> {
        let MuxReceivers {
            mut status_rx,
            mut redraw_rx,
            mut suspend_rx,
            mut host_modal_rx,
            mut watcher_rx,
        } = receivers;

        self.ensure_all_sessions().await;
        self.draw();
        let mut last_render_at = tokio::time::Instant::now();
        let mut pending_render = false;

        #[allow(unused_assignments)]
        while !self.quit {
            let mut dirty = false;
            // Principle: whatever the user is looking at must not change
            // unless the user explicitly requests it (key, mouse, command).
            // `focus_changed` is only set by *user* input, never by background
            // events (watcher, status, redraw, ticker). Background session
            // updates must refresh the sidebar/screen without touching focus.
            // `refresh_project_with_focus` likewise preserves the selected
            // session by key and falls back to the original index (not 0) if
            // the session was removed, so external file changes never steal
            // focus.
            let mut focus_changed = false;
            tokio::select! {
                event = input.next() => {
                    match event {
                        Some(event) => {
                            self.handle_remote_event(event).await;
                            dirty = true;
                            focus_changed = true;
                        }
                        None => break,
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
                    self.handle_suspend(&host, request);
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
                        let delta = workspace::refresh_project_with_focus(
                            &mut self.workspace,
                            &mut self.view.focus,
                            index,
                        );
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
                // Clock-driven timer: when animations are active (keep-alive > 0),
                // wake at the clock's tick interval (16ms focused, 32ms blurred).
                // When paused (no animations), wake at a longer idle interval so
                // background events still get processed without busy-looping.
                // This replaces the old unconditional `ticker.tick()` — the
                // clock is the single timer, and it only fires when something
                // needs animating.
                _ = async {
                    if self.clock.is_ticking() {
                        tokio::time::sleep_until(
                            tokio::time::Instant::now()
                                + Duration::from_millis(self.clock.tick_interval_ms()),
                        ).await
                    } else {
                        tokio::time::sleep(IDLE_POLL_INTERVAL).await
                    }
                } => {
                    if self.clock.is_ticking() {
                        self.clock.tick();
                    }
                    if let Some((_, _, at)) = &self.view.note
                        && at.elapsed() > NOTE_TTL {
                            self.view.note = None;
                            dirty = true;
                        }
                    // Redraw on tick so animated surfaces (statusbar shimmer,
                    // sidebar spinner, gradient text) stay in motion. The clock
                    // only ticks when at least one keep-alive interest is active.
                    if self.clock.is_ticking() {
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
                        let delta = workspace::refresh_project_with_focus(
                            &mut self.workspace,
                            &mut self.view.focus,
                            index,
                        );
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
                // User interactions (focus_changed) draw immediately for
                // instant feedback; background events (redraws, status,
                // spinner ticks) set pending_render so bursts collapse into
                // one draw at the next MIN_RENDER_INTERVAL boundary,
                // mirroring pi's requestRender / scheduleRender debounce.
                if focus_changed {
                    self.draw();
                    last_render_at = tokio::time::Instant::now();
                    pending_render = false;
                } else {
                    pending_render = true;
                }
            }

            // If a background render is pending and the minimum interval has
            // elapsed, draw now. Otherwise the next iteration of the select!
            // includes a render-timer arm (below) that fires when ready.
            if pending_render && last_render_at.elapsed() >= MIN_RENDER_INTERVAL {
                self.draw();
                last_render_at = tokio::time::Instant::now();
                pending_render = false;
            }

            // Render-timer arm: when a background render is pending but the
            // minimum interval hasn't elapsed, wait for the timer while still
            // processing events. This is the pi `scheduleRender` pattern —
            // events are never blocked by the debounce.
            if pending_render {
                let remaining = MIN_RENDER_INTERVAL.saturating_sub(last_render_at.elapsed());
                tokio::select! {
                    biased;
                    _ = tokio::time::sleep(remaining) => {
                        if pending_render {
                            self.draw();
                            last_render_at = tokio::time::Instant::now();
                            pending_render = false;
                        }
                    }
                    event = input.next() => {
                        match event {
                            Some(event) => {
                                self.handle_remote_event(event).await;
                                self.draw();
                                last_render_at = tokio::time::Instant::now();
                                pending_render = false;
                            }
                            None => break,
                        }
                    }
                    Some(()) = redraw_rx.recv() => {
                        pending_render = true;
                    }
                    Some((key, status)) = status_rx.recv() => {
                        self.apply_status(key, status);
                        pending_render = true;
                    }
                }
            }
        }
        Ok(())
    }

    /// Dispatch one event from the input source. Local terminal events
    /// arrive as `Input` from synthetic client 0; the daemon additionally
    /// sees attach/detach/resize and round-trip replies.
    pub(crate) async fn handle_remote_event(&mut self, event: RemoteEvent) {
        match event {
            RemoteEvent::Input { id, event } => {
                self.last_active_client = id;
                self.handle_event(event).await;
            }
            RemoteEvent::Attached { .. } => {
                self.apply_shared_size();
                // The newcomer starts from a cleared screen: force a full
                // repaint so its first frame is self-contained.
                self.view.renderer.invalidate();
            }
            RemoteEvent::Detached { id } => {
                // Fail any external-editor round-trip parked on this client
                // so the requesting session unblocks with an error.
                let mut i = 0;
                while i < self.pending_edits.len() {
                    if self.pending_edits[i].0 == id {
                        let (_, tx) = self.pending_edits.remove(i);
                        let _ = tx.send(Err("client detached during edit".to_owned()));
                    } else {
                        i += 1;
                    }
                }
                self.apply_shared_size();
            }
            RemoteEvent::Resized => self.apply_shared_size(),
            RemoteEvent::EditResult { id, result } => {
                if let Some(pos) = self.pending_edits.iter().position(|(cid, _)| *cid == id) {
                    let (_, tx) = self.pending_edits.remove(pos);
                    let _ = tx.send(result);
                }
                // The editing client's screen was overwritten by $EDITOR;
                // force a full repaint so it resynchronizes.
                self.view.renderer.invalidate();
            }
            RemoteEvent::ClipboardText { text, .. } => match text {
                Some(text) if !text.is_empty() => self.forward_to_session(Event::Paste(text)),
                _ => self.set_note("clipboard empty", StatusNoteKind::Error),
            },
            RemoteEvent::Kill => self.quit = true,
        }
    }

    /// Adopt the registry's shared canvas size (min across attached
    /// clients). No-op in local mode or with no clients (the last size is
    /// kept so background sessions keep rendering at sane dimensions).
    fn apply_shared_size(&mut self) {
        let Some(registry) = self.registry().cloned() else {
            return;
        };
        let Some((cols, rows)) = registry.target_size() else {
            return;
        };
        if (cols, rows) != self.view.size {
            self.handle_resize(cols, rows);
            if registry.client_count() > 1 {
                self.set_note(
                    format!("canvas {cols}x{rows} (smallest attached client)"),
                    StatusNoteKind::Ok,
                );
            }
        }
    }

    pub(crate) fn layout(&self) -> CellLayout {
        compute_cell_layout(self.view.size.0, self.view.size.1, self.sidebar_width)
    }

    pub(crate) fn pane_size(&self) -> (u16, u16) {
        let terminal = self.layout().terminal;
        (terminal.cols.max(1) as u16, terminal.rows.max(1) as u16)
    }

    pub(crate) fn set_note(&mut self, text: impl Into<String>, kind: StatusNoteKind) {
        self.view.note = Some((text.into(), kind, Instant::now()));
    }

    pub(crate) fn overlay_active(&self) -> bool {
        self.view.archive_viewer.is_some() || self.view.active_extension_overlay.is_some()
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
                .get(self.view.focus.project)
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
            let _ = workspace::refresh_project_with_focus(
                &mut self.workspace,
                &mut self.view.focus,
                project_index,
            );
        }
    }

    /// Spawn every session across all projects in parallel, so all are
    /// warm immediately at startup. Sessions created later are still
    /// loaded lazily by [`ensure_focused_session`] on focus.
    pub(crate) async fn ensure_all_sessions(&mut self) {
        let pane = self.pane_size();

        // Ensure every project has at least one (possibly draft) session.
        for i in 0..self.workspace.projects.len() {
            self.workspace.ensure_project_session(i);
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
            driver::spawn(
                *key,
                cwd,
                resume.as_deref(),
                pane,
                shared,
                channels,
                self.terminal_colors.clone(),
            )
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
        let project_index = self.view.focus.project;
        self.workspace.ensure_project_session(project_index);
        self.view
            .focus
            .clamp_session(&self.workspace, project_index);
        let Some(project) = self.workspace.projects.get(project_index) else {
            return;
        };
        let cwd = project.cwd.clone();
        let Some(session) = project
            .sessions
            .get(self.view.focus.selected_session(project_index))
        else {
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
            self.terminal_colors.clone(),
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
        let project = self.workspace.projects.get(self.view.focus.project)?;
        project
            .sessions
            .get(self.view.focus.selected_session(self.view.focus.project))
    }

    pub(crate) fn focused_session_mut(&mut self) -> Option<&mut workspace::MuxSession> {
        let selected = self.view.focus.selected_session(self.view.focus.project);
        let project = self.workspace.projects.get_mut(self.view.focus.project)?;
        project.sessions.get_mut(selected)
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
        self.view.selection.clear();
        if let Some(handle) = self.focused_session().and_then(|s| s.handle.as_ref()) {
            driver::lock_parser(&handle.parser)
                .screen_mut()
                .set_scrollback(0);
        }
        self.view.cached_scrollback_total = 0;
        // The new focused session's content is unrelated to the previous
        // frame's row hashes; drop them so scroll detection can't mistake the
        // session switch for a scroll, and force a clean repaint.
        self.view.renderer.invalidate();
        self.view.surface.invalidate();
        self.view.alt_screen_active = false;
        self.persist_now();
    }
}
