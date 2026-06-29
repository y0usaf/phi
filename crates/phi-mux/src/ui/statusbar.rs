//! Statusbar rendering: mode label, project/session titles, and status text.

use super::palette::ScenePalette;
use super::scene::{Chrome, MuxMode, StatusNoteKind};
use super::sidebar::SIDEBAR_ANIMATION_FRAME_MS;
use crate::grid::cell_surface::{CellSurface, char_cell_width, display_cell_width, truncate_to_cells};
use crate::grid::color::Color;
use crate::grid::layout::CellRect;
use crate::grid::theme;

const STATUS_SEPARATOR_GLYPH: &str = "│";
const STATUS_NOTE_OK_FG: Color = Color::rgb(0, 0, 0);
const STATUS_NOTE_ERROR_FG: Color = Color::rgb(255, 255, 255);

/// Render text with a sweeping shimmer highlight, mirroring the sidebar's
/// `render_sidebar_gradient_text` triangle-wave shimmer. Each character's
/// foreground is brightened by a shimmer intensity derived from `now_ms`
/// and the character's column position, producing a highlight that sweeps
/// left-to-right across the text and cycles repeatedly.
///
/// `phase_offset` shifts the wave start position so different segments
/// (mode label, project, session, status) shimmer with slight phase offsets
/// rather than all peaking at the same column simultaneously.
#[allow(clippy::too_many_arguments)]
fn render_shimmer_text(
    surface: &mut CellSurface,
    col: i32,
    row: i32,
    max_cols: i32,
    fg: Color,
    bg: Color,
    text: &str,
    now_ms: u64,
    phase_offset: usize,
    bold: bool,
    shimmer_color: Color,
) {
    if max_cols <= 0 || text.is_empty() {
        return;
    }
    let frame = (now_ms / SIDEBAR_ANIMATION_FRAME_MS) as usize;
    let end = col + max_cols;
    let mut cursor = col;
    for ch in text.chars() {
        if cursor >= end {
            break;
        }
        let width = char_cell_width(ch);
        if cursor + width > end {
            break;
        }
        let slot = phase_offset + (cursor - col).max(0) as usize;
        let phase = (frame + slot) % 24;
        let intensity = if phase < 12 { phase } else { 24 - phase };
        // Blend strongly toward the shimmer color (max ~231/255 = 91%),
        // matching the sidebar's proven `intensity * 21` approach.
        let shimmered = theme::fade_toward(fg, shimmer_color, (intensity * 21).min(255) as u8);
        let mut buf = [0; 4];
        surface.put_cell_span_styled(
            cursor,
            row,
            width.max(1),
            ch.encode_utf8(&mut buf),
            shimmered,
            bg,
            false,
            false,
            bold,
        );
        cursor += width.max(1);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusbarState {
    Normal,
    Command,
    Ok,
    Error,
}

impl StatusbarState {
    fn resolve(mode: MuxMode, chrome: &Chrome) -> Self {
        if matches!(mode, MuxMode::Command) {
            return Self::Command;
        }
        match chrome.status_kind {
            Some(StatusNoteKind::Ok) => Self::Ok,
            Some(StatusNoteKind::Error) => Self::Error,
            None => Self::Normal,
        }
    }
}

pub(super) fn render_statusbar(
    surface: &mut CellSurface,
    panel: CellRect,
    sidebar_panel: Option<CellRect>,
    chrome: &Chrome,
    palette: &ScenePalette,
    mode: MuxMode,
    now_ms: u64,
) {
    if panel.cols <= 0 || panel.rows <= 0 {
        return;
    }

    let state = StatusbarState::resolve(mode, chrome);
    let statusline = CellRect::new(panel.col, panel.row, panel.cols, 1);
    let bg = statusbar_bg(state, &chrome.status, palette);
    let fg = statusbar_fg(state, palette);
    surface.fill_rect(statusline, fg, bg);

    let row = statusline.row;
    let main_col =
        render_statusbar_sidebar_segment(surface, statusline, sidebar_panel, palette, state, bg, now_ms);
    let main_cols = (panel.col + panel.cols - main_col).max(0);
    if main_cols <= 0 {
        return;
    }

    let right = truncate_to_cells(&statusline_right_text(chrome, state), main_cols as usize);
    let right_cols = display_cell_width(&right) as i32;
    let left_max_cols = if right_cols > 0 {
        main_cols.saturating_sub(right_cols + 1)
    } else {
        main_cols
    };
    let left = truncate_to_cells(&format!(" {} ", chrome.project), left_max_cols as usize);
    let center = if chrome.session.is_empty() {
        String::new()
    } else {
        truncate_to_cells(&format!(" {} ", chrome.session), main_cols as usize)
    };
    let left_cols = display_cell_width(&left) as i32;
    let center_cols = display_cell_width(&center) as i32;

    if left_cols > 0 {
        render_shimmer_text(surface, main_col, row, left_cols, fg, bg, &left, now_ms, 8, false, palette.running);
    }

    if right_cols > 0 {
        let right_fg = statusbar_right_fg(state, &chrome.status, palette);
        let bold = matches!(state, StatusbarState::Ok | StatusbarState::Error);
        render_shimmer_text(
            surface,
            main_col + main_cols - right_cols,
            row,
            right_cols,
            right_fg,
            bg,
            &right,
            now_ms,
            16,
            bold,
            palette.accent,
        );
    }

    if center_cols > 0 && center_cols <= main_cols {
        let centered_col = main_col + (main_cols - center_cols) / 2;
        let left_limit_col = main_col + left_cols + i32::from(left_cols > 0);
        let right_limit_col = main_col + main_cols - right_cols - i32::from(right_cols > 0);
        if centered_col >= left_limit_col && centered_col + center_cols <= right_limit_col {
            render_shimmer_text(surface, centered_col, row, center_cols, fg, bg, &center, now_ms, 12, false, palette.accent_2);
        }
    }
}

fn statusbar_fg(state: StatusbarState, palette: &ScenePalette) -> Color {
    match state {
        StatusbarState::Command => palette.border,
        StatusbarState::Ok => STATUS_NOTE_OK_FG,
        StatusbarState::Error => STATUS_NOTE_ERROR_FG,
        StatusbarState::Normal => palette.statusbar_fg,
    }
}

fn statusbar_bg(state: StatusbarState, status: &str, palette: &ScenePalette) -> Color {
    match state {
        StatusbarState::Command => palette.warning,
        StatusbarState::Ok => palette.success_subtle,
        StatusbarState::Error => palette.error,
        StatusbarState::Normal if statusline_status_is_error(status) => palette.error,
        StatusbarState::Normal => palette.statusbar_bg,
    }
}

fn statusbar_right_fg(state: StatusbarState, status: &str, palette: &ScenePalette) -> Color {
    match state {
        StatusbarState::Ok => STATUS_NOTE_OK_FG,
        StatusbarState::Error => STATUS_NOTE_ERROR_FG,
        StatusbarState::Command => statusbar_fg(state, palette),
        StatusbarState::Normal => statusline_status_color(status, palette),
    }
}

fn render_statusbar_sidebar_segment(
    surface: &mut CellSurface,
    panel: CellRect,
    sidebar_panel: Option<CellRect>,
    palette: &ScenePalette,
    state: StatusbarState,
    status_bg: Color,
    now_ms: u64,
) -> i32 {
    let row = panel.row;
    let fg = statusbar_fg(state, palette);
    let Some(sidebar_panel) = sidebar_panel else {
        let mode_label = statusline_mode_label(state);
        render_shimmer_text(
            surface,
            panel.col,
            row,
            crate::MODE_LABEL_WIDTH,
            fg,
            status_bg,
            &mode_label,
            now_ms,
            0,
            true,
            palette.running,
        );
        return crate::MODE_LABEL_WIDTH.min(panel.cols);
    };

    let sidebar_cols = sidebar_panel.cols.min(panel.cols).max(0);
    if sidebar_cols <= 0 {
        return panel.col;
    }

    let mode_label = statusline_mode_label(state);
    render_shimmer_text(
        surface,
        panel.col,
        row,
        crate::MODE_LABEL_WIDTH.min(sidebar_cols),
        fg,
        status_bg,
        &mode_label,
        now_ms,
        0,
        true,
        palette.running,
    );

    let separator_col = panel.col + sidebar_cols - 1;
    surface.set_cell(
        separator_col,
        row,
        theme::TRANSPARENT,
        status_bg,
        STATUS_SEPARATOR_GLYPH,
        false,
    );
    panel.col + sidebar_cols
}

fn statusline_mode_label(state: StatusbarState) -> String {
    // Compact chips: the bar should orient, not shout.
    let text = match state {
        StatusbarState::Command => "CMD",
        StatusbarState::Ok => "OK",
        StatusbarState::Error => "ERR",
        StatusbarState::Normal => "NOR",
    };
    format!(" {:<3} ", text)
}

fn statusline_right_text(chrome: &Chrome, state: StatusbarState) -> String {
    if chrome.status.is_empty() {
        return String::new();
    }
    match state {
        StatusbarState::Ok => format!(" ✓ {} ", chrome.status),
        StatusbarState::Error => format!(" ✗ {} ", chrome.status),
        _ => format!(
            " {} {} ",
            statusline_status_symbol(&chrome.status),
            chrome.status
        ),
    }
}

fn statusline_status_symbol(status: &str) -> &'static str {
    match status {
        "thinking" => "⋯",
        "outputting" => "⟡",
        "running" | "launching" => ">",
        "error" | "interrupted" | "crashed" => "×",
        "exited" => "◌",
        _ if status.starts_with("tool") => "⚙",
        _ if status.contains("queued") => "○",
        _ => "•",
    }
}

fn statusline_status_color(status: &str, palette: &ScenePalette) -> Color {
    match status {
        "thinking" | "outputting" | "running" | "launching" => palette.running,
        "error" | "interrupted" | "crashed" => palette.error,
        "exited" => palette.warning,
        _ if status.starts_with("tool") => palette.running,
        _ if status.contains("queued") => palette.warning,
        _ if statusline_status_is_error(status) => palette.statusbar_fg,
        _ => palette.accent_2,
    }
}

fn statusline_status_is_error(status: &str) -> bool {
    let status = status.to_ascii_lowercase();
    status == "error"
        || status.contains("error")
        || status.contains("unknown command")
        || status.contains("not found")
        || status.contains("usage:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_symbols_match_plan_glyphs() {
        assert_eq!(statusline_status_symbol("thinking"), "⋯");
        assert_eq!(statusline_status_symbol("tool bash"), "⚙");
        assert_eq!(statusline_status_symbol("crashed"), "×");
        assert_eq!(statusline_status_symbol("exited"), "◌");
        assert_eq!(statusline_status_symbol("anything"), "•");
    }

    #[test]
    fn statusbar_renders_mode_project_and_status() {
        let mut surface = CellSurface::new(80, 3, theme::TEXT, theme::TRANSPARENT);
        let chrome = Chrome {
            project: "phi".into(),
            session: "hello world".into(),
            status: "thinking".into(),
            status_kind: None,
        };
        render_statusbar(
            &mut surface,
            CellRect::new(0, 2, 80, 1),
            None,
            &chrome,
            &ScenePalette::default(),
            MuxMode::Normal,
            0,
        );
        let row: String = (0..80)
            .map(|col| {
                surface
                    .resolve_text(surface.cell(col, 2).unwrap().text)
                    .to_owned()
            })
            .collect();
        assert!(row.contains("NOR"));
        assert!(row.contains("phi"));
        assert!(row.contains("hello world"));
        assert!(row.contains("⋯ thinking"));
    }
}
