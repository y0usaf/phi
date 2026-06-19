//! The mux main loop: owns the real terminal, multiplexes input between the
//! sidebar, overlays, the `:` command line, and the focused embedded
//! session, and composites every frame from the session's vt100 screen.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use phi_tui::RawModeGuard;
use phi_tui::crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use tokio::sync::mpsc;

use super::clipboard::MuxClipboard;
use super::command::{MuxCommand, parse_command};
use super::command_line::{CommandLineEvent, CommandLineState, command_line_start_col};
use super::driver::{self, MuxChannels, SessionKey};
use super::keymap::{AppAction, KeyStroke, Keymap, KeymapMatch};
use super::persisted::{self, PersistedState};
use super::status::SessionStatus;
use super::ui::ansi::{AnsiRenderer, HardwareCursor};
use super::ui::cell_surface::CellSurface;
use super::ui::layout::{CellLayout, SIDEBAR_WIDTH_DEFAULT, compute_cell_layout};
use super::ui::overlays::archive::{ArchiveOutcome, ArchiveViewerState, render_archive_viewer};
use super::ui::overlays::help::{HelpOverlayState, render_help_overlay};
use super::ui::overlays::model::{ModelOverlayOutcome, ModelOverlayState, render_model_overlay};
use super::ui::overlays::usage::{UsageOverlayState, render_usage_overlay};
use super::ui::scene::{
    Chrome, MuxMode, ScenePalette, StatusNoteKind, render_scene, statusline_status,
};
use super::ui::selection::{SelectionPoint, TerminalSelection, selected_text};
use super::ui::sidebar::{
    SidebarRowKind, build_sidebar_rows, ensure_sidebar_selection_visible, project_sidebar_title,
    scroll_sidebar_by_rows_value, selected_sidebar_selection_span, session_sidebar_title,
    sidebar_has_spinner, sidebar_viewport_items, sticky_sidebar_anchor_row,
};
use super::workspace::{self, Workspace};
use phi_session::config::MuxConfig;
use phi_session::session_builder::SharedResources;
use phi_session::tui::{self, HostModalKind, HostModalRequest};

const ANIMATION_TICK: Duration = Duration::from_millis(60);
const NOTE_TTL: Duration = Duration::from_secs(4);
const WHEEL_LINES: i32 = 3;
const ARCHIVE_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

