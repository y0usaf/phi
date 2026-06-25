//! Model picker overlay for mux-hosted sessions.

use phi_tui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use phi_tui::fuzzy_filter;
use tokio::sync::oneshot;

use super::dialog::render_dialog_title_line;
use crate::driver::SessionKey;
use phi_mux_grid::cell_surface::{CellSurface, draw_box, render_cell_scrollbar, truncate_to_cells};
use phi_mux_grid::layout::CellRect as Rect;
use crate::ui::scene::ScenePalette;
use phi_mux_grid::theme;

#[derive(Debug)]
pub enum ModelOverlayOutcome {
    None,
    Close,
    Submit(String),
}

#[derive(Debug)]
pub struct ModelOverlayState {
    pub root: bool,
    pub response_tx: Option<oneshot::Sender<std::result::Result<Option<String>, String>>>,
    models: Vec<String>,
    current: String,
    query: String,
    filtered: Vec<usize>,
    selected: usize,
}

impl ModelOverlayState {
    pub fn new(
        _session_key: SessionKey,
        root: bool,
        current: String,
        models: Vec<String>,
        response_tx: oneshot::Sender<std::result::Result<Option<String>, String>>,
    ) -> Self {
        let mut state = Self {
            root,
            response_tx: Some(response_tx),
            models,
            current,
            query: String::new(),
            filtered: Vec::new(),
            selected: 0,
        };
        state.refilter();
        if let Some(pos) = state
            .filtered
            .iter()
            .position(|&i| state.models[i] == state.current)
        {
            state.selected = pos;
        }
        state
    }

    fn refilter(&mut self) {
        self.filtered = fuzzy_filter((0..self.models.len()).collect(), &self.query, |&i| {
            self.models[i].clone()
        });
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }

    pub fn handle_key(&mut self, key: &KeyEvent, cols: u16, rows: u16) -> ModelOverlayOutcome {
        let mods = key.modifiers;
        if mods.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => return ModelOverlayOutcome::Close,
                KeyCode::Char('u') => {
                    self.query.clear();
                    self.refilter();
                }
                _ => {}
            }
            return ModelOverlayOutcome::None;
        }
        if mods.contains(KeyModifiers::SHIFT) {
            if matches!(key.code, KeyCode::Char('G')) {
                self.select_last();
            }
            return ModelOverlayOutcome::None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return ModelOverlayOutcome::Close,
            KeyCode::Enter => {
                if let Some(&index) = self.filtered.get(self.selected) {
                    return ModelOverlayOutcome::Submit(self.models[index].clone());
                }
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.page_selection(-1, visible_rows(cols, rows)),
            KeyCode::PageDown => self.page_selection(1, visible_rows(cols, rows)),
            KeyCode::Home | KeyCode::Char('g') => self.select_first(),
            KeyCode::End => self.select_last(),
            KeyCode::Backspace => {
                self.query.pop();
                self.refilter();
            }
            KeyCode::Char(c) if !mods.contains(KeyModifiers::ALT) => {
                self.query.push(c);
                self.refilter();
            }
            _ => {}
        }
        ModelOverlayOutcome::None
    }

    pub fn move_selection(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            return;
        }
        let max = self.filtered.len().saturating_sub(1) as i32;
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
        self.selected = self.filtered.len().saturating_sub(1);
    }
}

fn model_overlay_rect(cols: i32, rows: i32) -> Rect {
    let cols = cols.max(1);
    let rows = rows.max(1);
    let width = cols.clamp(1, 110);
    let height = rows.clamp(1, 24);
    Rect::new((cols - width) / 2, (rows - height) / 2, width, height)
}

fn visible_rows(cols: u16, rows: u16) -> usize {
    model_overlay_rect(i32::from(cols), i32::from(rows))
        .rows
        .saturating_sub(7) as usize
}

pub fn render_model_overlay(
    surface: &mut CellSurface,
    model: &mut ModelOverlayState,
    palette: &ScenePalette,
) {
    let rect = model_overlay_rect(surface.cols, surface.rows);
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
    let title = if model.root {
        " SWITCH MODEL "
    } else {
        " SWITCH SUB MODEL "
    };
    let info = format!(" {}/{} ", model.filtered.len(), model.models.len());
    render_dialog_title_line(surface, inner.row, inner.col, inner.cols, title, &info, palette);

    let query_row = inner.row + 2;
    let list_row = inner.row + 4;
    let footer_row = rect.row + rect.rows - 2;
    let list_rows = (footer_row - list_row).max(0) as usize;
    let list_width = inner.cols.max(0);

    surface.put_text(
        inner.col,
        query_row,
        inner.cols,
        palette.fg,
        palette.bg,
        &truncate_to_cells(&format!("› {}", model.query), inner.cols.max(0) as usize),
    );

    if model.filtered.is_empty() {
        surface.put_text(
            inner.col,
            list_row,
            list_width,
            palette.muted,
            palette.bg,
            "No matching models.",
        );
    } else {
        let start = model
            .selected
            .saturating_sub(list_rows.saturating_div(2))
            .min(model.filtered.len().saturating_sub(list_rows.max(1)));
        let end = (start + list_rows).min(model.filtered.len());
        for (row_offset, pos) in (start..end).enumerate() {
            let row = list_row + row_offset as i32;
            let index = model.filtered[pos];
            let selected = pos == model.selected;
            let value = &model.models[index];
            let current = *value == model.current;
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
            let mut line = if selected {
                format!("→ {}", pretty_model_name(value))
            } else {
                format!("  {}", pretty_model_name(value))
            };
            if current {
                line.push_str("  ✓");
            }
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
            model.filtered.len(),
            start,
            palette.border,
            palette.bg,
            "╎",
            palette.accent_2,
            "┃",
        );
    }

    let hint = "type to filter · ↑/↓ select · Enter confirm · Esc close";
    surface.put_text(
        inner.col,
        footer_row,
        inner.cols,
        palette.muted,
        palette.bg,
        &truncate_to_cells(hint, inner.cols.max(0) as usize),
    );
}

fn pretty_model_name(value: &str) -> String {
    match value.split_once('/') {
        Some((provider, id)) => format!("{id} [{provider}]"),
        None => value.to_owned(),
    }
}
