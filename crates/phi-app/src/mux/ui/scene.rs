//! Frame composition (port of pi-harness `app::scene`): sidebar, embedded
//! terminal blit, and the bottom statusline.

use super::super::status::SessionStatus;
use super::cell_surface::{
    CellSurface, display_cell_width, render_cell_scrollbar, truncate_to_cells,
};
use super::color::Color;
use super::layout::{CellLayout, CellRect};
use super::selection::SelectionRange;
use super::sidebar::{
    SIDEBAR_ANIMATION_FRAME_MS, SidebarRow, SidebarRowKind, SidebarStatusKind, SidebarViewportItem,
    sidebar_status_glyph,
};
use super::terminal_view::for_each_terminal_screen_cell;
use super::theme;

const STATUSLINE_MODE_LABEL_WIDTH: i32 = 5;
const SCROLLBAR_TRACK_GLYPH: &str = "│";
const SCROLLBAR_THUMB_GLYPH: &str = "┃";
const STATUS_SEPARATOR_GLYPH: &str = "│";
const STATUS_NOTE_OK_FG: Color = Color::rgb(0, 0, 0);
const STATUS_NOTE_ERROR_FG: Color = Color::rgb(255, 255, 255);

const SIDEBAR_SELECTOR_GLYPH: &str = "> ";

#[derive(Clone, Copy)]
pub struct ScenePalette {
    pub fg: Color,
    pub bg: Color,
    pub statusbar_bg: Color,
    pub statusbar_fg: Color,
    pub sidebar_bg: Color,
    pub border: Color,
    pub muted: Color,
    pub term_fg: Color,
    pub term_bg: Color,
    pub accent: Color,
    pub accent_2: Color,

    pub ansi: [Color; 16],

    pub running: Color,
    pub success: Color,
    pub success_subtle: Color,
    pub warning: Color,
    pub error: Color,
}

impl Default for ScenePalette {
    fn default() -> Self {
        Self {
            fg: theme::TEXT,
            // Grounded canvas: real surfaces instead of floating on whatever
            // the host terminal happens to show through.
            bg: theme::SURFACE,
            statusbar_fg: theme::STATUS_FG,
            statusbar_bg: theme::STATUS_BG,
            // Sidebar reads as its own raised pane next to the terminal.
            sidebar_bg: theme::SURFACE_RAISED,
            border: theme::BORDER,
            muted: theme::MUTED,
            term_fg: theme::TERM_FG,
            term_bg: theme::SURFACE,
            accent: theme::ACCENT,
            accent_2: theme::ACCENT_2,
            ansi: theme::default_ansi_palette(),
            running: theme::RUNNING,
            success: theme::SUCCESS,
            success_subtle: theme::SUCCESS_SUBTLE,
            warning: theme::WARNING,
            error: theme::ERROR,
        }
    }
}

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
    screen: Option<&vt100::Screen>,
    selection: Option<SelectionRange>,
    chrome: &Chrome,
    palette: &ScenePalette,
    mode: MuxMode,
    now_ms: u64,
    scrollback_total: usize,
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
    );
    cursor
}

