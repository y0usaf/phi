use super::*;

impl MuxApp {
    pub(crate) async fn handle_event(&mut self, vid: u64, event: Event) {
        match event {
            Event::Resize(cols, rows) => self.handle_resize(vid, cols, rows),
            Event::Mouse(mouse) => self.handle_mouse(vid, mouse),
            Event::Paste(text) => {
                let Some(view) = self.views.get_mut(&vid) else {
                    return;
                };
                if let Some(command_line) = &mut view.command_line {
                    command_line.insert_str(&text.replace(['\n', '\r'], " "));
                } else if !view.overlay_active() {
                    self.forward_to_session(vid, Event::Paste(text));
                }
            }
            Event::Key(key) => {
                if key.kind == KeyEventKind::Release {
                    return;
                }
                self.handle_key(vid, key).await;
            }
            // Focus events drive the animation clock's throttle: focused →
            // 16ms tick (~60fps), blurred → 32ms tick (~31fps), matching
            // claude-code's ClockProvider + useTerminalFocus model.
            Event::FocusGained => {
                self.clock.set_focused(true);
                self.forward_to_session(vid, Event::FocusGained);
            }
            Event::FocusLost => {
                self.clock.set_focused(false);
                self.forward_to_session(vid, Event::FocusLost);
            }
        }
    }

    pub(crate) fn handle_resize(&mut self, vid: u64, cols: u16, rows: u16) {
        if let Some(view) = self.views.get_mut(&vid) {
            view.size = (cols, rows);
            view.renderer.invalidate();
        }
        self.schedule_size_recompute();
    }

    pub(crate) async fn handle_key(&mut self, vid: u64, key: KeyEvent) {
        let Some(view) = self.views.get_mut(&vid) else {
            return;
        };
        let (cols, rows) = view.size;

        if let Some(active) = &mut view.active_extension_overlay {
            // The overlay's toggle key closes it again.
            if let Some(stroke) = KeyStroke::from_key_event(&key) {
                let toggled = active
                    .spec
                    .toggle_key
                    .as_deref()
                    .is_some_and(|t| KeyStroke::parse(t).is_ok_and(|target| target == stroke));
                if toggled {
                    view.active_extension_overlay = None;
                    return;
                }
            }
            let overlay_key = crate::ui::draw_context_impl::key_event_to_overlay_key(&key);
            let outcome = (active.spec.handle_key)(&mut active.state, &overlay_key, cols, rows);
            match outcome {
                phi_ext::OverlayOutcome::None => {} // consumed, no change
                phi_ext::OverlayOutcome::Close => view.active_extension_overlay = None,
                phi_ext::OverlayOutcome::Submit(value) => {
                    view.active_extension_overlay = None;
                    self.set_note_on(vid, format!("Overlay: {value}"), super::StatusNoteKind::Ok);
                }
                phi_ext::OverlayOutcome::Complete(output) => {
                    view.active_extension_overlay = None;
                    // Actions first, message second: a host action may post
                    // its own note (e.g. "reload" → "rescanned all projects"),
                    // and the overlay's status is what should stick.
                    for action in output.actions {
                        self.apply_mux_ui_action(vid, action).await;
                    }
                    if let Some(message) = output.message {
                        self.set_note_on(vid, message, super::StatusNoteKind::Ok);
                    }
                }
            }
            return;
        }

        if view.command_line.is_some() {
            // `::` immediately after opening sends a literal ':' through.
            let literal_colon = view
                .command_line
                .as_ref()
                .is_some_and(|c| c.input.is_empty())
                && matches!(key.code, KeyCode::Char(':'))
                && !key.modifiers.contains(KeyModifiers::CONTROL);
            if literal_colon {
                view.command_line = None;
                self.forward_to_session(vid, Event::Key(key));
                return;
            }
            let outcome = view
                .command_line
                .as_mut()
                .map(|c| c.handle_key(&key))
                .unwrap_or(CommandLineEvent::Consumed);
            match outcome {
                CommandLineEvent::Consumed => {}
                CommandLineEvent::Cancel => {
                    view.command_line = None;
                }
                CommandLineEvent::Submit(input) => {
                    view.command_line = None;
                    self.run_command_input(vid, &input).await;
                }
            }
            return;
        }

        if matches!(key.code, KeyCode::Char(':'))
            && key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
        {
            view.command_line = Some(CommandLineState::with_input(""));
            return;
        }

        // Check extension overlay toggle keys.
        //
        // Toggle keys are parsed as KeyStroke strings (e.g. "ctrl+?") so
        // that modifiers are honoured, unlike the old raw-char matching.
        if let Some(stroke) = KeyStroke::from_key_event(&key)
            && view.active_extension_overlay.is_none()
        {
            let toggled = self.extension_overlays.iter().find_map(|spec| {
                let toggle_key = spec.toggle_key.as_deref()?;
                KeyStroke::parse(toggle_key)
                    .is_ok_and(|target| target == stroke)
                    .then(|| spec.name.clone())
            });
            if let Some(name) = toggled {
                self.open_extension_overlay(vid, &name, None);
                return;
            }
        }

        match KeyStroke::from_key_event(&key) {
            Some(stroke) => match self.keymap.lookup(&stroke) {
                KeymapMatch::Triggered(action) => self.run_action(vid, action).await,
                KeymapMatch::NoMatch => self.forward_to_session(vid, Event::Key(key)),
            },
            None => self.forward_to_session(vid, Event::Key(key)),
        }
    }

