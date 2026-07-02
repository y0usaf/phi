//! The `phi:about` overlay — a simple info panel demonstrating the
//! extension overlay pipeline. Toggled by `Ctrl+?`, closes on Esc/Enter/q.

use std::sync::Arc;

use phi_ext::{DrawContext, DrawRect, OverlayKeyCode, OverlayOutcome, OverlaySpec, OverlayState};

#[derive(Default)]
struct AboutState {}

pub(super) fn spec() -> OverlaySpec {
    OverlaySpec::new(
        "phi:about",
        "About Phi — version and build info",
        Arc::new(|_payload| Box::new(AboutState::default()) as OverlayState),
        Arc::new(render),
        Arc::new(handle_key),
    )
    .with_toggle_key("ctrl+?") // Ctrl+Shift+/ (a.k.a. Ctrl+?)
}

fn render(ctx: &mut dyn DrawContext, _state: &mut OverlayState) {
    let palette = ctx.palette();
    let size = ctx.size();
    let width = size.cols.clamp(20, 60);
    let height = size.rows.clamp(6, 12);
    let col = (size.cols - width) / 2;
    let row = (size.rows - height) / 2;
    let rect = DrawRect::new(col, row, width, height);

    let fg = palette.fg;
    let bg = palette.bg;

    ctx.draw_box(rect, fg, bg, palette.border);
    let inner = rect.inset(2, 1);

    ctx.put_text_bold(
        inner.col,
        inner.row,
        inner.cols,
        " Phi ",
        palette.accent,
        bg,
    );

    ctx.put_text(
        inner.col,
        inner.row + 2,
        inner.cols,
        "An AI coding agent with an extensible TUI.",
        fg,
        bg,
    );
    ctx.put_text(
        inner.col,
        inner.row + 3,
        inner.cols,
        "Extensions can register overlays,",
        fg,
        bg,
    );
    ctx.put_text(
        inner.col,
        inner.row + 4,
        inner.cols,
        "themes, animations, and more.",
        fg,
        bg,
    );

    ctx.put_text(
        inner.col,
        inner.row + 6,
        inner.cols,
        "Press Esc/Enter/q to close.",
        palette.muted,
        bg,
    );
}

fn handle_key(
    _state: &mut OverlayState,
    key: &phi_ext::OverlayKey,
    _cols: u16,
    _rows: u16,
) -> OverlayOutcome {
    match key.code {
        OverlayKeyCode::Esc | OverlayKeyCode::Enter => OverlayOutcome::Close,
        OverlayKeyCode::Char('q') | OverlayKeyCode::Char('Q') if !key.modifiers.control => {
            OverlayOutcome::Close
        }
        _ => OverlayOutcome::None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_host::registered_overlays;
    use phi_ext::{OverlayKey, OverlayKeyCode, OverlayKeyModifiers, OverlayOutcome};

    #[test]
    fn about_overlay_handle_key_closes_on_esc_and_enter() {
        let overlays = registered_overlays();
        let overlay = overlays.get("phi:about").unwrap();
        let mut state = (overlay.init_state)(None);

        for code in [OverlayKeyCode::Esc, OverlayKeyCode::Enter] {
            let key = OverlayKey {
                code,
                modifiers: OverlayKeyModifiers::default(),
            };
            assert_eq!(
                (overlay.handle_key)(&mut state, &key, 80, 24),
                OverlayOutcome::Close
            );
        }
    }

    #[test]
    fn about_overlay_handle_key_none_on_random_char() {
        let overlays = registered_overlays();
        let overlay = overlays.get("phi:about").unwrap();
        let mut state = (overlay.init_state)(None);

        let key = OverlayKey {
            code: OverlayKeyCode::Char('x'),
            modifiers: OverlayKeyModifiers::default(),
        };
        assert_eq!(
            (overlay.handle_key)(&mut state, &key, 80, 24),
            OverlayOutcome::None
        );
    }
}
