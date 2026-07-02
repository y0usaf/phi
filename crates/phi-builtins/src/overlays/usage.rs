//! The `phi:usage` overlay — usage totals table over phi's session-JSONL
//! usage scanner (port of the mux's hardwired `overlays/usage.rs`).

use std::sync::Arc;

use phi_ext::{
    DrawContext, DrawRect, OVERLAY_USAGE, OverlayKey, OverlayKeyCode, OverlayOutcome, OverlaySpec,
    OverlayState, brighten,
};
use phi_session::usage::{
    self, UsageReport, is_usage_separator_line, is_usage_total_line, usage_header_line,
    usage_overlay_line_count, usage_overlay_lines, usage_separator_line, usage_total_line,
};
use phi_tui::truncate_to_cells;

use super::dialog::render_dialog_title_line;

const USAGE_FOOTER_HINT: &str = "\u{2191}/\u{2193}/j/k \u{b7} r reload \u{b7} q/Esc";

struct UsageOverlayState {
    report: UsageReport,
    scroll: usize,
}

impl UsageOverlayState {
    fn load() -> Self {
        Self {
            report: usage::load_usage_report(),
            scroll: 0,
        }
    }

    fn reload(&mut self) {
        self.report = usage::load_usage_report();
        self.scroll = 0;
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
            OverlayKeyCode::Char('r') => self.reload(),
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
        self.scroll = scroll.min(usage_overlay_line_count(&self.report).saturating_sub(visible));
    }

    fn clamp_scroll(&mut self, visible_rows: usize) {
        let lines = usage_overlay_line_count(&self.report);
        self.scroll = self.scroll.min(lines.saturating_sub(visible_rows));
    }
}

pub(super) fn spec() -> OverlaySpec {
    OverlaySpec::new(
        OVERLAY_USAGE,
        "Usage totals across all sessions",
        Arc::new(|_payload| Box::new(UsageOverlayState::load()) as OverlayState),
        Arc::new(render),
        Arc::new(
            |state: &mut OverlayState, key: &OverlayKey, cols, rows| match state
                .downcast_mut::<UsageOverlayState>()
            {
                Some(usage) => usage.handle_key(key, cols, rows),
                None => OverlayOutcome::Close,
            },
        ),
    )
    .with_scroll(Arc::new(|state: &mut OverlayState, delta, cols, rows| {
        if let Some(usage) = state.downcast_mut::<UsageOverlayState>() {
            usage.scroll_by(delta, cols, rows);
        }
    }))
}

fn usage_overlay_rect(cols: i32, rows: i32) -> DrawRect {
    let cols = cols.max(1);
    let rows = rows.max(1);
    let width = cols.clamp(1, 118);
    let height = rows.clamp(1, 34);
    DrawRect::new((cols - width) / 2, (rows - height) / 2, width, height)
}

fn visible_rows(cols: u16, rows: u16) -> usize {
    usage_overlay_rect(i32::from(cols), i32::from(rows))
        .rows
        .saturating_sub(8) as usize
}

fn render(ctx: &mut dyn DrawContext, state: &mut OverlayState) {
    let Some(usage) = state.downcast_mut::<UsageOverlayState>() else {
        return;
    };
    let palette = ctx.palette();
    let size = ctx.size();
    let rect = usage_overlay_rect(size.cols, size.rows);
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

    let count = format!(" {} ", usage.report.entries);
    render_dialog_title_line(
        ctx, inner.row, inner.col, inner.cols, " USAGE ", &count, &palette,
    );

    let header_row = inner.row + 1;
    let header_separator_row = inner.row + 2;
    let total_row = inner.row + 3;
    let total_separator_row = inner.row + 4;
    let list_row = inner.row + 5;
    let footer_row = inner.row + inner.rows - 1;
    let list_rows = (footer_row - list_row).max(0) as usize;
    let list_width = (inner.cols - 1).max(0);

    let header_line = usage_header_line();
    ctx.put_text(
        inner.col,
        header_row,
        inner.cols,
        &header_line,
        brighten(palette.muted, 24),
        palette.bg,
    );

    let separator_line = usage_separator_line();
    ctx.put_text(
        inner.col,
        header_separator_row,
        inner.cols,
        &separator_line,
        palette.border,
        palette.bg,
    );

    let total_line = usage_total_line(&usage.report.totals);
    ctx.put_text_bold(
        inner.col,
        total_row,
        inner.cols,
        &total_line,
        palette.fg,
        palette.bg,
    );
    ctx.put_text(
        inner.col,
        total_separator_row,
        inner.cols,
        &separator_line,
        palette.border,
        palette.bg,
    );

    usage.clamp_scroll(list_rows);
    let lines = usage_overlay_lines(&usage.report);

    for (row_offset, line) in lines.iter().skip(usage.scroll).take(list_rows).enumerate() {
        let row = list_row + row_offset as i32;
        let is_separator = is_usage_separator_line(line);
        let is_total = is_usage_total_line(line);
        let fg = if is_separator {
            palette.border
        } else if is_total {
            palette.fg
        } else {
            brighten(palette.muted, 32)
        };
        let text = truncate_to_cells(line, list_width as usize);
        if is_total {
            ctx.put_text_bold(inner.col, row, list_width, &text, fg, palette.bg);
        } else {
            ctx.put_text(inner.col, row, list_width, &text, fg, palette.bg);
        }
    }
    ctx.render_scrollbar(
        inner.col + inner.cols - 1,
        list_row,
        list_rows as i32,
        list_rows,
        usage_overlay_line_count(&usage.report),
        usage.scroll,
        palette.border,
        palette.bg,
        "╎",
        palette.accent_2,
        "┃",
    );

    ctx.put_text(
        inner.col,
        footer_row,
        inner.cols,
        &truncate_to_cells(USAGE_FOOTER_HINT, inner.cols.max(0) as usize),
        palette.muted,
        palette.bg,
    );
}

#[cfg(test)]
mod tests {
    use super::super::test_host::registered_overlays;
    use phi_ext::{OVERLAY_USAGE, OverlayKey, OverlayKeyCode, OverlayKeyModifiers, OverlayOutcome};

    #[test]
    fn usage_overlay_closes_on_esc_and_q() {
        let overlays = registered_overlays();
        let overlay = overlays.get(OVERLAY_USAGE).unwrap();
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
    fn usage_overlay_scroll_keys_do_not_close() {
        let overlays = registered_overlays();
        let overlay = overlays.get(OVERLAY_USAGE).unwrap();
        let mut state = (overlay.init_state)(None);
        for code in [
            OverlayKeyCode::Up,
            OverlayKeyCode::Down,
            OverlayKeyCode::PageDown,
            OverlayKeyCode::Char('r'),
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
    fn usage_overlay_has_scroll_handler() {
        let overlays = registered_overlays();
        let overlay = overlays.get(OVERLAY_USAGE).unwrap();
        let scroll = overlay.handle_scroll.as_ref().expect("scroll handler");
        let mut state = (overlay.init_state)(None);
        scroll(&mut state, 3, 120, 40);
    }
}
