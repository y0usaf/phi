//! The `phi:archive` overlay — browse and restore archived sessions
//! (port of the mux's hardwired `overlays/archive.rs`) over
//! `phi_session::archive`.
//!
//! Restore is performed by the overlay itself (`restore_session` is plain
//! session-store persistence); the host only learns "something changed"
//! through the returned [`OverlayOutcome::Complete`] actions.

use std::sync::Arc;
use std::time::SystemTime;

use phi_ext::{
    CommandOutput, DrawContext, DrawRect, OVERLAY_ARCHIVE, OverlayKey, OverlayKeyCode,
    OverlayOutcome, OverlaySpec, OverlayState, UiAction, brighten,
};
use phi_session::archive::{self, ArchivedSession};
use phi_tui::truncate_to_cells;

use super::dialog::render_dialog_title_line;

const ARCHIVE_FOOTER_HINT: &str = "Restores selected archive to its original project cwd.";

struct ArchiveOverlayState {
    sessions: Vec<ArchivedSession>,
    selected: usize,
    scroll: usize,
    note: Option<String>,
}

impl ArchiveOverlayState {
    fn load() -> Self {
        let mut state = Self {
            sessions: archive::list_archived(),
            selected: 0,
            scroll: 0,
            note: None,
        };
        state.clamp_selection();
        state
    }

    fn handle_key(&mut self, key: &OverlayKey, cols: u16, rows: u16) -> OverlayOutcome {
        if key.modifiers.control {
            if matches!(key.code, OverlayKeyCode::Char('c')) {
                return OverlayOutcome::Close;
            }
            return OverlayOutcome::None;
        }
        if key.modifiers.shift {
            if matches!(key.code, OverlayKeyCode::Char('G')) {
                self.select_last();
            }
            return OverlayOutcome::None;
        }
        match key.code {
            OverlayKeyCode::Esc | OverlayKeyCode::Char('q') => return OverlayOutcome::Close,
            OverlayKeyCode::Enter => return self.restore_selected(),
            OverlayKeyCode::Up | OverlayKeyCode::Char('k') => self.move_selection(-1),
            OverlayKeyCode::Down | OverlayKeyCode::Char('j') => self.move_selection(1),
            OverlayKeyCode::PageUp => self.page_selection(-1, visible_rows(cols, rows)),
            OverlayKeyCode::PageDown => self.page_selection(1, visible_rows(cols, rows)),
            OverlayKeyCode::Home | OverlayKeyCode::Char('g') => self.select_first(),
            OverlayKeyCode::End => self.select_last(),
            OverlayKeyCode::Char('r') => {
                self.reload_sessions();
                self.note = Some(format!("{} archived sessions", self.sessions.len()));
            }
            _ => {}
        }
        OverlayOutcome::None
    }

    fn restore_selected(&mut self) -> OverlayOutcome {
        let Some(path) = self.selected_session().map(|s| s.path.clone()) else {
            self.note = Some("no archived sessions".to_string());
            return OverlayOutcome::None;
        };
        match archive::restore_session(&path) {
            Ok(target) => OverlayOutcome::Complete(CommandOutput {
                message: Some(format!("restored {}", target.display())),
                actions: vec![UiAction::RunHostAction {
                    name: "reload".to_owned(),
                }],
            }),
            Err(_error) => {
                // Restore I/O is copy+delete fallback — the archived file is
                // still safely in place. Reload the list silently.
                self.reload_sessions();
                OverlayOutcome::None
            }
        }
    }

    fn reload_sessions(&mut self) {
        let selected_path = self.selected_session().map(|session| session.path.clone());
        self.sessions = archive::list_archived();
        self.selected = selected_path
            .as_deref()
            .and_then(|path| {
                self.sessions
                    .iter()
                    .position(|session| session.path == path)
            })
            .unwrap_or(0);
        self.clamp_selection();
        self.scroll = self.scroll.min(self.sessions.len().saturating_sub(1));
    }

    fn selected_session(&self) -> Option<&ArchivedSession> {
        self.sessions.get(self.selected)
    }

    fn move_selection(&mut self, delta: i32) {
        if self.sessions.is_empty() {
            return;
        }
        let max = self.sessions.len().saturating_sub(1) as i32;
        self.selected = (self.selected as i32 + delta).clamp(0, max) as usize;
    }

    fn page_selection(&mut self, delta_pages: i32, visible_rows: usize) {
        let step = visible_rows.max(1) as i32;
        self.move_selection(delta_pages.saturating_mul(step));
    }

    fn select_first(&mut self) {
        self.selected = 0;
    }

    fn select_last(&mut self) {
        self.selected = self.sessions.len().saturating_sub(1);
    }

