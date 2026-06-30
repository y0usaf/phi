use super::*;

impl MuxApp {
    pub(crate) async fn handle_event(&mut self, event: Event) {
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

    pub(crate) fn handle_resize(&mut self, cols: u16, rows: u16) {
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

    pub(crate) async fn handle_key(&mut self, key: KeyEvent) {
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
        if let Some(active) = &mut self.active_extension_overlay {
            let overlay_key = crate::ui::draw_context_impl::key_event_to_overlay_key(&key);
            let outcome = (active.spec.handle_key)(&mut active.state, &overlay_key, cols, rows);
            match outcome {
                phi_ext::OverlayOutcome::None => {} // consumed, no change
                phi_ext::OverlayOutcome::Close => self.active_extension_overlay = None,
                phi_ext::OverlayOutcome::Submit(value) => {
                    self.set_note(format!("Overlay: {value}"), super::StatusNoteKind::Ok);
                    self.active_extension_overlay = None;
                }
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

        // Check extension overlay toggle keys.
        //
        // Toggle keys are parsed as KeyStroke strings (e.g. "ctrl+?") so
        // that modifiers are honoured, unlike the old raw-char matching.
        if let Some(stroke) = KeyStroke::from_key_event(&key) {
            if self.active_extension_overlay.is_none() {
                for spec in &self.extension_overlays {
                    if KeyStroke::parse(&spec.toggle_key)
                        .is_ok_and(|target| target == stroke)
                    {
                        let name = spec.name.clone();
                        let state = (spec.init_state)();
                        let spec = spec.clone();
                        self.active_extension_overlay =
                            Some(super::ActiveExtensionOverlay { name, spec, state });
                        return;
                    }
                }
            }
        }

        match KeyStroke::from_key_event(&key) {
            Some(stroke) => match self.keymap.lookup(&stroke) {
                KeymapMatch::Triggered(action) => self.run_action(action).await,
                KeymapMatch::NoMatch => self.forward_to_session(Event::Key(key)),
            },
            None => self.forward_to_session(Event::Key(key)),
        }
    }

    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent) {
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
                self.selection.end_drag();
                self.copy_selection(false);
            }
            _ => {}
        }
    }

    /// Select the project or session under a sidebar click, mirroring the
    /// viewport (scroll + sticky header) the last frame was drawn with.
    pub(crate) fn handle_sidebar_click(&mut self, layout: &CellLayout, row: u16) {
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

    pub(crate) fn scroll_terminal(&mut self, delta_up: i32) {
        // Compute the new scrollback and the delta, then drop the parser guard
        // before touching `self.selection` (avoids borrow-conflict with the
        // immutable `focused_session` borrow).
        let scrolled = {
            let Some(handle) = self.focused_session().and_then(|s| s.handle.as_ref()) else {
                return;
            };
            let mut parser = driver::lock_parser(&handle.parser);
            let current = parser.screen().scrollback() as i32;
            let next = (current + delta_up).max(0) as usize;
            parser.screen_mut().set_scrollback(next);
            next as i32 - current
        };

        // Shift the selection's row coordinates by the scroll delta so the
        // highlight tracks the same content (zellij's move_up/move_down
        // pattern). `delta_up > 0` means we scrolled up (into history), so
        // visible content moved down — the selection rows shift down too.
        if self.selection.is_active() && scrolled != 0 {
            self.selection.shift_rows(scrolled);
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

