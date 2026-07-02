//! Frame composition (port of pi-harness `app::scene`): sidebar, embedded
//! terminal blit, and the bottom statusline.

use super::palette::ScenePalette;
use super::screen_snapshot::ScreenSnapshot;
use super::sidebar::{
    SidebarRow, SidebarRowKind, SidebarStatusKind, SidebarViewportItem, sidebar_status_glyph,
};
use super::statusbar::render_statusbar;
use super::terminal_scene::render_terminal;
use super::visual_context::VisualContext;
use crate::grid::cell_surface::{CellSurface, display_cell_width, truncate_to_cells};
use crate::grid::color::Color;
use crate::grid::layout::{CellLayout, CellRect};
use crate::grid::selection::SelectionRange;
use crate::grid::theme;
use crate::status::SessionStatus;

const SCROLLBAR_TRACK_GLYPH: &str = "│";
const SCROLLBAR_THUMB_GLYPH: &str = "┃";
const SIDEBAR_SELECTOR_GLYPH: &str = "> ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MuxMode {
    Normal,
    Command,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusNoteKind {
    Ok,
    Error,
}

/// Statusline content for the current frame.
#[derive(Clone, Debug, Default)]
pub struct Chrome {
    pub project: String,
    pub session: String,
    pub status: String,
    pub status_kind: Option<StatusNoteKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HardwareCursorPos {
    pub col: i32,
    pub row: i32,
}

/// The statusline text and color for a session status.
pub fn statusline_status(status: &SessionStatus) -> String {
    match status {
        SessionStatus::Idle => String::new(),
        SessionStatus::Thinking => "thinking".to_owned(),
        SessionStatus::Outputting => "outputting".to_owned(),
        SessionStatus::Tool { name } => format!("tool {name}"),
        SessionStatus::Crashed => "crashed".to_owned(),
        SessionStatus::Done => "exited".to_owned(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_scene(
    surface: &mut CellSurface,
    layout: &CellLayout,
    sidebar_rows: &[SidebarRow],
    sidebar_viewport: &[SidebarViewportItem],
    screen: Option<&ScreenSnapshot>,
    selection: Option<SelectionRange>,
    chrome: &Chrome,
    palette: &ScenePalette,
    mode: MuxMode,
    now_ms: u64,
    scrollback_total: usize,
    visual: &VisualContext,
) -> Option<HardwareCursorPos> {
    let sidebar_panel =
        (layout.sidebar.cols > 0 && layout.sidebar.rows > 0).then_some(layout.sidebar);
    if let Some(panel) = sidebar_panel {
        render_sidebar(
            surface,
            panel,
            panel,
            sidebar_rows,
            sidebar_viewport,
            palette,
            now_ms,
            visual,
        );
    }
    let cursor = render_terminal(
        surface,
        layout.terminal,
        screen,
        selection,
        scrollback_total,
        palette,
    );
    render_statusbar(
        surface,
        layout.statusbar,
        sidebar_panel,
        chrome,
        palette,
        mode,
        now_ms,
        visual,
    );
    cursor
}

#[allow(clippy::too_many_arguments)]
pub fn render_sidebar(
    surface: &mut CellSurface,
    panel: CellRect,
    mut content: CellRect,
    rows: &[SidebarRow],
    viewport: &[SidebarViewportItem],
    palette: &ScenePalette,
    now_ms: u64,
    visual: &VisualContext,
) {
    surface.fill_rect(panel, palette.fg, palette.sidebar_bg);

    let visible_rows = content.rows.max(0) as usize;
    if visible_rows == 0 || content.cols <= 0 {
        return;
    }

    if content.cols > 1 {
        let scrollbar_col = content.col + content.cols - 1;
        render_sidebar_scrollbar(
            surface,
            scrollbar_col,
            content.row,
            content.rows,
            visible_rows,
            rows.len(),
            viewport_scroll_from_rows(viewport),
            theme::TRANSPARENT,
            palette.sidebar_bg,
            palette.accent_2,
        );
        content.cols -= 1;
    }

    for item in viewport {
        let Some(row) = rows.get(item.row_index) else {
            continue;
        };
        if item.visible_row >= visible_rows || content.cols <= 0 {
            continue;
        }
        let row_y = content.row + item.visible_row as i32;
        let selected = row.inverted;

        let sticky = item.sticky;
        let (row_fg, row_bg, reverse) = sidebar_row_style(row, selected, sticky, palette);
        surface.fill_rect(
            CellRect {
                col: content.col,
                row: row_y,
                cols: content.cols,
                rows: 1,
            },
            row_fg,
            row_bg,
        );

        match row.kind {
            SidebarRowKind::Project(_) => {
                render_sidebar_project_rule(
                    surface,
                    CellRect::new(content.col, row_y, content.cols, 1),
                    &row.text,
                    row_fg,
                    row_bg,
                    row.status,
                    now_ms,
                    reverse,
                    row.selector,
                    row.current,
                    palette,
                );
            }

            SidebarRowKind::Session { .. } => {
                let status = row.status.map(|status| {
                    (
                        sidebar_status_glyph(status, now_ms, visual),
                        animated_sidebar_status_color(status, palette, now_ms, item.visible_row),
                    )
                });
                let branch = if row.selector {
                    SIDEBAR_SELECTOR_GLYPH
                } else {
                    "  "
                };
                let indent_cols = content.cols.min(display_cell_width(branch) as i32);
                let reserved_cells = status
                    .as_ref()
                    .map(|(glyph, _)| display_cell_width(glyph) as i32 + 1)
                    .unwrap_or(0);
                let text_cols = content
                    .cols
                    .saturating_sub(indent_cols)
                    .saturating_sub(reserved_cells);
                let value = truncate_to_cells(&row.text, text_cols as usize);
                let branch_fg = if selected {
                    palette.accent
                } else {
                    palette.border
                };
                surface.put_text_styled(
                    content.col,
                    row_y,
                    indent_cols,
                    branch_fg,
                    row_bg,
                    branch,
                    reverse,
                    false,
                );
                if matches!(row.status, Some(SidebarStatusKind::Active)) {
                    render_sidebar_gradient_text(
                        surface,
                        CellRect::new(content.col + indent_cols, row_y, text_cols, 1),
                        &value,
                        row_bg,
                        reverse,
                        palette.accent,
                        palette.accent_2,
                        row.status,
                        now_ms,
                        item.visible_row,
                        palette,
                    );
                } else {
                    let title_fg = if matches!(
                        row.status,
                        Some(SidebarStatusKind::Interrupted | SidebarStatusKind::Notification)
                    ) {
                        sidebar_status_color_for_palette(row.status.unwrap(), palette)
                    } else {
                        row_fg
                    };
                    surface.put_text_styled(
                        content.col + indent_cols,
                        row_y,
                        text_cols,
                        title_fg,
                        row_bg,
                        &value,
                        reverse,
                        false,
                    );
                }

                if let Some((glyph, color)) = status {
                    let glyph_cols = display_cell_width(&glyph) as i32;
                    let glyph_col = content.col + content.cols.saturating_sub(glyph_cols);
                    surface.put_text_styled(
                        glyph_col, row_y, glyph_cols, color, row_bg, &glyph, reverse, false,
                    );
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_sidebar_scrollbar(
    surface: &mut CellSurface,
    col: i32,
    row: i32,
    rows: i32,
    visible_items: usize,
    total_items: usize,
    scroll_from_top: usize,
    track_fg: Color,
    track_bg: Color,
    thumb_fg: Color,
) {
    if rows <= 0 {
        return;
    }

    for offset in 0..rows {
        surface.set_cell(
            col,
            row + offset,
            track_fg,
            track_bg,
            SCROLLBAR_TRACK_GLYPH,
            false,
        );
    }

    if visible_items == 0 || total_items <= visible_items {
        return;
    }

    let thumb_rows =
        (((rows as i64 * visible_items as i64) / total_items as i64).max(1) as i32).min(rows);
    let max_scroll = total_items.saturating_sub(visible_items).max(1);
    let thumb_row = row
        + (((rows - thumb_rows).max(0) as i64 * scroll_from_top as i64) / max_scroll as i64) as i32;
    for offset in 0..thumb_rows {
        surface.set_cell(
            col,
            thumb_row + offset,
            thumb_fg,
            track_bg,
            SCROLLBAR_THUMB_GLYPH,
            false,
        );
    }
}

/// Render text with a per-character left-to-right colour gradient and,
/// when `status` is `Active`, a triangle-wave shimmer overlay.
///
/// The shimmer uses [`phi_tui::shimmer_triangle_intensity`] and
/// [`phi_tui::shimmer_triangle_blend`] — the same shared parameters as the
/// statusbar shimmer and the thinking-equalizer gradient, so all three
/// surfaces conform to one intensity and blend strength.
#[allow(clippy::too_many_arguments)]
fn render_sidebar_gradient_text(
    surface: &mut CellSurface,
    rect: CellRect,
    value: &str,
    bg: Color,
    reverse: bool,
    from: Color,
    to: Color,
    status: Option<SidebarStatusKind>,
    now_ms: u64,
    phase_slot: usize,
    palette: &ScenePalette,
) {
    if rect.cols <= 0 || value.is_empty() {
        return;
    }

    let value_cols = display_cell_width(value) as i32;
    let denom = value_cols.saturating_sub(1).max(1) as u16;

    let mut cursor = rect.col;
    for ch in value.chars() {
        if cursor >= rect.col + rect.cols {
            break;
        }
        let width = display_cell_width(&ch.to_string()) as i32;
        if cursor + width > rect.col + rect.cols {
            break;
        }
        let mix = (((cursor - rect.col) as u16 * 255) / denom).min(255) as u8;
        let mut fg = theme::fade_toward(from, to, mix);
        if let Some(SidebarStatusKind::Active) = status {
            let slot = phase_slot + (cursor - rect.col).max(0) as usize;
            let intensity = phi_tui::shimmer_triangle_intensity(now_ms, slot);
            let blend = phi_tui::shimmer_triangle_blend(intensity);
            let status_color = sidebar_status_color_for_palette(status.unwrap(), palette);
            fg = theme::fade_toward(fg, status_color, blend);
        }
        let mut buf = [0; 4];
        surface.put_cell_span_styled(
            cursor,
            rect.row,
            width.max(1),
            ch.encode_utf8(&mut buf),
            fg,
            bg,
            false,
            reverse,
            false,
        );
        cursor += width.max(1);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_sidebar_project_rule(
    surface: &mut CellSurface,
    rect: CellRect,
    label: &str,
    label_fg: Color,
    bg: Color,
    status: Option<SidebarStatusKind>,
    now_ms: u64,
    reverse: bool,
    show_selector: bool,
    current_project: bool,
    palette: &ScenePalette,
) {
    if rect.cols <= 0 {
        return;
    }

    let selector_cols = if show_selector {
        let selector_cols = rect
            .cols
            .min(display_cell_width(SIDEBAR_SELECTOR_GLYPH) as i32);
        surface.put_text_styled(
            rect.col,
            rect.row,
            selector_cols,
            palette.accent,
            bg,
            SIDEBAR_SELECTOR_GLYPH,
            reverse,
            false,
        );
        selector_cols
    } else {
        0
    };
    let label_area = CellRect::new(
        rect.col + selector_cols,
        rect.row,
        rect.cols.saturating_sub(selector_cols),
        1,
    );

    let idle_project_fg = theme::brighten(palette.statusbar_bg, 36);
    let label_fg = match status {
        Some(SidebarStatusKind::Notification) => {
            sidebar_status_color_for_palette(SidebarStatusKind::Notification, palette)
        }
        None => idle_project_fg,
        _ => label_fg,
    };
    let value = truncate_to_cells(&format!(" {label} "), label_area.cols.max(0) as usize);
    let value_cols = display_cell_width(&value) as i32;
    let label_rect = CellRect::new(
        label_area.col + label_area.cols.saturating_sub(value_cols) / 2,
        label_area.row,
        value_cols.min(label_area.cols),
        1,
    );

    let gradient_title = matches!(status, Some(SidebarStatusKind::Active));
    if !reverse {
        let left_rule = CellRect::new(
            label_area.col,
            label_area.row,
            label_rect.col.saturating_sub(label_area.col),
            1,
        );
        let right_rule_col = label_rect.col + label_rect.cols;
        let right_rule = CellRect::new(
            right_rule_col,
            label_area.row,
            (label_area.col + label_area.cols).saturating_sub(right_rule_col),
            1,
        );
        if gradient_title {
            render_sidebar_project_rule_gradient(
                surface,
                left_rule,
                bg,
                reverse,
                status,
                now_ms,
                0,
                idle_project_fg,
                palette,
            );
            render_sidebar_project_rule_gradient(
                surface,
                right_rule,
                bg,
                reverse,
                status,
                now_ms,
                left_rule.cols.max(0) as usize + value_cols.max(0) as usize,
                idle_project_fg,
                palette,
            );
        } else {
            let rule_fg = sidebar_project_rule_fg(
                label_fg,
                idle_project_fg,
                status,
                current_project,
                palette,
            );
            render_sidebar_project_rule_solid(surface, left_rule, rule_fg, bg);
            render_sidebar_project_rule_solid(surface, right_rule, rule_fg, bg);
        }
    }
    if gradient_title {
        render_sidebar_gradient_text(
            surface,
            label_rect,
            &value,
            bg,
            reverse,
            palette.accent,
            palette.accent_2,
            status,
            now_ms,
            0,
            palette,
        );
    } else {
        surface.put_text_styled(
            label_rect.col,
            label_rect.row,
            label_rect.cols,
            label_fg,
            bg,
            &value,
            reverse,
            true,
        );
    }
}

fn render_sidebar_project_rule_solid(
    surface: &mut CellSurface,
    rect: CellRect,
    fg: Color,
    bg: Color,
) {
    if rect.cols <= 0 {
        return;
    }
    for col in rect.col..(rect.col + rect.cols) {
        surface.set_cell(col, rect.row, fg, bg, "━", false);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_sidebar_project_rule_gradient(
    surface: &mut CellSurface,
    rect: CellRect,
    bg: Color,
    reverse: bool,
    status: Option<SidebarStatusKind>,
    now_ms: u64,
    phase_slot: usize,
    idle_project_fg: Color,
    palette: &ScenePalette,
) {
    if rect.cols <= 0 {
        return;
    }
    let value = "━".repeat(rect.cols as usize);
    render_sidebar_gradient_text(
        surface,
        rect,
        &value,
        bg,
        reverse,
        idle_project_fg,
        palette.statusbar_bg,
        status,
        now_ms,
        phase_slot,
        palette,
    );
}

fn sidebar_project_rule_fg(
    label_fg: Color,
    idle_project_fg: Color,
    status: Option<SidebarStatusKind>,
    current_project: bool,
    palette: &ScenePalette,
) -> Color {
    match status {
        Some(SidebarStatusKind::Interrupted | SidebarStatusKind::Notification) => label_fg,
        Some(SidebarStatusKind::Active) => idle_project_fg,
        None if current_project => palette.statusbar_fg,
        None => palette.border,
    }
}

fn animated_sidebar_status_color(
    status: SidebarStatusKind,
    palette: &ScenePalette,
    now_ms: u64,
    slot: usize,
) -> Color {
    let base = sidebar_status_color_for_palette(status, palette);
    match status {
        SidebarStatusKind::Interrupted | SidebarStatusKind::Notification => base,
        SidebarStatusKind::Active => {
            // Use the shared triangle-wave intensity, but a gentler blend
            // (intensity * 10, max 120/255 ≈ 47 %) since this modulates a
            // status glyph colour, not a gradient text shimmer.
            let intensity = phi_tui::shimmer_triangle_intensity(now_ms, slot);
            theme::fade_toward(base, palette.accent_2, (intensity * 10).min(180))
        }
    }
}

fn sidebar_status_color_for_palette(status: SidebarStatusKind, palette: &ScenePalette) -> Color {
    match status {
        SidebarStatusKind::Active => palette.running,
        SidebarStatusKind::Interrupted => palette.error,
        SidebarStatusKind::Notification => palette.success,
    }
}

fn viewport_scroll_from_rows(viewport: &[SidebarViewportItem]) -> usize {
    viewport
        .iter()
        .find(|item| !item.sticky)
        .map(|item| item.row_index)
        .unwrap_or(0)
}

fn sidebar_row_style(
    row: &SidebarRow,
    selected: bool,
    sticky: bool,
    palette: &ScenePalette,
) -> (Color, Color, bool) {
    let fg = palette_color(row.fg, palette);
    let base_bg = row.bg.unwrap_or(palette.sidebar_bg);
    if selected {
        // Keep the row's semantic color live even while selected so status /
        // model-color changes show through immediately; selection itself is
        // carried by the accent selector glyph on the leftmost cells.
        return (fg, base_bg, false);
    }
    if sticky {
        return (palette.accent, base_bg, false);
    }

    (fg, base_bg, false)
}

fn palette_color(color: Color, palette: &ScenePalette) -> Color {
    if color == theme::TEXT {
        palette.fg
    } else if color == theme::MUTED {
        palette.muted
    } else if color == theme::ACCENT {
        palette.accent
    } else if color == theme::RUNNING {
        palette.running
    } else if color == theme::WARNING {
        palette.warning
    } else if color == theme::ERROR {
        palette.error
    } else {
        color
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statusline_status_maps_session_statuses() {
        assert_eq!(statusline_status(&SessionStatus::Idle), "");
        assert_eq!(statusline_status(&SessionStatus::Thinking), "thinking");
        assert_eq!(
            statusline_status(&SessionStatus::Tool {
                name: "bash".into()
            }),
            "tool bash"
        );
        assert_eq!(statusline_status(&SessionStatus::Crashed), "crashed");
    }

    #[test]
    fn selected_sidebar_rows_keep_their_resolved_fg_color() {
        let palette = ScenePalette::default();
        let selected_running = SidebarRow {
            kind: SidebarRowKind::Session {
                project_index: 0,
                session_index: 0,
            },
            text: "session".into(),
            fg: theme::RUNNING,
            bg: None,
            inverted: true,
            selector: true,
            current: true,
            status: Some(SidebarStatusKind::Active),
        };

        let (fg, bg, reverse) = sidebar_row_style(&selected_running, true, false, &palette);
        assert_eq!(fg, palette.running);
        assert_eq!(bg, palette.sidebar_bg);
        assert!(!reverse);
    }
}