    fn ensure_selection_visible(&mut self, visible_rows: usize) {
        if self.sessions.is_empty() || visible_rows == 0 {
            self.scroll = 0;
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + visible_rows {
            self.scroll = self.selected.saturating_sub(visible_rows.saturating_sub(1));
        }
        self.scroll = self
            .scroll
            .min(self.sessions.len().saturating_sub(visible_rows));
    }

    fn clamp_selection(&mut self) {
        if self.sessions.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.sessions.len() - 1);
        }
    }
}

pub(super) fn spec() -> OverlaySpec {
    OverlaySpec::new(
        OVERLAY_ARCHIVE,
        "Browse and restore archived sessions",
        Arc::new(|_payload| Box::new(ArchiveOverlayState::load()) as OverlayState),
        Arc::new(render),
        Arc::new(
            |state: &mut OverlayState, key: &OverlayKey, cols, rows| match state
                .downcast_mut::<ArchiveOverlayState>()
            {
                Some(viewer) => viewer.handle_key(key, cols, rows),
                None => OverlayOutcome::Close,
            },
        ),
    )
    .with_scroll(Arc::new(|state: &mut OverlayState, delta, _cols, _rows| {
        if let Some(viewer) = state.downcast_mut::<ArchiveOverlayState>() {
            viewer.move_selection(delta);
        }
    }))
}

fn archive_viewer_rect(cols: i32, rows: i32) -> DrawRect {
    let cols = cols.max(1);
    let rows = rows.max(1);
    let width = cols.clamp(1, 110);
    let height = rows.clamp(1, 32);
    DrawRect::new((cols - width) / 2, (rows - height) / 2, width, height)
}

fn visible_rows(cols: u16, rows: u16) -> usize {
    archive_viewer_rect(i32::from(cols), i32::from(rows))
        .rows
        .saturating_sub(7) as usize
}

fn render(ctx: &mut dyn DrawContext, state: &mut OverlayState) {
    let Some(viewer) = state.downcast_mut::<ArchiveOverlayState>() else {
        return;
    };
    let palette = ctx.palette();
    let size = ctx.size();
    let rect = archive_viewer_rect(size.cols, size.rows);
    ctx.draw_box(
        rect,
        palette.fg,
        palette.surface_raised,
        palette.statusbar_bg,
    );
    if rect.cols <= 2 || rect.rows <= 2 {
        return;
    }

    let inner = DrawRect::new(
        rect.col + 2,
        rect.row + 1,
        (rect.cols - 4).max(0),
        (rect.rows - 2).max(0),
    );
    ctx.fill_rect(inner, palette.fg, palette.bg);
    let count = format!(" {} archived ", viewer.sessions.len());
    render_dialog_title_line(
        ctx,
        inner.row,
        inner.col,
        inner.cols,
        " ARCHIVE ",
        &count,
        &palette,
    );

    let hint = "↑/↓/j/k select  Enter restore  r reload  q/Esc close";
    ctx.put_text(
        inner.col,
        inner.row + 2,
        inner.cols,
        hint,
        palette.muted,
        palette.bg,
    );
    let header_row = inner.row + 3;
    ctx.put_text(
        inner.col,
        header_row,
        inner.cols,
        "UPDATED  TOKENS     COST SESSION / PROJECT",
        brighten(palette.muted, 24),
        palette.bg,
    );
    let list_row = inner.row + 4;
    let footer_row = rect.row + rect.rows - 2;
    let list_rows = (footer_row - list_row).max(0) as usize;
    let list_width = (inner.cols - 1).max(0);
    viewer.ensure_selection_visible(list_rows);

    if viewer.sessions.is_empty() {
        ctx.put_text(
            inner.col,
            list_row,
            list_width,
            "No archived sessions. Ctrl+Delete archives the selected session.",
            palette.muted,
            palette.bg,
        );
    } else {
        let now = SystemTime::now();
        let end = (viewer.scroll + list_rows).min(viewer.sessions.len());
        for (row_offset, index) in (viewer.scroll..end).enumerate() {
            let row = list_row + row_offset as i32;
            let selected = index == viewer.selected;
            let line = archive_viewer_row_text(&viewer.sessions[index], now);
            let row_rect = DrawRect::new(inner.col, row, list_width, 1);
            let row_bg = if selected {
                palette.surface_raised
            } else {
                palette.bg
            };
            let row_fg = if selected {
                palette.fg
            } else {
                brighten(palette.muted, 30)
            };
            ctx.fill_rect(row_rect, row_fg, row_bg);
            ctx.put_text(
                inner.col,
                row,
                list_width,
                &truncate_to_cells(&line, list_width as usize),
                row_fg,
                row_bg,
            );
        }
        ctx.render_scrollbar(
            inner.col + inner.cols - 1,
            list_row,
            list_rows as i32,
            list_rows,
            viewer.sessions.len(),
            viewer.scroll,
            palette.border,
            palette.bg,
            "╎",
            palette.accent_2,
            "┃",
        );
    }

    let footer = viewer.note.as_deref().unwrap_or(ARCHIVE_FOOTER_HINT);
    ctx.put_text(
        inner.col,
        footer_row,
        inner.cols,
        &truncate_to_cells(footer, inner.cols.max(0) as usize),
        palette.muted,
        palette.bg,
    );
}

