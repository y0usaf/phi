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

/// Debounce for per-session pty resizes after focus/size changes: flipping
/// through the sidebar or drag-resizing a client coalesces into one resize
/// per session once the dust settles.
const SIZE_RECOMPUTE_DEBOUNCE: Duration = Duration::from_millis(150);

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
            // `refresh_project_all_views` likewise preserves every view's
            // selected session by key and falls back to the original index
            // (not 0) if the session was removed, so external file changes
            // never steal focus.
            let mut focus_changed = false;
            let recompute_deadline = self.pending_size_recompute;
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
                Some((key, request)) = suspend_rx.recv() => {
                    self.handle_suspend(&host, key, request);
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
                        let delta = self.refresh_project_all_views(index);
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
                // Debounced per-session size recompute: fires once the last
                // focus/size change is SIZE_RECOMPUTE_DEBOUNCE old.
                _ = async move {
                    match recompute_deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.recompute_session_sizes();
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
                    for view in self.views.values_mut() {
                        if let Some((_, _, at)) = &view.note
                            && at.elapsed() > NOTE_TTL {
                                view.note = None;
                                dirty = true;
                            }
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
                        let delta = self.refresh_project_all_views(index);
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
    /// sees attach/detach/resize and round-trip replies. Input is routed to
    /// the sending client's view.
    pub(crate) async fn handle_remote_event(&mut self, event: RemoteEvent) {
        match event {
            RemoteEvent::Input { id, event } => {
                if !self.views.contains_key(&id) {
                    return;
                }
                self.last_active_client = id;
                self.handle_event(id, event).await;
                self.ensure_focused_session(id).await;
            }
            RemoteEvent::Attached { id, info } => {
                // The newcomer starts looking at whatever the last active
                // view is looking at (tmux-attach feel); its fresh renderer
                // makes its first frame a self-contained full repaint.
                let focus = self
                    .views
                    .get(&self.last_active_client)
                    .or_else(|| self.views.values().next())
                    .map(|view| view.focus.clone())
                    .unwrap_or_default();
                self.views
                    .insert(id, MuxView::new(focus, (info.cols, info.rows)));
                self.schedule_size_recompute();
                self.ensure_focused_session(id).await;
            }
            RemoteEvent::Detached { id } => {
                self.views.remove(&id);
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
                self.schedule_size_recompute();
            }
            RemoteEvent::Resized { id, cols, rows } => self.handle_resize(id, cols, rows),
            RemoteEvent::EditResult { id, result } => {
                if let Some(pos) = self.pending_edits.iter().position(|(cid, _)| *cid == id) {
                    let (_, tx) = self.pending_edits.remove(pos);
                    let _ = tx.send(result);
                }
                // The editing client's screen was overwritten by $EDITOR;
                // force a full repaint so it resynchronizes.
                if let Some(view) = self.views.get_mut(&id) {
                    view.renderer.invalidate();
                }
            }
            RemoteEvent::ClipboardText { id, text } => match text {
                Some(text) if !text.is_empty() => {
                    self.forward_to_session(id, Event::Paste(text));
                }
                _ => self.set_note_on(id, "clipboard empty", StatusNoteKind::Error),
            },
            RemoteEvent::Kill => self.quit = true,
        }
    }

    pub(crate) fn layout_for(&self, size: (u16, u16)) -> CellLayout {
        compute_cell_layout(size.0, size.1, self.sidebar_width)
    }

    pub(crate) fn pane_size_for(&self, size: (u16, u16)) -> (u16, u16) {
        let terminal = self.layout_for(size).terminal;
        (terminal.cols.max(1) as u16, terminal.rows.max(1) as u16)
    }

    /// The pane size a session should render at: the smallest pane among
    /// the views currently focused on it (tmux grouped-session behavior —
    /// co-viewers get padding, never clipping; clients on different sessions
    /// get their full size). `None` when no view is looking at it, in which
    /// case the session keeps its last size.
    fn session_target_pane(&self, project: usize, session: usize) -> Option<(u16, u16)> {
        self.views
            .values()
            .filter(|view| {
                view.focus.project == project && view.focus.selected_session(project) == session
            })
            .map(|view| self.pane_size_for(view.size))
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1)))
    }

    pub(crate) fn schedule_size_recompute(&mut self) {
        self.pending_size_recompute = Some(tokio::time::Instant::now() + SIZE_RECOMPUTE_DEBOUNCE);
    }

    /// Resize every viewed session to the min pane among its viewers.
    /// Skips no-op resizes so co-viewers navigating around don't ping ptys
    /// that didn't change.
    pub(crate) fn recompute_session_sizes(&mut self) {
        self.pending_size_recompute = None;
        for project_index in 0..self.workspace.projects.len() {
            for session_index in 0..self.workspace.projects[project_index].sessions.len() {
                let Some((cols, rows)) = self.session_target_pane(project_index, session_index)
                else {
                    continue;
                };
                let session = &self.workspace.projects[project_index].sessions[session_index];
                let Some(handle) = session.handle.as_ref() else {
                    continue;
                };
                let (cur_rows, cur_cols) = driver::lock_parser(&handle.parser).screen().size();
                if (cur_cols, cur_rows) != (cols, rows) {
                    handle.resize(cols, rows);
                }
            }
        }
    }

    /// Post a transient status note on every view (background events:
    /// session crashes, external archives, spawn failures).
    pub(crate) fn set_note(&mut self, text: impl Into<String>, kind: StatusNoteKind) {
        let text = text.into();
        let now = Instant::now();
        for view in self.views.values_mut() {
            view.note = Some((text.clone(), kind, now));
        }
    }

    /// Post a transient status note on one client's view (feedback for that
    /// client's own action).
    pub(crate) fn set_note_on(&mut self, vid: u64, text: impl Into<String>, kind: StatusNoteKind) {
        if let Some(view) = self.views.get_mut(&vid) {
            view.note = Some((text.into(), kind, Instant::now()));
        }
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
        // With several clients attached, the last active one's selection is
        // what a future first view seeds from.
        let focus_project = self
            .views
            .get(&self.last_active_client)
            .or_else(|| self.views.values().next())
            .map(|view| view.focus.project);
        let state = PersistedState {
            projects: self
                .workspace
                .projects
                .iter()
                .map(|p| p.cwd.display().to_string())
                .collect(),
            selected_project: focus_project
                .and_then(|i| self.workspace.projects.get(i))
                .map(|p| p.cwd.display().to_string()),
        };
        state.enqueue_save();
    }

    /// Rescan a project while preserving every view's selection (mark by
    /// key → rebuild → restore per view).
    pub(crate) fn refresh_project_all_views(&mut self, index: usize) -> workspace::RefreshDelta {
        let mut focuses: Vec<&mut Focus> = self
            .views
            .values_mut()
            .map(|view| &mut view.focus)
            .collect();
        workspace::refresh_project_with_focuses(&mut self.workspace, &mut focuses, index)
    }

    /// Run a workspace mutation that may insert/remove sessions in
    /// `project`, re-resolving every view's selection by key afterwards so
    /// index shifts don't retarget other clients' focus.
    pub(crate) fn with_focus_preserved<R>(
        &mut self,
        project: usize,
        mutate: impl FnOnce(&mut Workspace) -> R,
    ) -> R {
        self.workspace.stabilize_keys(project);
        let marks: Vec<(u64, workspace::FocusMark)> = self
            .views
            .iter()
            .map(|(id, view)| (*id, view.focus.mark(&self.workspace, project)))
            .collect();
        let result = mutate(&mut self.workspace);
        for (id, mark) in marks {
            if let Some(view) = self.views.get_mut(&id) {
                view.focus.restore(&self.workspace, project, mark);
            }
        }
        result
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
            let _ = self.refresh_project_all_views(project_index);
        }
    }

    /// Spawn every session across all projects in parallel, so all are
    /// warm immediately at startup. Sessions created later are still
    /// loaded lazily by [`ensure_focused_session`] on focus.
    pub(crate) async fn ensure_all_sessions(&mut self) {
        // Unviewed sessions spawn at the first view's pane; the debounced
        // recompute converges viewed sessions once focus settles.
        let pane = self
            .views
            .values()
            .next()
            .map(|view| self.pane_size_for(view.size))
            .unwrap_or((80, 24));

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
        self.schedule_size_recompute();
    }

    /// Spawn the embedded loop for the session a view is focused on, if it
    /// isn't running yet.
    pub(crate) async fn ensure_focused_session(&mut self, vid: u64) {
        let Some(project_index) = self.views.get(&vid).map(|view| view.focus.project) else {
            return;
        };
        self.workspace.ensure_project_session(project_index);
        let selected = {
            let Some(view) = self.views.get_mut(&vid) else {
                return;
            };
            view.focus.clamp_session(&self.workspace, project_index);
            view.focus.selected_session(project_index)
        };
        // Spawn at the min pane among this session's viewers (this view is
        // one of them, so the target always exists).
        let pane = self
            .session_target_pane(project_index, selected)
            .unwrap_or((80, 24));
        let Some(project) = self.workspace.projects.get(project_index) else {
            return;
        };
        let cwd = project.cwd.clone();
        let Some(session) = project.sessions.get(selected) else {
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

    pub(crate) fn session_for_focus(&self, focus: &Focus) -> Option<&workspace::MuxSession> {
        let project = self.workspace.projects.get(focus.project)?;
        project.sessions.get(focus.selected_session(focus.project))
    }

    /// The session a view is focused on.
    pub(crate) fn focused_session(&self, vid: u64) -> Option<&workspace::MuxSession> {
        self.session_for_focus(&self.views.get(&vid)?.focus)
    }

    pub(crate) fn focused_session_mut(&mut self, vid: u64) -> Option<&mut workspace::MuxSession> {
        let focus = &self.views.get(&vid)?.focus;
        let (project_index, selected) = (focus.project, focus.selected_session(focus.project));
        let project = self.workspace.projects.get_mut(project_index)?;
        project.sessions.get_mut(selected)
    }

    /// Client ids of the views currently focused on the session with `key`.
    pub(crate) fn views_focused_on(&self, key: SessionKey) -> Vec<u64> {
        self.views
            .iter()
            .filter_map(|(id, view)| {
                self.session_for_focus(&view.focus)
                    .is_some_and(|s| s.key == key)
                    .then_some(*id)
            })
            .collect()
    }

    /// The client a session-initiated round-trip (external editor, model
    /// picker) should target: the last active client if it's viewing the
    /// session, otherwise any viewer.
    pub(crate) fn route_client_for_session(&self, key: SessionKey) -> Option<u64> {
        let viewers = self.views_focused_on(key);
        if viewers.contains(&self.last_active_client) {
            Some(self.last_active_client)
        } else {
            viewers.first().copied()
        }
    }

    pub(crate) fn forward_to_session(&mut self, vid: u64, event: Event) {
        if let Some(handle) = self.focused_session(vid).and_then(|s| s.handle.as_ref()) {
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

    pub(crate) fn on_focus_changed(&mut self, vid: u64) {
        if let Some(handle) = self.focused_session(vid).and_then(|s| s.handle.as_ref()) {
            driver::lock_parser(&handle.parser)
                .screen_mut()
                .set_scrollback(0);
        }
        if let Some(view) = self.views.get_mut(&vid) {
            view.selection.clear();
            view.cached_scrollback_total = 0;
            // The new focused session's content is unrelated to the previous
            // frame's row hashes; drop them so scroll detection can't mistake
            // the session switch for a scroll, and force a clean repaint.
            view.renderer.invalidate();
            view.surface.invalidate();
            view.alt_screen_active = false;
        }
        self.schedule_size_recompute();
        self.persist_now();
    }
}
