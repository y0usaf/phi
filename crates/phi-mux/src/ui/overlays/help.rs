//! Help overlay (port of pi-harness `overlays/help.rs`) listing mux commands
//! and the current keybindings.

use phi_tui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::dialog::render_dialog_title_line;
use crate::grid::cell_surface::{CellSurface, draw_box, render_cell_scrollbar, truncate_to_cells};
use crate::grid::layout::CellRect as Rect;
use crate::grid::theme;
use crate::keys::{AppAction, Keymap};
use crate::ui::palette::ScenePalette;

#[derive(Clone, Debug, Default)]
pub struct HelpOverlayState {
    pub scroll: usize,
}

impl HelpOverlayState {
    /// Returns `true` when the overlay should close.
    pub fn handle_key(&mut self, key: &KeyEvent, keymap: &Keymap, cols: u16, rows: u16) -> bool {
        let mods = key.modifiers;
        if mods.contains(KeyModifiers::CONTROL) {
            return matches!(key.code, KeyCode::Char('c') | KeyCode::Char('g'));
        }
        if mods.contains(KeyModifiers::SHIFT) {
            if matches!(key.code, KeyCode::Char('G')) {
                self.set_scroll(usize::MAX, keymap, cols, rows);
            }
            return false;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return true,
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1, keymap, cols, rows),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1, keymap, cols, rows),
            KeyCode::PageUp => self.page(-1, keymap, cols, rows),
            KeyCode::PageDown => self.page(1, keymap, cols, rows),
            KeyCode::Home | KeyCode::Char('g') => self.set_scroll(0, keymap, cols, rows),
            KeyCode::End => self.set_scroll(usize::MAX, keymap, cols, rows),
            _ => {}
        }
        false
    }

    pub fn scroll_by(&mut self, delta: i32, keymap: &Keymap, cols: u16, rows: u16) {
        let next = if delta < 0 {
            self.scroll.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            self.scroll.saturating_add(delta as usize)
        };
        self.set_scroll(next, keymap, cols, rows);
    }

    fn page(&mut self, delta_pages: i32, keymap: &Keymap, cols: u16, rows: u16) {
        let visible = visible_rows(cols, rows).max(1);
        self.scroll_by(
            delta_pages.saturating_mul(visible as i32),
            keymap,
            cols,
            rows,
        );
    }

    fn set_scroll(&mut self, scroll: usize, keymap: &Keymap, cols: u16, rows: u16) {
        let lines = help_overlay_lines(keymap);
        let visible = visible_rows(cols, rows).max(1);
        self.scroll = scroll.min(lines.len().saturating_sub(visible));
    }
}

fn help_overlay_rect(cols: i32, rows: i32) -> Rect {
    let cols = cols.max(1);
    let rows = rows.max(1);
    let width = cols.clamp(1, 88);
    let height = rows.clamp(1, 20);
    Rect::new((cols - width) / 2, (rows - height) / 2, width, height)
}

fn visible_rows(cols: u16, rows: u16) -> usize {
    let inner = help_overlay_rect(i32::from(cols), i32::from(rows)).inset_edges(2, 1, 2, 1);
    inner.rows.saturating_sub(2).max(0) as usize
}

