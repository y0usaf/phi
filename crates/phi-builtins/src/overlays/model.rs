//! The `phi:model` overlay — fuzzy model picker (port of the mux's
//! hardwired `overlays/model.rs`).
//!
//! Opened programmatically with a [`ModelPickerPayload`] carrying the
//! candidate list, the current selection, and an [`OverlayResponder`] the
//! picked value is sent through. The responder fires `Ok(None)` on drop,
//! so a dismissed or replaced picker never leaves the requester hanging.

use std::sync::Arc;

use phi_ext::{
    DrawContext, DrawRect, ModelPickerPayload, OVERLAY_MODEL, OverlayKey, OverlayKeyCode,
    OverlayOutcome, OverlayResponder, OverlaySpec, OverlayState, brighten,
};
use phi_tui::{fuzzy_filter, truncate_to_cells};

use super::dialog::render_dialog_title_line;

struct ModelOverlayState {
    root: bool,
    responder: OverlayResponder,
    models: Vec<String>,
    current: String,
    query: String,
    filtered: Vec<usize>,
    selected: usize,
}

impl ModelOverlayState {
    fn new(payload: ModelPickerPayload) -> Self {
        let mut state = Self {
            root: payload.root,
            responder: payload.responder,
            models: payload.models,
            current: payload.current,
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

    fn handle_key(&mut self, key: &OverlayKey, cols: u16, rows: u16) -> OverlayOutcome {
        if key.modifiers.control {
            match key.code {
                OverlayKeyCode::Char('c') => {
                    self.responder.respond(Ok(None));
                    return OverlayOutcome::Close;
                }
                OverlayKeyCode::Char('u') => {
                    self.query.clear();
                    self.refilter();
                }
                _ => {}
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
            OverlayKeyCode::Esc | OverlayKeyCode::Char('q') => {
                self.responder.respond(Ok(None));
                return OverlayOutcome::Close;
            }
            OverlayKeyCode::Enter => {
                if let Some(&index) = self.filtered.get(self.selected) {
                    self.responder.respond(Ok(Some(self.models[index].clone())));
                    return OverlayOutcome::Close;
                }
            }
            OverlayKeyCode::Up | OverlayKeyCode::Char('k') | OverlayKeyCode::BackTab => {
                self.move_selection(-1)
            }
            OverlayKeyCode::Down | OverlayKeyCode::Char('j') => self.move_selection(1),
            OverlayKeyCode::PageUp => self.page_selection(-1, visible_rows(cols, rows)),
            OverlayKeyCode::PageDown => self.page_selection(1, visible_rows(cols, rows)),
            OverlayKeyCode::Home | OverlayKeyCode::Char('g') => self.select_first(),
            OverlayKeyCode::End => self.select_last(),
            OverlayKeyCode::Backspace => {
                self.query.pop();
                self.refilter();
            }
            OverlayKeyCode::Char(c) if !key.modifiers.alt => {
                self.query.push(c);
                self.refilter();
            }
            _ => {}
        }
        OverlayOutcome::None
    }

    fn move_selection(&mut self, delta: i32) {
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

pub(super) fn spec() -> OverlaySpec {
    OverlaySpec::new(
        OVERLAY_MODEL,
        "Switch the session's model",
        Arc::new(|payload| {
            let payload = payload
                .and_then(|p| p.downcast::<ModelPickerPayload>().ok())
                .map(|p| *p)
                // Opened without a payload (nothing to pick or respond to):
                // an empty picker that only closes.
                .unwrap_or_else(|| ModelPickerPayload {
                    root: true,
                    current: String::new(),
                    models: Vec::new(),
                    responder: OverlayResponder::new(|_| {}),
                });
            Box::new(ModelOverlayState::new(payload)) as OverlayState
        }),
        Arc::new(render),
        Arc::new(
            |state: &mut OverlayState, key: &OverlayKey, cols, rows| match state
                .downcast_mut::<ModelOverlayState>()
            {
                Some(model) => model.handle_key(key, cols, rows),
                None => OverlayOutcome::Close,
            },
        ),
    )
    .with_scroll(Arc::new(|state: &mut OverlayState, delta, _cols, _rows| {
        if let Some(model) = state.downcast_mut::<ModelOverlayState>() {
            model.move_selection(delta);
        }
    }))
}

fn model_overlay_rect(cols: i32, rows: i32) -> DrawRect {
    let cols = cols.max(1);
    let rows = rows.max(1);
    let width = cols.clamp(1, 110);
    let height = rows.clamp(1, 24);
    DrawRect::new((cols - width) / 2, (rows - height) / 2, width, height)
}

fn visible_rows(cols: u16, rows: u16) -> usize {
    model_overlay_rect(i32::from(cols), i32::from(rows))
        .rows
        .saturating_sub(7) as usize
}

fn render(ctx: &mut dyn DrawContext, state: &mut OverlayState) {
    let Some(model) = state.downcast_mut::<ModelOverlayState>() else {
        return;
    };
    let palette = ctx.palette();
    let size = ctx.size();
    let rect = model_overlay_rect(size.cols, size.rows);
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
    let title = if model.root {
        " SWITCH MODEL "
    } else {
        " SWITCH SUB MODEL "
    };
    let info = format!(" {}/{} ", model.filtered.len(), model.models.len());
    render_dialog_title_line(
        ctx, inner.row, inner.col, inner.cols, title, &info, &palette,
    );

    let query_row = inner.row + 2;
    let list_row = inner.row + 4;
    let footer_row = rect.row + rect.rows - 2;
    let list_rows = (footer_row - list_row).max(0) as usize;
    let list_width = inner.cols.max(0);

    ctx.put_text(
        inner.col,
        query_row,
        inner.cols,
        &truncate_to_cells(&format!("› {}", model.query), inner.cols.max(0) as usize),
        palette.fg,
        palette.bg,
    );

    if model.filtered.is_empty() {
        ctx.put_text(
            inner.col,
            list_row,
            list_width,
            "No matching models.",
            palette.muted,
            palette.bg,
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
            let mut line = if selected {
                format!("→ {}", pretty_model_name(value))
            } else {
                format!("  {}", pretty_model_name(value))
            };
            if current {
                line.push_str("  ✓");
            }
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
    ctx.put_text(
        inner.col,
        footer_row,
        inner.cols,
        &truncate_to_cells(hint, inner.cols.max(0) as usize),
        palette.muted,
        palette.bg,
    );
}

fn pretty_model_name(value: &str) -> String {
    match value.split_once('/') {
        Some((provider, id)) => format!("{id} [{provider}]"),
        None => value.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_host::registered_overlays;
    use phi_ext::{
        ModelPickerPayload, OVERLAY_MODEL, OverlayKey, OverlayKeyCode, OverlayKeyModifiers,
        OverlayOutcome, OverlayResponder,
    };
    use std::sync::{Arc, Mutex};

    type Response = std::result::Result<Option<String>, String>;

    fn payload_with_capture(
        models: Vec<&str>,
        current: &str,
    ) -> (ModelPickerPayload, Arc<Mutex<Option<Response>>>) {
        let captured = Arc::new(Mutex::new(None));
        let capture = Arc::clone(&captured);
        let payload = ModelPickerPayload {
            root: true,
            current: current.to_owned(),
            models: models.into_iter().map(str::to_owned).collect(),
            responder: OverlayResponder::new(move |result| {
                *capture.lock().unwrap() = Some(result);
            }),
        };
        (payload, captured)
    }

    fn key(code: OverlayKeyCode) -> OverlayKey {
        OverlayKey {
            code,
            modifiers: OverlayKeyModifiers::default(),
        }
    }

    #[test]
    fn enter_submits_selected_model_through_responder() {
        let overlays = registered_overlays();
        let overlay = overlays.get(OVERLAY_MODEL).unwrap();
        let (payload, captured) =
            payload_with_capture(vec!["anthropic/claude", "openai/gpt"], "anthropic/claude");
        let mut state = (overlay.init_state)(Some(Box::new(payload)));

        let outcome = (overlay.handle_key)(&mut state, &key(OverlayKeyCode::Enter), 120, 40);
        assert_eq!(outcome, OverlayOutcome::Close);
        assert_eq!(
            captured.lock().unwrap().clone(),
            Some(Ok(Some("anthropic/claude".to_owned())))
        );
    }

    #[test]
    fn esc_responds_none() {
        let overlays = registered_overlays();
        let overlay = overlays.get(OVERLAY_MODEL).unwrap();
        let (payload, captured) = payload_with_capture(vec!["a/b"], "a/b");
        let mut state = (overlay.init_state)(Some(Box::new(payload)));

        let outcome = (overlay.handle_key)(&mut state, &key(OverlayKeyCode::Esc), 120, 40);
        assert_eq!(outcome, OverlayOutcome::Close);
        assert_eq!(captured.lock().unwrap().clone(), Some(Ok(None)));
    }

    #[test]
    fn dropping_state_responds_none() {
        let overlays = registered_overlays();
        let overlay = overlays.get(OVERLAY_MODEL).unwrap();
        let (payload, captured) = payload_with_capture(vec!["a/b"], "a/b");
        let state = (overlay.init_state)(Some(Box::new(payload)));

        drop(state);
        assert_eq!(captured.lock().unwrap().clone(), Some(Ok(None)));
    }

    #[test]
    fn typing_filters_candidates() {
        let overlays = registered_overlays();
        let overlay = overlays.get(OVERLAY_MODEL).unwrap();
        let (payload, captured) =
            payload_with_capture(vec!["anthropic/claude", "openai/gpt"], "openai/gpt");
        let mut state = (overlay.init_state)(Some(Box::new(payload)));

        for c in "gpt".chars() {
            (overlay.handle_key)(&mut state, &key(OverlayKeyCode::Char(c)), 120, 40);
        }
        let outcome = (overlay.handle_key)(&mut state, &key(OverlayKeyCode::Enter), 120, 40);
        assert_eq!(outcome, OverlayOutcome::Close);
        assert_eq!(
            captured.lock().unwrap().clone(),
            Some(Ok(Some("openai/gpt".to_owned())))
        );
    }

    #[test]
    fn opens_without_payload_and_closes() {
        let overlays = registered_overlays();
        let overlay = overlays.get(OVERLAY_MODEL).unwrap();
        let mut state = (overlay.init_state)(None);
        let outcome = (overlay.handle_key)(&mut state, &key(OverlayKeyCode::Esc), 120, 40);
        assert_eq!(outcome, OverlayOutcome::Close);
    }
}
