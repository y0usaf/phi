use super::*;

impl MuxApp {
    pub(crate) fn draw(&mut self) {
        let (cols, rows) = self.size;
        let layout = self.layout();
        let palette = self.palette;

        // Reuse the persistent surface across frames (resized in place on
        // dimension change) so the interner survives and dirty tracking is
        // relative to the previous frame. `mem::take` sidesteps the partial
        // borrows of `self` below; the surface is put back at the end.
        let mut surface = std::mem::take(&mut self.surface);
        let want_cols = i32::from(cols);
        let want_rows = i32::from(rows);
        if surface.cols != want_cols || surface.rows != want_rows {
            surface.resize(want_cols, want_rows, palette.fg, palette.bg);
            // New dimensions: previous pane row hashes no longer line up.
            self.renderer.invalidate();
        }

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

        // Alt-screen transition hook (Priority 4): entering/leaving the
        // alternate screen replaces the whole pane, so drop scroll history and
        // force a full repaint to avoid scrolling primary content into history.
        let alt_active = screen.map(|s| s.alternate_screen()).unwrap_or(false);
        if alt_active != self.alt_screen_active {
            self.alt_screen_active = alt_active;
            surface.invalidate();
            self.renderer.invalidate();
        }

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
            render_archive_viewer(&mut surface, viewer, &palette);
            hardware_cursor = None;
        } else if let Some(model) = &mut self.model_overlay {
            render_model_overlay(&mut surface, model, &palette);
            hardware_cursor = None;
        } else if let Some(usage) = &mut self.usage_overlay {
            render_usage_overlay(&mut surface, usage, &palette);
            hardware_cursor = None;
        } else if let Some(help) = &mut self.help_overlay {
            let keymap = self.keymap.clone();
            render_help_overlay(&mut surface, help, &keymap, &palette);
            hardware_cursor = None;
        }

        let term_pane = layout.terminal;
        let mut stdout = std::io::stdout();
        let _ = self
            .renderer
            .render(&mut stdout, &mut surface, term_pane, hardware_cursor);
        self.surface = surface;
    }

    pub(crate) fn chrome(&self) -> Chrome {
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