pub fn help_overlay_lines(keymap: &Keymap) -> Vec<String> {
    let mut lines = vec![
        "Global".to_string(),
        help_line("ctrl+g / :help", "toggle this help"),
        help_line(":open <dir>", "open project"),
        help_line(":archive", "browse archived sessions"),
        help_line(":usage", "show usage totals"),
        help_line(":refresh / :reload", "refresh current / all sessions"),
        help_line(":up / :down", "previous / next session"),
        help_line(":left / :right", "previous / next project"),
        help_line(":quit / ctrl+q", "quit"),
        String::new(),
        "Navigation & actions".to_string(),
    ];

    for action in [
        AppAction::PreviousProject,
        AppAction::NextProject,
        AppAction::PreviousSession,
        AppAction::NextSession,
        AppAction::NewSession,
        AppAction::RefreshSession,
        AppAction::RefreshAllSessions,
        AppAction::ArchiveSession,
        AppAction::RemoveProject,
        AppAction::CopySelection,
        AppAction::PasteClipboard,
    ] {
        let bindings = keymap.bindings_for(action).join(" / ");
        if !bindings.is_empty() {
            lines.push(help_line(&bindings, action_help_text(action)));
        }
    }

    lines.extend([
        String::new(),
        "Terminal".to_string(),
        help_line("mouse drag", "select text"),
        help_line("wheel / scroll keys", "scroll terminal"),
        help_line("::", "send literal ':' to terminal"),
        String::new(),
        "Command line".to_string(),
        help_line("ctrl+a/e", "start / end"),
        help_line("ctrl+w/u", "delete word / clear"),
        help_line("backspace", "delete; empty input closes"),
        String::new(),
        "Help / archive dialogs".to_string(),
        help_line("↑/↓ j/k wheel", "scroll / select"),
        help_line("pgup/pgdn", "page up / down"),
        help_line("home/end g/G", "jump top / bottom"),
        help_line("esc q ctrl+c", "close"),
    ]);
    lines
}

fn action_help_text(action: AppAction) -> &'static str {
    match action {
        AppAction::PreviousProject => "previous project",
        AppAction::NextProject => "next project",
        AppAction::PreviousSession => "previous session",
        AppAction::NextSession => "next session",
        AppAction::NewSession => "new session",
        AppAction::RefreshSession => "refresh session",
        AppAction::RefreshAllSessions => "refresh all sessions",
        AppAction::ArchiveSession => "archive selected session",
        AppAction::RemoveProject => "remove project from sidebar",
        AppAction::CopySelection => "copy terminal selection",
        AppAction::PasteClipboard => "paste clipboard",
        AppAction::Help => "toggle help",
        AppAction::Quit => "quit",
    }
}

fn help_line(keys: &str, description: &str) -> String {
    format!("{keys:<24} {description}")
}

pub fn render_help_overlay(
    surface: &mut CellSurface,
    help: &mut HelpOverlayState,
    keymap: &Keymap,
    palette: &ScenePalette,
) {
    let rect = help_overlay_rect(surface.cols, surface.rows);
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
    render_dialog_title_line(
        surface,
        inner.row,
        inner.col,
        inner.cols,
        " HELP ",
        " ctrl+g/Esc/q close ",
        palette,
    );

    let lines = help_overlay_lines(keymap);
    let list_row = inner.row + 2;
    let list_rows = (inner.row + inner.rows - list_row).max(0) as usize;
    if list_rows == 0 {
        return;
    }
    help.scroll = help.scroll.min(lines.len().saturating_sub(list_rows));
    let text_width = (inner.cols - 1).max(0);
    for (row_offset, line) in lines.iter().skip(help.scroll).take(list_rows).enumerate() {
        let row = list_row + row_offset as i32;
        if line.is_empty() {
            continue;
        }
        let is_heading = !line.starts_with(' ')
            && !line.contains("  ")
            && !line.contains('/')
            && !line.contains(':');
        if is_heading {
            render_dialog_title_line(surface, row, inner.col, text_width, line, "", palette);
        } else {
            let split_at = line.len().min(24);
            let (keys, description) = line.split_at(split_at);
            surface.put_text(
                inner.col,
                row,
                split_at as i32,
                palette.accent_2,
                palette.bg,
                keys.trim_end(),
            );
            surface.put_text(
                inner.col + 25,
                row,
                text_width.saturating_sub(25),
                theme::brighten(palette.muted, 44),
                palette.bg,
                &truncate_to_cells(description.trim(), text_width.saturating_sub(25) as usize),
            );
        }
    }
    render_cell_scrollbar(
        surface,
        inner.col + inner.cols - 1,
        list_row,
        list_rows as i32,
        list_rows,
        lines.len(),
        help.scroll,
        palette.border,
        palette.bg,
        "╎",
        palette.accent_2,
        "┃",
    );
}
