//! The `phi:help` overlay — reference Rust implementation.
//!
//! The live default is now `crates/phi-builtins/defaults/help_overlay.lua`;
//! this module stays as a test fixture and documentation of the Rust overlay
//! pattern (draw-context rendering, typed payloads, scroll math). Kept under
//! `#[allow(dead_code)]` because the overlay is no longer registered by
//! `DefaultOverlayExtension`.
//!
//! The host supplies the content as a [`HelpPayload`] built at open time,
//! so live keymap overrides are reflected; the overlay owns scrolling and
//! rendering.
#![allow(dead_code)]

use std::sync::Arc;

use phi_ext::{
    DrawContext, DrawRect, HelpEntry, HelpPayload, OVERLAY_HELP, OverlayKey, OverlayKeyCode,
    OverlayOutcome, OverlaySpec, OverlayState, brighten,
};
use phi_tui::truncate_to_cells;

use super::dialog::render_dialog_title_line;

struct HelpOverlayState {
    entries: Vec<HelpEntry>,
    scroll: usize,
}

impl HelpOverlayState {
    fn handle_key(&mut self, key: &OverlayKey, cols: u16, rows: u16) -> OverlayOutcome {
        if key.modifiers.control {
            if matches!(
                key.code,
                OverlayKeyCode::Char('c') | OverlayKeyCode::Char('g')
            ) {
                return OverlayOutcome::Close;
            }
            return OverlayOutcome::None;
        }
        if key.modifiers.shift {
            if matches!(key.code, OverlayKeyCode::Char('G')) {
                self.set_scroll(usize::MAX, cols, rows);
            }
            return OverlayOutcome::None;
        }
        match key.code {
            OverlayKeyCode::Esc | OverlayKeyCode::Char('q') => return OverlayOutcome::Close,
            OverlayKeyCode::Up | OverlayKeyCode::Char('k') => self.scroll_by(-1, cols, rows),
            OverlayKeyCode::Down | OverlayKeyCode::Char('j') => self.scroll_by(1, cols, rows),
            OverlayKeyCode::PageUp => self.page(-1, cols, rows),
            OverlayKeyCode::PageDown => self.page(1, cols, rows),
            OverlayKeyCode::Home | OverlayKeyCode::Char('g') => self.set_scroll(0, cols, rows),
            OverlayKeyCode::End => self.set_scroll(usize::MAX, cols, rows),
            _ => {}
        }
        OverlayOutcome::None
    }

    fn scroll_by(&mut self, delta: i32, cols: u16, rows: u16) {
        let next = if delta < 0 {
            self.scroll.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            self.scroll.saturating_add(delta as usize)
        };
        self.set_scroll(next, cols, rows);
    }

    fn page(&mut self, delta_pages: i32, cols: u16, rows: u16) {
        let visible = visible_rows(cols, rows).max(1);
        self.scroll_by(delta_pages.saturating_mul(visible as i32), cols, rows);
    }

    fn set_scroll(&mut self, scroll: usize, cols: u16, rows: u16) {
        let visible = visible_rows(cols, rows).max(1);
        self.scroll = scroll.min(self.entries.len().saturating_sub(visible));
    }
}

pub(super) fn spec() -> OverlaySpec {
    OverlaySpec::new(
        OVERLAY_HELP,
        "Keybinding and command reference",
        Arc::new(|payload| {
            let entries = payload
                .and_then(|p| p.downcast::<HelpPayload>().ok())
                .map(|p| p.entries)
                .unwrap_or_else(|| vec![HelpEntry::Heading("No help available".to_owned())]);
            Box::new(HelpOverlayState { entries, scroll: 0 }) as OverlayState
        }),
        Arc::new(render),
        Arc::new(|state: &mut OverlayState, key: &OverlayKey, cols, rows| {
            match state.downcast_mut::<HelpOverlayState>() {
                Some(help) => help.handle_key(key, cols, rows),
                None => OverlayOutcome::Close,
            }
        }),
    )
    .with_scroll(Arc::new(|state: &mut OverlayState, delta, cols, rows| {
        if let Some(help) = state.downcast_mut::<HelpOverlayState>() {
            help.scroll_by(delta, cols, rows);
        }
    }))
}