pub async fn run(shared: SharedResources, mux_config: MuxConfig, cwd: PathBuf) -> Result<()> {
    install_panic_log_hook();

    let mut persisted = PersistedState::load_default().unwrap_or_default();
    let mut project_dirs: Vec<PathBuf> = persisted.projects.iter().map(PathBuf::from).collect();
    let push_project = |dirs: &mut Vec<PathBuf>, dir: PathBuf| {
        let dir = PathBuf::from(persisted::normalize_path(&dir));
        if dir.is_dir() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };
    push_project(&mut project_dirs, cwd);
    for dir in &shared.args.project {
        push_project(&mut project_dirs, PathBuf::from(dir));
    }
    project_dirs.retain(|dir| dir.is_dir());

    persisted.normalize();
    let mut workspace = Workspace::discover(project_dirs);

    if let Some(selected) = &persisted.selected_project {
        let selected = PathBuf::from(selected);
        if let Some(i) = workspace.projects.iter().position(|p| p.cwd == selected) {
            workspace.selected_project = i;
        }
    }

    if let Some(session) = &persisted.selected_session {
        let session = PathBuf::from(session);

        let mut matched_project: Option<usize> = None;
        let mut matched_session: Option<usize> = None;

        if let Some(project) = workspace.projects.get(workspace.selected_project)
            && let Some(i) = project
                .sessions
                .iter()
                .position(|s| s.store_path.as_deref() == Some(session.as_path()))
        {
            matched_project = Some(workspace.selected_project);
            matched_session = Some(i);
        }

        if matched_project.is_none() {
            for (project_index, project) in workspace.projects.iter().enumerate() {
                if let Some(i) = project
                    .sessions
                    .iter()
                    .position(|s| s.store_path.as_deref() == Some(session.as_path()))
                {
                    matched_project = Some(project_index);
                    matched_session = Some(i);
                    break;
                }
            }
        }

        if let (Some(project_index), Some(session_index)) = (matched_project, matched_session) {
            workspace.selected_project = project_index;
            if let Some(project) = workspace.projects.get_mut(project_index) {
                project.selected = session_index;
            }
        }
    }

    let _ = workspace.ensure_project_session(workspace.selected_project);

    let guard = RawModeGuard::new().context("mux requires a tty")?;
    let mut stdout = std::io::stdout();
    let _ = phi_tui::crossterm::execute!(stdout, EnableMouseCapture);
    // The mux owns the whole screen (unlike the inline single-session TUI).
    let _ = write!(stdout, "\x1b[2J\x1b[H");

    let (status_tx, status_rx) = mpsc::unbounded_channel();
    let (redraw_tx, redraw_rx) = mpsc::unbounded_channel();
    let (suspend_tx, suspend_rx) = mpsc::unbounded_channel();
    let (host_modal_tx, host_modal_rx) = mpsc::unbounded_channel();

    let mut app = MuxApp {
        shared,
        workspace,
        keymap: Keymap::from_overrides(mux_config.keybinds.as_ref().unwrap_or(&Default::default())),
        sidebar_width: mux_config.sidebar_width.unwrap_or(SIDEBAR_WIDTH_DEFAULT),
        command_line: None,
        help_overlay: None,
        usage_overlay: None,
        model_overlay: None,
        archive_viewer: None,
        selection: TerminalSelection::default(),
        clipboard: MuxClipboard::default(),
        renderer: AnsiRenderer::default(),
        size: phi_tui::crossterm::terminal::size().unwrap_or((80, 24)),
        sidebar_scroll: 0,
        note: None,
        channels: MuxChannels {
            status_tx,
            redraw_tx,
            suspend_tx,
            host_modal_tx,
        },
        quit: false,
    };

    let result = app
        .main_loop(&guard, status_rx, redraw_rx, suspend_rx, host_modal_rx)
        .await;

    app.persist_now();
    PersistedState::flush();
    // Drop sessions before restoring the terminal so their final writes land
    // in the in-memory parsers, not the real screen.
    drop(app);
    let _ = phi_tui::crossterm::execute!(stdout, DisableMouseCapture);
    let _ = write!(stdout, "\x1b[2J\x1b[H\x1b[?25h");
    let _ = stdout.flush();
    drop(guard);
    result
}

struct MuxApp {
    shared: SharedResources,
    workspace: Workspace,
    keymap: Keymap,
    sidebar_width: u16,
    command_line: Option<CommandLineState>,
    help_overlay: Option<HelpOverlayState>,
    usage_overlay: Option<UsageOverlayState>,
    model_overlay: Option<ModelOverlayState>,
    archive_viewer: Option<ArchiveViewerState>,
    selection: TerminalSelection,
    clipboard: MuxClipboard,
    renderer: AnsiRenderer,
    size: (u16, u16),
    sidebar_scroll: usize,
    note: Option<(String, StatusNoteKind, Instant)>,
    channels: MuxChannels,
    quit: bool,
}

