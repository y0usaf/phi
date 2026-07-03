use super::*;
use crate::ui::draw_context_impl::MuxDrawContext;

impl MuxApp {
    /// Composite and ship one frame per attached view. Each view renders at
    /// its own size through its own surface and diff renderer, so clients on
    /// different sessions (or different terminal sizes) are independent.
    pub(crate) fn draw(&mut self) {
        let vids: Vec<u64> = self.views.keys().copied().collect();
        for vid in vids {
            // Take the view out of the map so the render body can borrow
            // core state freely; reinsert when done.
            let Some(mut view) = self.views.remove(&vid) else {
                continue;
            };
            self.render_view(vid, &mut view);
            self.views.insert(vid, view);
        }
    }

    fn render_view(&mut self, vid: u64, view: &mut MuxView) {
        let (cols, rows) = view.size;
        let layout = self.layout_for(view.size);
        let palette = self.palette;

        // The persistent surface is reused across frames (resized in place
        // on dimension change) so the interner survives and dirty tracking
        // is relative to this view's previous frame.
        let want_cols = i32::from(cols);
        let want_rows = i32::from(rows);
        if view.surface.cols != want_cols || view.surface.rows != want_rows {
            view.surface
                .resize(want_cols, want_rows, palette.fg, palette.bg);
            // New dimensions: previous pane row hashes no longer line up.
            view.renderer.invalidate();
        }

        // Use the universal animation clock — the single source of truth
        // for all animation timing. All render functions in the same frame
        // see the same `now_ms`, keeping every animated surface in sync.
        let now_ms = self.clock.now_ms();

        let sidebar_rows = build_sidebar_rows(&self.workspace.projects, &view.focus);
        let visible = layout.sidebar.rows.max(0) as usize;
        view.sidebar_scroll = ensure_sidebar_selection_visible(
            view.sidebar_scroll,
            view.focus.project,
            &sidebar_rows,
            visible,
        );
        let sticky = selected_sidebar_selection_span(view.focus.project, &sidebar_rows)
            .and_then(|span| sticky_sidebar_anchor_row(view.sidebar_scroll, visible, span));
        let viewport = sidebar_viewport_items(&sidebar_rows, view.sidebar_scroll, visible, sticky);

        let mode = if view.command_line.is_some() {
            MuxMode::Command
        } else {
            MuxMode::Normal
        };
        let chrome = self.chrome(view);
        let selection = view.selection.normalized();

        let focused_parser = self
            .session_for_focus(&view.focus)
            .and_then(|s| s.handle.as_ref())
            .map(|h| h.parser.clone());

        // Snapshot the vt100 screen under the lock, then release immediately
        // so the session's ParserWriter never blocks during compositing.
        // The snapshot copies only the visible cells (~61KB for 80x24), not
        // the full scrollback buffer.
        let snapshot = focused_parser.as_ref().map(|p| {
            let mut guard = driver::lock_parser(p);
            let screen = guard.screen();
            let mut snap = ScreenSnapshot::from_screen(screen);
            // Compute the total scrollback under the lock (the only time we
            // touch the mutable offset), then restore it. Since we snapshot
            // only the visible region, this is the only mutation needed.
            let s = guard.screen_mut();
            let offset = s.scrollback();
            s.set_scrollback(usize::MAX);
            let total = s.scrollback();
            s.set_scrollback(offset);
            snap.scrollback_total = total;
            snap
        });

        // Use the cached scrollback total when available; update the cache
        // when it changes so the scrollbar stays accurate without recomputing
        // on every frame if the value hasn't changed.
        let scrollback_total = snapshot.as_ref().map(|s| s.scrollback_total).unwrap_or(0);
        if scrollback_total > view.cached_scrollback_total {
            view.cached_scrollback_total = scrollback_total;
        }
        let scrollback_total = view.cached_scrollback_total.max(scrollback_total);

        // Alt-screen transition hook (Priority 4): entering/leaving the
        // alternate screen replaces the whole pane, so drop scroll history and
        // force a full repaint to avoid scrolling primary content into history.
        let alt_active = snapshot.as_ref().is_some_and(|s| s.alternate_screen());
        if alt_active != view.alt_screen_active {
            view.alt_screen_active = alt_active;
            view.surface.invalidate();
            view.renderer.invalidate();
        }

        let visual =
            crate::ui::visual_context::VisualContext::from_runtime(self.visual_runtime.as_deref());

        let mut hardware_cursor = render_scene(
            &mut view.surface,
            &layout,
            &sidebar_rows,
            &viewport,
            snapshot.as_ref(),
            selection,
            &chrome,
            &palette,
            mode,
            now_ms,
            scrollback_total,
            &visual,
        )
        .map(|pos| HardwareCursor {
            col: pos.col,
            row: pos.row,
        });

        if let Some(command_line) = &view.command_line {
            let start_col = command_line_start_col(&layout, i32::from(cols));
            let max_cols = (i32::from(cols) - start_col - 1).max(1);
            let (text, cursor_col) = command_line.visible_text_and_cursor_col(max_cols as usize);
            view.surface.put_text(
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

        if view.active_extension_overlay.is_some() {
            let overlay_palette = crate::ui::draw_context_impl::overlay_palette(
                &palette,
                self.terminal_colors.background,
            );
            let MuxView {
                surface,
                active_extension_overlay,
                ..
            } = view;
            if let Some(active) = active_extension_overlay {
                let mut ctx = MuxDrawContext::new(surface, overlay_palette);
                (active.spec.render)(&mut ctx, &mut active.state);
            }
            hardware_cursor = None;
        }

        let term_pane = layout.terminal;
        // Render this view's diff and ship it to its own destination: the
        // local tty for the synthetic client, that client's socket queue in
        // daemon mode.
        let MuxView {
            surface, renderer, ..
        } = view;
        let render_result = match &self.sink {
            RenderSink::Stdout => {
                let mut stdout = std::io::stdout();
                renderer.render(&mut stdout, surface, term_pane, hardware_cursor)
            }
            RenderSink::Registry(registry) => {
                let mut buf: Vec<u8> = Vec::with_capacity(16 * 1024);
                renderer
                    .render(&mut buf, surface, term_pane, hardware_cursor)
                    .and_then(|()| {
                        let frame = String::from_utf8(buf)
                            .map_err(|_| std::io::Error::other("frame was not valid UTF-8"))?;
                        // A gone client is cleaned up by its Detached event;
                        // dropping the frame is fine.
                        registry.send_to(vid, crate::remote::protocol::ServerMsg::Frame(frame));
                        Ok(())
                    })
            }
        };
        match render_result {
            Ok(()) => {
                view.render_failures = 0;
                view.renderer_invalidated = false;
            }
            Err(e) => {
                view.render_failures += 1;
                if view.render_failures <= 3 {
                    view.note = Some((
                        format!("render error: {e}"),
                        StatusNoteKind::Error,
                        Instant::now(),
                    ));
                }
                if view.render_failures >= 10 && !view.renderer_invalidated {
                    // Persistent render failure — the destination is dead.
                    // Invalidate the renderer so the next attempt is a full
                    // repaint rather than a diff against stale state.
                    view.renderer.invalidate();
                    view.renderer_invalidated = true;
                }
            }
        }
    }

    pub(crate) fn chrome(&self, view: &MuxView) -> Chrome {
        let project = self
            .workspace
            .projects
            .get(view.focus.project)
            .map(project_sidebar_title)
            .unwrap_or_default();
        let session = self
            .session_for_focus(&view.focus)
            .map(session_sidebar_title)
            .unwrap_or_default();
        match &view.note {
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
                    .session_for_focus(&view.focus)
                    .map(|s| statusline_status(&s.status))
                    .unwrap_or_default(),
                status_kind: None,
            },
        }
    }
}