fn help_overlay_rect(cols: i32, rows: i32) -> DrawRect {
    let cols = cols.max(1);
    let rows = rows.max(1);
    let width = cols.clamp(1, 88);
    let height = rows.clamp(1, 20);
    DrawRect::new((cols - width) / 2, (rows - height) / 2, width, height)
}

fn visible_rows(cols: u16, rows: u16) -> usize {
    let inner = help_overlay_rect(i32::from(cols), i32::from(rows)).inset(2, 1);
    inner.rows.saturating_sub(2).max(0) as usize
}

fn render(ctx: &mut dyn DrawContext, state: &mut OverlayState) {
    let Some(help) = state.downcast_mut::<HelpOverlayState>() else {
        return;
    };
    let palette = ctx.palette();
    let size = ctx.size();
    let rect = help_overlay_rect(size.cols, size.rows);
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
    render_dialog_title_line(
        ctx,
        inner.row,
        inner.col,
        inner.cols,
        " HELP ",
        " ctrl+g/Esc/q close ",
        &palette,
    );

    let list_row = inner.row + 2;
    let list_rows = (inner.row + inner.rows - list_row).max(0) as usize;
    if list_rows == 0 {
        return;
    }
    help.scroll = help
        .scroll
        .min(help.entries.len().saturating_sub(list_rows));
    let text_width = (inner.cols - 1).max(0);
    let entries = help.entries.iter().skip(help.scroll).take(list_rows);
    for (row_offset, entry) in entries.enumerate() {
        let row = list_row + row_offset as i32;
        match entry {
            HelpEntry::Blank => {}
            HelpEntry::Heading(heading) => {
                render_dialog_title_line(ctx, row, inner.col, text_width, heading, "", &palette);
            }
            HelpEntry::Item { keys, description } => {
                ctx.put_text(
                    inner.col,
                    row,
                    24.min(text_width),
                    keys,
                    palette.accent_2,
                    palette.bg,
                );
                ctx.put_text(
                    inner.col + 25,
                    row,
                    text_width.saturating_sub(25),
                    &truncate_to_cells(description, text_width.saturating_sub(25).max(0) as usize),
                    brighten(palette.muted, 44),
                    palette.bg,
                );
            }
        }
    }
    ctx.render_scrollbar(
        inner.col + inner.cols - 1,
        list_row,
        list_rows as i32,
        list_rows,
        help.entries.len(),
        help.scroll,
        palette.border,
        palette.bg,
        "╎",
        palette.accent_2,
        "┃",
    );
}

#[cfg(test)]
mod tests {
    use super::spec;
    use phi_ext::{
        HelpEntry, HelpPayload, OverlayKey, OverlayKeyCode, OverlayKeyModifiers, OverlayOutcome,
    };

    fn payload() -> Box<HelpPayload> {
        Box::new(HelpPayload {
            entries: vec![
                HelpEntry::Heading("Global".to_owned()),
                HelpEntry::Item {
                    keys: "ctrl+g".to_owned(),
                    description: "toggle help".to_owned(),
                },
                HelpEntry::Blank,
            ],
        })
    }

    #[test]
    fn help_overlay_closes_on_esc_q_and_ctrl_g() {
        let overlay = spec();

        for (code, control) in [
            (OverlayKeyCode::Esc, false),
            (OverlayKeyCode::Char('q'), false),
            (OverlayKeyCode::Char('g'), true),
        ] {
            let mut state = (overlay.init_state)(Some(payload()));
            let key = OverlayKey {
                code,
                modifiers: OverlayKeyModifiers {
                    control,
                    ..Default::default()
                },
            };
            assert_eq!(
                (overlay.handle_key)(&mut state, &key, 100, 30),
                OverlayOutcome::Close
            );
        }
    }

    #[test]
    fn help_overlay_opens_without_payload() {
        let overlay = spec();
        let mut state = (overlay.init_state)(None);
        let key = OverlayKey {
            code: OverlayKeyCode::Down,
            modifiers: OverlayKeyModifiers::default(),
        };
        assert_eq!(
            (overlay.handle_key)(&mut state, &key, 100, 30),
            OverlayOutcome::None
        );
    }
}