impl MuxApp {
    async fn main_loop(
        &mut self,
        guard: &RawModeGuard,
        mut status_rx: mpsc::UnboundedReceiver<(SessionKey, SessionStatus)>,
        mut redraw_rx: mpsc::UnboundedReceiver<()>,
        mut suspend_rx: mpsc::UnboundedReceiver<(SessionKey, tui::SuspendRequest)>,
        mut host_modal_rx: mpsc::UnboundedReceiver<(SessionKey, HostModalRequest)>,
    ) -> Result<()> {
        use futures::StreamExt as _;
        let mut events = EventStream::new();
        let mut ticker = tokio::time::interval(ANIMATION_TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        self.ensure_all_sessions().await;
        self.draw();

        while !self.quit {
            let mut dirty = false;
            tokio::select! {
                event = events.next() => {
                    match event {
                        Some(Ok(event)) => {
                            self.handle_event(event).await;
                            dirty = true;
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

            if dirty {
                self.ensure_focused_session().await;
                self.draw();
            }
        }
        Ok(())
    }

    // ----- state helpers -------------------------------------------------

    fn layout(&self) -> CellLayout {
        compute_cell_layout(self.size.0, self.size.1, self.sidebar_width)
    }

    fn pane_size(&self) -> (u16, u16) {
        let terminal = self.layout().terminal;
        (terminal.cols.max(1) as u16, terminal.rows.max(1) as u16)
    }

    fn set_note(&mut self, text: impl Into<String>, kind: StatusNoteKind) {
        self.note = Some((text.into(), kind, Instant::now()));
    }

    fn overlay_active(&self) -> bool {
        self.help_overlay.is_some()
            || self.usage_overlay.is_some()
            || self.model_overlay.is_some()
            || self.archive_viewer.is_some()
    }

    fn persist_now(&self) {
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
            selected_session: self
                .workspace
                .projects
                .get(self.workspace.selected_project)
                .and_then(|p| p.sessions.get(p.selected))
                .filter(|s| !s.is_unmaterialized_draft())
                .and_then(|s| s.store_path.as_ref())
                .map(|p| p.display().to_string()),
        };
        state.enqueue_save();
    }

    fn apply_status(&mut self, key: SessionKey, status: SessionStatus) {
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
            self.workspace.refresh_project(project_index);
        }
    }

    /// Spawn every session across all projects in parallel, so all are
    /// warm immediately at startup. Sessions created later are still
    /// loaded lazily by [`ensure_focused_session`] on focus.
    async fn ensure_all_sessions(&mut self) {
        let pane = self.pane_size();

        // Ensure every project has at least one (possibly draft) session.
        for i in 0..self.workspace.projects.len() {
            let _ = self.workspace.ensure_project_session(i);
        }

        // Collect sessions that still need an embedded handle.
        let mut specs: Vec<(SessionKey, PathBuf, Option<PathBuf>)> = Vec::new();
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
        let futures = specs
            .iter()
            .map(|(key, cwd, resume)| {
                driver::spawn(*key, cwd, resume.as_deref(), pane, shared, channels)
            });
        let results = futures::future::join_all(futures).await;

        // Assign handles (or crash status) back to the workspace.
        let mut errors: Vec<String> = Vec::new();
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
    async fn ensure_focused_session(&mut self) {
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

    fn focused_session(&self) -> Option<&workspace::MuxSession> {
        let project = self
            .workspace
            .projects
            .get(self.workspace.selected_project)?;
        project.sessions.get(project.selected)
    }

    fn focused_session_mut(&mut self) -> Option<&mut workspace::MuxSession> {
        let project = self
            .workspace
            .projects
            .get_mut(self.workspace.selected_project)?;
        project.sessions.get_mut(project.selected)
    }

    fn forward_to_session(&mut self, event: Event) {
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

    fn on_focus_changed(&mut self) {
        self.selection.clear();
        if let Some(handle) = self.focused_session().and_then(|s| s.handle.as_ref()) {
            driver::lock_parser(&handle.parser)
                .screen_mut()
                .set_scrollback(0);
        }
        self.persist_now();
    }

    // ----- input routing --------------------------------------------------

    async fn handle_event(&mut self, event: Event) {
        match event {
            Event::Resize(cols, rows) => self.handle_resize(cols, rows),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Paste(text) => {
                if self.command_line.is_some() {
                    if let Some(command_line) = &mut self.command_line {
                        command_line.insert_str(&text.replace(['\n', '\r'], " "));
                    }
                } else if !self.overlay_active() {
                    self.forward_to_session(Event::Paste(text));
                }
            }
            Event::Key(key) => {
                if key.kind == KeyEventKind::Release {
                    return;
                }
                self.handle_key(key).await;
            }
            Event::FocusGained | Event::FocusLost => {}
        }
    }

    fn handle_resize(&mut self, cols: u16, rows: u16) {
        self.size = (cols, rows);
        let pane = self.pane_size();
        for project in &mut self.workspace.projects {
            for session in &mut project.sessions {
                if let Some(handle) = &session.handle {
                    handle.resize(pane.0, pane.1);
                }
            }
        }
        self.renderer.invalidate();
    }

    async fn handle_key(&mut self, key: KeyEvent) {
        let (cols, rows) = self.size;

        if let Some(viewer) = &mut self.archive_viewer {
            match viewer.handle_key(&key, cols, rows) {
                ArchiveOutcome::Close => self.archive_viewer = None,
                ArchiveOutcome::Restore(path) => self.restore_archived(&path),
                ArchiveOutcome::None => {}
            }
            return;
        }
        if let Some(model) = &mut self.model_overlay {
            match model.handle_key(&key, cols, rows) {
                ModelOverlayOutcome::None => {}
                ModelOverlayOutcome::Close => self.finish_model_overlay(None),
                ModelOverlayOutcome::Submit(value) => self.finish_model_overlay(Some(value)),
            }
            return;
        }
        if let Some(usage) = &mut self.usage_overlay {
            if usage.handle_key(&key, cols, rows) {
                self.usage_overlay = None;
            }
            return;
        }
        if let Some(help) = &mut self.help_overlay {
            let keymap = self.keymap.clone();
            if help.handle_key(&key, &keymap, cols, rows) {
                self.help_overlay = None;
            }
            return;
        }

        if self.command_line.is_some() {
            // `::` immediately after opening sends a literal ':' through.
            let literal_colon = self
                .command_line
                .as_ref()
                .is_some_and(|c| c.input.is_empty())
                && matches!(key.code, KeyCode::Char(':'))
                && !key.modifiers.contains(KeyModifiers::CONTROL);
            if literal_colon {
                self.command_line = None;
                self.forward_to_session(Event::Key(key));
                return;
            }
            let outcome = self
                .command_line
                .as_mut()
                .map(|c| c.handle_key(&key))
                .unwrap_or(CommandLineEvent::Consumed);
            match outcome {
                CommandLineEvent::Consumed => {}
                CommandLineEvent::Cancel => {
                    self.command_line = None;
                }
                CommandLineEvent::Submit(input) => {
                    self.command_line = None;
                    self.run_command_input(&input).await;
                }
            }
            return;
        }

        if matches!(key.code, KeyCode::Char(':'))
            && key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
        {
            self.command_line = Some(CommandLineState::with_input(""));
            return;
        }

        match KeyStroke::from_key_event(&key) {
            Some(stroke) => match self.keymap.lookup(&stroke) {
                KeymapMatch::Triggered(action) => self.run_action(action).await,
                KeymapMatch::NoMatch => self.forward_to_session(Event::Key(key)),
            },
            None => self.forward_to_session(Event::Key(key)),
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let (cols, rows) = self.size;
        if self.overlay_active() {
            let delta = match mouse.kind {
                MouseEventKind::ScrollUp => -WHEEL_LINES,
                MouseEventKind::ScrollDown => WHEEL_LINES,
                _ => return,
            };
            if let Some(viewer) = &mut self.archive_viewer {
                viewer.move_selection(delta);
            } else if let Some(model) = &mut self.model_overlay {
                model.move_selection(delta);
            } else if let Some(usage) = &mut self.usage_overlay {
                usage.scroll_by(delta, cols, rows);
            } else if let Some(help) = &mut self.help_overlay {
                let keymap = self.keymap.clone();
                help.scroll_by(delta, &keymap, cols, rows);
            }
            return;
        }

        let layout = self.layout();
        let (col, row) = (i32::from(mouse.column), i32::from(mouse.row));
        let in_terminal = layout.terminal.contains_cell(col, row);
        let in_sidebar = layout.sidebar.contains_cell(col, row);

        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let delta = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                    -WHEEL_LINES
                } else {
                    WHEEL_LINES
                };
                if in_sidebar {
                    let visible = layout.sidebar.rows.max(0) as usize;
                    let row_count = build_sidebar_rows(
                        &self.workspace.projects,
                        self.workspace.selected_project,
                    )
                    .len();
                    let (next, _) = scroll_sidebar_by_rows_value(
                        self.sidebar_scroll,
                        delta,
                        visible,
                        row_count,
                    );
                    self.sidebar_scroll = next;
                } else if in_terminal {
                    self.scroll_terminal(-delta);
                }
            }
            MouseEventKind::Down(MouseButton::Left) if in_sidebar => {
                self.handle_sidebar_click(&layout, mouse.row);
            }
            MouseEventKind::Down(MouseButton::Left) if in_terminal => {
                let point = terminal_point(&layout, mouse.column, mouse.row);
                self.selection.set(point);
            }
            MouseEventKind::Drag(MouseButton::Left) if self.selection.is_active() => {
                let point = terminal_point(&layout, mouse.column, mouse.row);
                self.selection.update_focus(point);
            }
            MouseEventKind::Up(MouseButton::Left) if self.selection.is_active() => {
                self.copy_selection(false);
            }
            _ => {}
        }
    }

    /// Select the project or session under a sidebar click, mirroring the
    /// viewport (scroll + sticky header) the last frame was drawn with.
    fn handle_sidebar_click(&mut self, layout: &CellLayout, row: u16) {
        let visible = layout.sidebar.rows.max(0) as usize;
        let visible_row = (i32::from(row) - layout.sidebar.row).max(0) as usize;
        let rows = build_sidebar_rows(&self.workspace.projects, self.workspace.selected_project);
        let sticky = selected_sidebar_selection_span(self.workspace.selected_project, &rows)
            .and_then(|span| sticky_sidebar_anchor_row(self.sidebar_scroll, visible, span));
        let viewport = sidebar_viewport_items(&rows, self.sidebar_scroll, visible, sticky);
        let Some(item) = viewport.iter().find(|i| i.visible_row == visible_row) else {
            return;
        };
        let Some(kind) = rows.get(item.row_index).map(|r| r.kind.clone()) else {
            return;
        };
        match kind {
            SidebarRowKind::Project(index) => {
                self.workspace.selected_project = index;
            }
            SidebarRowKind::Session {
                project_index,
                session_index,
            } => {
                self.workspace.selected_project = project_index;
                if let Some(project) = self.workspace.projects.get_mut(project_index) {
                    project.selected = session_index;
                }
            }
        }
        self.on_focus_changed();
    }

    fn scroll_terminal(&mut self, delta_up: i32) {
        if let Some(handle) = self.focused_session().and_then(|s| s.handle.as_ref()) {
            let mut parser = driver::lock_parser(&handle.parser);
            let current = parser.screen().scrollback() as i32;
            let next = (current + delta_up).max(0) as usize;
            parser.screen_mut().set_scrollback(next);
        }
    }

    // ----- actions ---------------------------------------------------------

    async fn run_action(&mut self, action: AppAction) {
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
                    self.workspace.refresh_project(index);
                }
                self.set_note("rescanned all projects", StatusNoteKind::Ok);
            }
            AppAction::ArchiveSession => self.archive_focused_session().await,
            AppAction::RemoveProject => {
                let index = self.workspace.selected_project;
                if let Some(project) = self.workspace.projects.get(index) {
                    let title = project_sidebar_title(project);
                    self.workspace.remove_project(index);
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
    fn refresh_focused_session(&mut self) {
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
            self.workspace.refresh_project(index);
            self.set_note("project rescanned", StatusNoteKind::Ok);
        }
    }

    async fn archive_focused_session(&mut self) {
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
                self.workspace.refresh_project(index);
                // The archived entry survives a rescan only as a stale draft;
                // drop it explicitly.
                if let Some(project) = self.workspace.projects.get_mut(index) {
                    project
                        .sessions
                        .retain(|s| !(s.is_draft() && s.handle.is_none() && s.key != 0));
                    if project.sessions.is_empty() {
                        project.sessions.clear();
                    }
                    project.selected = project
                        .selected
                        .min(project.sessions.len().saturating_sub(1));
                }
                self.workspace.normalize_visible_selection(index);
                drain_pending_terminal_events();
                self.ensure_focused_session().await;
                self.set_note("session archived", StatusNoteKind::Ok);
                self.on_focus_changed();
            }
            Err(error) => self.set_note(format!("archive: {error:#}"), StatusNoteKind::Error),
        }
    }

    fn restore_archived(&mut self, archived: &std::path::Path) {
        match workspace::restore_session(archived) {
            Ok(target) => {
                self.archive_viewer = None;
                for index in 0..self.workspace.projects.len() {
                    self.workspace.refresh_project(index);
                }
                self.set_note(format!("restored {}", target.display()), StatusNoteKind::Ok);
            }
            Err(error) => {
                if let Some(viewer) = &mut self.archive_viewer {
                    viewer.reload_sessions();
                    viewer.note = Some(format!("{error:#}"));
                }
            }
        }
    }

    fn copy_selection(&mut self, note_on_empty: bool) {
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

    async fn run_command_input(&mut self, input: &str) {
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

    fn handle_host_modal_request(&mut self, key: SessionKey, request: HostModalRequest) {
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

    fn finish_model_overlay(&mut self, selected: Option<String>) {
        if let Some(mut model) = self.model_overlay.take()
            && let Some(tx) = model.response_tx.take()
        {
            let _ = tx.send(Ok(selected));
        }
    }

    // ----- suspension ($EDITOR) --------------------------------------------

    fn handle_suspend(&mut self, guard: &RawModeGuard, request: tui::SuspendRequest) {
        let mut stdout = std::io::stdout();
        let _ = phi_tui::crossterm::execute!(stdout, DisableMouseCapture);
        let result = guard.with_suspended(|| tui::external_editor(&request.text));
        let _ = phi_tui::crossterm::execute!(stdout, EnableMouseCapture);
        let _ = request.response_tx.send(result);
        self.renderer.invalidate();
    }

    // ----- rendering ---------------------------------------------------------

    fn draw(&mut self) {
        let (cols, rows) = self.size;
        let layout = self.layout();
        let palette = ScenePalette::default();
        let mut surface =
            CellSurface::new(i32::from(cols), i32::from(rows), palette.fg, palette.bg);

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let sidebar_rows =
            build_sidebar_rows(&self.workspace.projects, self.workspace.selected_project);
        let visible = layout.sidebar.rows.max(0) as usize;
        self.sidebar_scroll = ensure_sidebar_selection_visible(
            self.sidebar_scroll,
            self.workspace.selected_project,
            &sidebar_rows,
            visible,
        );
        let sticky =
            selected_sidebar_selection_span(self.workspace.selected_project, &sidebar_rows)
                .and_then(|span| sticky_sidebar_anchor_row(self.sidebar_scroll, visible, span));
        let viewport = sidebar_viewport_items(&sidebar_rows, self.sidebar_scroll, visible, sticky);

        let mode = if self.command_line.is_some() {
            MuxMode::Command
        } else {
            MuxMode::Normal
        };
        let chrome = self.chrome();
        let selection = self.selection.normalized();

        let focused_parser = self
            .focused_session()
            .and_then(|s| s.handle.as_ref())
            .map(|h| h.parser.clone());
        let mut parser_guard = focused_parser.as_ref().map(|p| driver::lock_parser(p));
        let scrollback_total = parser_guard
            .as_deref_mut()
            .map(|p| {
                let screen = p.screen_mut();
                let offset = screen.scrollback();
                screen.set_scrollback(usize::MAX);
                let total = screen.scrollback();
                screen.set_scrollback(offset);
                total
            })
            .unwrap_or(0);
        let screen = parser_guard.as_deref().map(|p| p.screen());

        let mut hardware_cursor = render_scene(
            &mut surface,
            &layout,
            &sidebar_rows,
            &viewport,
            screen,
            selection,
            &chrome,
            &palette,
            mode,
            now_ms,
            scrollback_total,
        )
        .map(|pos| HardwareCursor {
            col: pos.col,
            row: pos.row,
        });
        drop(parser_guard);

        if let Some(command_line) = &self.command_line {
            let start_col = command_line_start_col(&layout, i32::from(cols));
            let max_cols = (i32::from(cols) - start_col - 1).max(1);
            let (text, cursor_col) = command_line.visible_text_and_cursor_col(max_cols as usize);
            surface.put_text(
                start_col + 1,
                layout.statusbar.row,
                max_cols,
                palette.border,
                palette.warning,
                &text,
            );
            hardware_cursor = Some(HardwareCursor {
                col: start_col + 1 + cursor_col,
                row: layout.statusbar.row,
            });
        }

        if let Some(viewer) = &mut self.archive_viewer {
            render_archive_viewer(&mut surface, viewer);
            hardware_cursor = None;
        } else if let Some(model) = &mut self.model_overlay {
            render_model_overlay(&mut surface, model);
            hardware_cursor = None;
        } else if let Some(usage) = &mut self.usage_overlay {
            render_usage_overlay(&mut surface, usage);
            hardware_cursor = None;
        } else if let Some(help) = &mut self.help_overlay {
            let keymap = self.keymap.clone();
            render_help_overlay(&mut surface, help, &keymap);
            hardware_cursor = None;
        }

        let mut stdout = std::io::stdout();
        let _ = self.renderer.render(&mut stdout, surface, hardware_cursor);
    }

    fn chrome(&self) -> Chrome {
        let project = self
            .workspace
            .projects
            .get(self.workspace.selected_project)
            .map(project_sidebar_title)
            .unwrap_or_default();
        let session = self
            .focused_session()
            .map(session_sidebar_title)
            .unwrap_or_default();
        match &self.note {
            Some((text, kind, _)) => Chrome {
                project,
                session,
                status: text.clone(),
                status_kind: Some(*kind),
            },
            None => Chrome {
                project,
                session,
                status: self
                    .focused_session()
                    .map(|s| statusline_status(&s.status))
                    .unwrap_or_default(),
                status_kind: None,
            },
        }
    }
}

fn drain_pending_terminal_events() {
    use phi_tui::crossterm::event::{self, Event};

    while event::poll(Duration::from_millis(0)).unwrap_or(false) {
        match event::read() {
            Ok(
                Event::Key(_)
                | Event::Mouse(_)
                | Event::Paste(_)
                | Event::Resize(_, _)
                | Event::FocusGained
                | Event::FocusLost,
            ) => {}
            Err(_) => break,
        }
    }
}

fn terminal_point(layout: &CellLayout, column: u16, row: u16) -> SelectionPoint {
    let rect = layout.terminal;
    let col = i32::from(column) - rect.col;
    let row = i32::from(row) - rect.row;
    SelectionPoint {
        col: col.clamp(0, (rect.cols - 1).max(0)) as u16,
        row: row.clamp(0, (rect.rows - 1).max(0)) as u16,
    }
}

/// Panics (from the mux loop or any session task) go to a log file instead
/// of stderr so they don't corrupt the composited screen.
fn install_panic_log_hook() {
    std::panic::set_hook(Box::new(|info| {
        let Some(dirs) = directories::ProjectDirs::from("", "", "phi") else {
            return;
        };
        let path = dirs.data_dir().join("mux-panic.log");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(file, "[{:?}] panic: {info}", std::time::SystemTime::now());
            let _ = writeln!(file, "{}", std::backtrace::Backtrace::force_capture());
        }
    }));
}