pub fn render_statusbar(
    surface: &mut CellSurface,
    panel: CellRect,
    sidebar_panel: Option<CellRect>,
    chrome: &Chrome,
    palette: &ScenePalette,
    mode: MuxMode,
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
        render_statusbar_sidebar_segment(surface, statusline, sidebar_panel, palette, state, bg);
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
        surface.put_text(main_col, row, left_cols, fg, bg, &left);
    }

    if right_cols > 0 {
        let right_fg = statusbar_right_fg(state, &chrome.status, palette);
        let bold = matches!(state, StatusbarState::Ok | StatusbarState::Error);
        surface.put_text_styled(
            main_col + main_cols - right_cols,
            row,
            right_cols,
            right_fg,
            bg,
            &right,
            false,
            bold,
        );
    }

    if center_cols > 0 && center_cols <= main_cols {
        let centered_col = main_col + (main_cols - center_cols) / 2;
        let left_limit_col = main_col + left_cols + i32::from(left_cols > 0);
        let right_limit_col = main_col + main_cols - right_cols - i32::from(right_cols > 0);
        if centered_col >= left_limit_col && centered_col + center_cols <= right_limit_col {
            surface.put_text(centered_col, row, center_cols, fg, bg, &center);
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
) -> i32 {
    let row = panel.row;
    let fg = statusbar_fg(state, palette);
    let Some(sidebar_panel) = sidebar_panel else {
        let mode_label = statusline_mode_label(state);
        surface.put_text_bold(
            panel.col,
            row,
            STATUSLINE_MODE_LABEL_WIDTH,
            fg,
            status_bg,
            &mode_label,
        );
        return STATUSLINE_MODE_LABEL_WIDTH.min(panel.cols);
    };

    let sidebar_cols = sidebar_panel.cols.min(panel.cols).max(0);
    if sidebar_cols <= 0 {
        return panel.col;
    }

    let mode_label = statusline_mode_label(state);
    surface.put_text_bold(
        panel.col,
        row,
        STATUSLINE_MODE_LABEL_WIDTH.min(sidebar_cols),
        fg,
        status_bg,
        &mode_label,
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
        "running" | "launching" => "●",
        "error" | "interrupted" | "crashed" => "×",
        "exited" => "◌",
        _ if status.starts_with("tool") => "⚙",
        _ if status.contains("queued") => "▶",
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

#[allow(clippy::too_many_arguments)]
pub fn render_sidebar(
    surface: &mut CellSurface,
    panel: CellRect,
    mut content: CellRect,
    rows: &[SidebarRow],
    viewport: &[SidebarViewportItem],
    palette: &ScenePalette,
    now_ms: u64,
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
                        sidebar_status_glyph(status, now_ms),
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
                    let glyph_cols = display_cell_width(glyph) as i32;
                    let glyph_col = content.col + content.cols.saturating_sub(glyph_cols);
                    surface.put_text_styled(
                        glyph_col, row_y, glyph_cols, color, row_bg, glyph, reverse, false,
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
            let phase = ((now_ms / SIDEBAR_ANIMATION_FRAME_MS) as usize + slot) % 24;
            let intensity = if phase < 12 { phase } else { 24 - phase };
            let status_color = sidebar_status_color_for_palette(status.unwrap(), palette);
            fg = theme::fade_toward(fg, status_color, (intensity * 21).min(255) as u8);
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
            let phase = ((now_ms / SIDEBAR_ANIMATION_FRAME_MS) as usize + slot) % 24;
            let intensity = if phase < 12 { phase } else { 24 - phase };
            theme::fade_toward(base, palette.accent_2, (intensity * 10).min(180) as u8)
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

pub fn render_terminal(
    surface: &mut CellSurface,
    rect: CellRect,
    screen: Option<&vt100::Screen>,
    selection: Option<SelectionRange>,
    scrollback_total: usize,
    palette: &ScenePalette,
) -> Option<HardwareCursorPos> {
    surface.fill_rect(rect, palette.term_fg, palette.term_bg);
    if rect.rows <= 0 || rect.cols <= 0 {
        return None;
    }
    let screen = screen?;

    for_each_terminal_screen_cell(
        screen,
        rect.rows.max(0) as u16,
        rect.cols.max(0) as u16,
        selection,
        palette.term_fg,
        palette.term_bg,
        &palette.ansi,
        false,
        |cell| {
            surface.put_cell_span(
                rect.col + i32::from(cell.col),
                rect.row + i32::from(cell.row),
                i32::from(cell.span),
                cell.text,
                cell.fg,
                cell.bg,
                cell.underline,
            );
        },
    );
    render_terminal_scrollback(surface, rect, screen, scrollback_total, palette);

    hardware_cursor_for_screen(rect, screen)
}

fn render_terminal_scrollback(
    surface: &mut CellSurface,
    rect: CellRect,
    screen: &vt100::Screen,
    scrollback_total: usize,
    palette: &ScenePalette,
) {
    let visible_rows = usize::from(screen.size().0);
    let offset = screen.scrollback();
    if visible_rows == 0 || offset == 0 || rect.rows <= 0 {
        return;
    }

    let total_above = scrollback_total.max(offset);
    render_cell_scrollbar(
        surface,
        rect.col + rect.cols - 1,
        rect.row,
        rect.rows,
        visible_rows,
        visible_rows.saturating_add(total_above),
        total_above.saturating_sub(offset),
        palette.border,
        palette.term_bg,
        SCROLLBAR_TRACK_GLYPH,
        palette.accent_2,
        SCROLLBAR_THUMB_GLYPH,
    );
}

fn hardware_cursor_for_screen(rect: CellRect, screen: &vt100::Screen) -> Option<HardwareCursorPos> {
    if screen.scrollback() != 0 || screen.hide_cursor() || rect.cols <= 0 || rect.rows <= 0 {
        return None;
    }

    let (cursor_row, cursor_col) = screen.cursor_position();
    let col = rect.col + i32::from(cursor_col).min(rect.cols.saturating_sub(1));
    let row = rect.row + i32::from(cursor_row).min(rect.rows.saturating_sub(1));
    Some(HardwareCursorPos { col, row })
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
    fn status_symbols_match_plan_glyphs() {
        assert_eq!(statusline_status_symbol("thinking"), "⋯");
        assert_eq!(statusline_status_symbol("tool bash"), "⚙");
        assert_eq!(statusline_status_symbol("crashed"), "×");
        assert_eq!(statusline_status_symbol("exited"), "◌");
        assert_eq!(statusline_status_symbol("anything"), "•");
    }

    #[test]
    fn terminal_blit_writes_screen_text_and_cursor() {
        let mut parser = vt100::Parser::new(4, 10, 0);
        parser.process(b"hi");
        let mut surface = CellSurface::new(12, 6, theme::TEXT, theme::TRANSPARENT);
        let rect = CellRect::new(1, 1, 10, 4);
        let cursor = render_terminal(
            &mut surface,
            rect,
            Some(parser.screen()),
            None,
            0,
            &ScenePalette::default(),
        )
        .unwrap();
        assert_eq!(surface.cell(1, 1).unwrap().text, "h");
        assert_eq!(surface.cell(2, 1).unwrap().text, "i");
        assert_eq!((cursor.col, cursor.row), (3, 1));
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
        );
        let row: String = (0..80)
            .map(|col| surface.cell(col, 2).unwrap().text.clone())
            .collect();
        assert!(row.contains("NOR"));
        assert!(row.contains("phi"));
        assert!(row.contains("hello world"));
        assert!(row.contains("⋯ thinking"));
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