    pub(crate) fn handle_mouse(&mut self, vid: u64, mouse: MouseEvent) {
        let sidebar_width = self.sidebar_width;
        let Some(view) = self.views.get_mut(&vid) else {
            return;
        };
        let (cols, rows) = view.size;
        if view.overlay_active() {
            let delta = match mouse.kind {
                MouseEventKind::ScrollUp => -WHEEL_LINES,
                MouseEventKind::ScrollDown => WHEEL_LINES,
                _ => return,
            };
            if let Some(active) = &mut view.active_extension_overlay
                && let Some(handle_scroll) = &active.spec.handle_scroll
            {
                handle_scroll(&mut active.state, delta, cols, rows);
            }
            return;
        }

        let layout = compute_cell_layout(cols, rows, sidebar_width);
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
                    let row_count = build_sidebar_rows(&self.workspace.projects, &view.focus).len();
                    let (next, _) = scroll_sidebar_by_rows_value(
                        view.sidebar_scroll,
                        delta,
                        visible,
                        row_count,
                    );
                    view.sidebar_scroll = next;
                } else if in_terminal {
                    self.scroll_terminal(vid, -delta);
                }
            }
            MouseEventKind::Down(MouseButton::Left) if in_sidebar => {
                self.handle_sidebar_click(vid, &layout, mouse.row);
            }
            MouseEventKind::Down(MouseButton::Left) if in_terminal => {
                let point = terminal_point(&layout, mouse.column, mouse.row);
                view.selection.set(point);
            }
            MouseEventKind::Drag(MouseButton::Left) if view.selection.is_active() => {
                let point = terminal_point(&layout, mouse.column, mouse.row);
                view.selection.update_focus(point);
            }
            MouseEventKind::Up(MouseButton::Left) if view.selection.is_active() => {
                view.selection.end_drag();
                self.copy_selection(vid, false);
            }
            _ => {}
        }
    }

    /// Select the project or session under a sidebar click, mirroring the
    /// viewport (scroll + sticky header) the last frame was drawn with.
    pub(crate) fn handle_sidebar_click(&mut self, vid: u64, layout: &CellLayout, row: u16) {
        let Some(view) = self.views.get_mut(&vid) else {
            return;
        };
        let visible = layout.sidebar.rows.max(0) as usize;
        let visible_row = (i32::from(row) - layout.sidebar.row).max(0) as usize;
        let rows = build_sidebar_rows(&self.workspace.projects, &view.focus);
        let sticky = selected_sidebar_selection_span(view.focus.project, &rows)
            .and_then(|span| sticky_sidebar_anchor_row(view.sidebar_scroll, visible, span));
        let viewport = sidebar_viewport_items(&rows, view.sidebar_scroll, visible, sticky);
        let Some(item) = viewport.iter().find(|i| i.visible_row == visible_row) else {
            return;
        };
        let Some(kind) = rows.get(item.row_index).map(|r| r.kind.clone()) else {
            return;
        };
        match kind {
            SidebarRowKind::Project(index) => {
                view.focus.project = index;
            }
            SidebarRowKind::Session {
                project_index,
                session_index,
            } => {
                view.focus.project = project_index;
                view.focus
                    .set_selected_session(project_index, session_index);
            }
        }
        self.on_focus_changed(vid);
    }

    pub(crate) fn scroll_terminal(&mut self, vid: u64, delta_up: i32) {
        // Compute the new scrollback and the delta, then drop the parser guard
        // before touching the view's selection (avoids borrow-conflict with
        // the immutable `focused_session` borrow). The scrollback offset
        // lives on the shared vt100 screen, so co-viewers scroll together.
        let scrolled = {
            let Some(handle) = self.focused_session(vid).and_then(|s| s.handle.as_ref()) else {
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
        if let Some(view) = self.views.get_mut(&vid)
            && view.selection.is_active()
            && scrolled != 0
        {
            view.selection.shift_rows(scrolled);
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