fn archive_viewer_row_text(session: &ArchivedSession, now: SystemTime) -> String {
    let name = if session.first_message.is_empty() {
        "(empty session)".to_string()
    } else {
        session.first_message.replace(['\n', '\r'], " ")
    };
    format!(
        "{:<7} {:>7} {:>8} {}  {}",
        archive_age_label(now, session.modified),
        archive_tokens_label(session.usage.total_tokens()),
        archive_cost_label(session.usage.total_cost),
        truncate_to_cells(&name, 48),
        session.cwd.display()
    )
}

fn archive_tokens_label(total_tokens: u64) -> String {
    if total_tokens == 0 {
        "-".to_string()
    } else if total_tokens >= 1_000_000 {
        format!("{:.1}M", total_tokens as f64 / 1e6)
    } else if total_tokens >= 1_000 {
        format!("{:.1}k", total_tokens as f64 / 1e3)
    } else {
        total_tokens.to_string()
    }
}

fn archive_cost_label(total_cost: f64) -> String {
    if total_cost <= 0.0 {
        "-".to_string()
    } else {
        format!("${total_cost:.2}")
    }
}

fn archive_age_label(now: SystemTime, modified: Option<SystemTime>) -> String {
    let Some(modified) = modified else {
        return "unknown".to_string();
    };
    let age_secs = now
        .duration_since(modified)
        .map(|age| age.as_secs())
        .unwrap_or(0);
    if age_secs < 60 {
        "now".to_string()
    } else if age_secs < 60 * 60 {
        format!("{}m ago", age_secs / 60)
    } else if age_secs < 60 * 60 * 24 {
        format!("{}h ago", age_secs / (60 * 60))
    } else if age_secs < 60 * 60 * 24 * 30 {
        format!("{}d ago", age_secs / (60 * 60 * 24))
    } else if age_secs < 60 * 60 * 24 * 365 {
        format!("{}mo ago", age_secs / (60 * 60 * 24 * 30))
    } else {
        format!("{}y ago", age_secs / (60 * 60 * 24 * 365))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_host::registered_overlays;
    use phi_ext::{
        OVERLAY_ARCHIVE, OverlayKey, OverlayKeyCode, OverlayKeyModifiers, OverlayOutcome,
    };

    #[test]
    fn archive_overlay_closes_on_esc_and_q() {
        let overlays = registered_overlays();
        let overlay = overlays.get(OVERLAY_ARCHIVE).unwrap();
        for code in [OverlayKeyCode::Esc, OverlayKeyCode::Char('q')] {
            let mut state = (overlay.init_state)(None);
            let key = OverlayKey {
                code,
                modifiers: OverlayKeyModifiers::default(),
            };
            assert_eq!(
                (overlay.handle_key)(&mut state, &key, 120, 40),
                OverlayOutcome::Close
            );
        }
    }

    #[test]
    fn archive_overlay_navigation_keys_do_not_close() {
        let overlays = registered_overlays();
        let overlay = overlays.get(OVERLAY_ARCHIVE).unwrap();
        let mut state = (overlay.init_state)(None);
        for code in [
            OverlayKeyCode::Up,
            OverlayKeyCode::Down,
            OverlayKeyCode::PageDown,
            OverlayKeyCode::Home,
            OverlayKeyCode::End,
        ] {
            let key = OverlayKey {
                code,
                modifiers: OverlayKeyModifiers::default(),
            };
            assert_eq!(
                (overlay.handle_key)(&mut state, &key, 120, 40),
                OverlayOutcome::None
            );
        }
    }

    #[test]
    fn archive_overlay_has_scroll_handler() {
        let overlays = registered_overlays();
        let overlay = overlays.get(OVERLAY_ARCHIVE).unwrap();
        let scroll = overlay.handle_scroll.as_ref().expect("scroll handler");
        let mut state = (overlay.init_state)(None);
        scroll(&mut state, 3, 120, 40);
    }
}
