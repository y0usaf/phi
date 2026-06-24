//! Archive browser overlay (port of pi-harness `overlays/archive.rs`) over
//! phi's `~/.local/share/phi/sessions/ARCHIVE` scan.

use std::time::SystemTime;

use phi_tui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::dialog::render_dialog_title_line;
use crate::ui::cell_surface::{CellSurface, draw_box, render_cell_scrollbar, truncate_to_cells};
use crate::ui::layout::CellRect as Rect;
use crate::ui::scene::ScenePalette;
use crate::ui::theme;
use crate::workspace::{self, ArchivedSession};

/// What the mux loop should do after the archive viewer consumed a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArchiveOutcome {
    None,
    Close,
    /// Restore the archived session at this path.
    Restore(std::path::PathBuf),
}

#[derive(Debug)]
pub struct ArchiveViewerState {
    pub sessions: Vec<ArchivedSession>,
    pub selected: usize,
    pub scroll: usize,
    pub note: Option<String>,
}

impl ArchiveViewerState {
    pub fn load() -> Self {
        let mut viewer = Self {
            sessions: workspace::list_archived(),
            selected: 0,
            scroll: 0,
            note: None,
        };
        viewer.clamp_selection();
        viewer
    }

    pub fn handle_key(&mut self, key: &KeyEvent, cols: u16, rows: u16) -> ArchiveOutcome {
        let mods = key.modifiers;
        if mods.contains(KeyModifiers::CONTROL) {
            if matches!(key.code, KeyCode::Char('c')) {
                return ArchiveOutcome::Close;
            }
            return ArchiveOutcome::None;
        }
        if mods.contains(KeyModifiers::SHIFT) {
            if matches!(key.code, KeyCode::Char('G')) {
                self.select_last();
            }
            return ArchiveOutcome::None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return ArchiveOutcome::Close,
            KeyCode::Enter => {
                if let Some(session) = self.selected_session() {
                    return ArchiveOutcome::Restore(session.path.clone());
                }
                self.note = Some("no archived sessions".to_string());
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.page_selection(-1, visible_rows(cols, rows)),
            KeyCode::PageDown => self.page_selection(1, visible_rows(cols, rows)),
            KeyCode::Home | KeyCode::Char('g') => self.select_first(),
            KeyCode::End => self.select_last(),
            KeyCode::Char('r') => {
                self.reload_sessions();
                self.note = Some(format!("{} archived sessions", self.sessions.len()));
            }
            _ => {}
        }
        ArchiveOutcome::None
    }

    pub fn reload_sessions(&mut self) {
        let selected_path = self.selected_session().map(|session| session.path.clone());
        self.sessions = workspace::list_archived();
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

    pub fn selected_session(&self) -> Option<&ArchivedSession> {
        self.sessions.get(self.selected)
    }

    pub fn move_selection(&mut self, delta: i32) {
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

fn archive_viewer_rect(cols: i32, rows: i32) -> Rect {
    let cols = cols.max(1);
    let rows = rows.max(1);
    let width = cols.clamp(1, 110);
    let height = rows.clamp(1, 32);
    Rect::new((cols - width) / 2, (rows - height) / 2, width, height)
}

fn visible_rows(cols: u16, rows: u16) -> usize {
    archive_viewer_rect(i32::from(cols), i32::from(rows))
        .rows
        .saturating_sub(7) as usize
}

pub fn render_archive_viewer(
    surface: &mut CellSurface,
    viewer: &mut ArchiveViewerState,
    palette: &ScenePalette,
) {
    let rect = archive_viewer_rect(surface.cols, surface.rows);
    draw_box(
        surface,
        rect,
        palette.fg,
        palette.surface_raised,
        palette.statusbar_bg,
    );
    if rect.cols <= 2 || rect.rows <= 2 {
        return;
    }

    let inner = rect.inset_edges(2, 1, 2, 1);
    surface.fill_rect(inner, palette.fg, palette.bg);
    let count = format!(" {} archived ", viewer.sessions.len());
    render_dialog_title_line(
        surface,
        inner.row,
        inner.col,
        inner.cols,
        " ARCHIVE ",
        &count,
        palette,
    );

    let hint = "↑/↓/j/k select  Enter restore  r reload  q/Esc close";
    surface.put_text(
        inner.col,
        inner.row + 2,
        inner.cols,
        palette.muted,
        palette.bg,
        hint,
    );
    let header_row = inner.row + 3;
    surface.put_text(
        inner.col,
        header_row,
        inner.cols,
        theme::brighten(palette.muted, 24),
        palette.bg,
        "UPDATED  SESSION / PROJECT",
    );
    let list_row = inner.row + 4;
    let footer_row = rect.row + rect.rows - 2;
    let list_rows = (footer_row - list_row).max(0) as usize;
    let list_width = (inner.cols - 1).max(0);
    viewer.ensure_selection_visible(list_rows);

    if viewer.sessions.is_empty() {
        surface.put_text(
            inner.col,
            list_row,
            list_width,
            palette.muted,
            palette.bg,
            "No archived sessions. Ctrl+Delete archives the selected session.",
        );
    } else {
        let now = SystemTime::now();
        let end = (viewer.scroll + list_rows).min(viewer.sessions.len());
        for (row_offset, index) in (viewer.scroll..end).enumerate() {
            let row = list_row + row_offset as i32;
            let selected = index == viewer.selected;
            let line = archive_viewer_row_text(&viewer.sessions[index], now);
            let row_rect = Rect::new(inner.col, row, list_width, 1);
            let row_bg = if selected {
                palette.surface_raised
            } else {
                palette.bg
            };
            let row_fg = if selected {
                palette.fg
            } else {
                theme::brighten(palette.muted, 30)
            };
            surface.fill_rect(row_rect, row_fg, row_bg);
            surface.put_text(
                inner.col,
                row,
                list_width,
                row_fg,
                row_bg,
                &truncate_to_cells(&line, list_width as usize),
            );
        }
        render_cell_scrollbar(
            surface,
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

    let footer = viewer
        .note
        .as_deref()
        .unwrap_or("Restores selected archive to its original project cwd.");
    surface.put_text(
        inner.col,
        footer_row,
        inner.cols,
        palette.muted,
        palette.bg,
        &truncate_to_cells(footer, inner.cols.max(0) as usize),
    );
}

fn archive_viewer_row_text(session: &ArchivedSession, now: SystemTime) -> String {
    let name = if session.first_message.is_empty() {
        "(empty session)".to_string()
    } else {
        session.first_message.replace(['\n', '\r'], " ")
    };
    format!(
        "{:<7} {}  {}",
        archive_age_label(now, session.modified),
        truncate_to_cells(&name, 48),
        session.cwd.display()
    )
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
