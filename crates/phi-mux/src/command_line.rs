//! `:` command-line editing state, ported from pi-harness `tui/command_line.rs`.

use phi_tui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::grid::cell_surface::display_cell_width;
use crate::grid::layout::CellLayout;

/// What the mux loop should do after the command line consumed a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandLineEvent {
    Consumed,
    Submit(String),
    Cancel,
}

#[derive(Clone, Debug)]
pub struct CommandLineState {
    pub input: String,
    cursor: usize,
}

impl CommandLineState {
    /// An empty command line (the only production construction site: `:`
    /// always opens blank).
    pub fn new() -> Self {
        Self {
            input: String::new(),
            cursor: 0,
        }
    }

    /// Test-only: start with pre-filled input so key handling can be
    /// exercised on realistic text.
    #[cfg(test)]
    fn with_input(input: impl Into<String>) -> Self {
        let input = input.into();
        let cursor = input.len();
        Self { input, cursor }
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> CommandLineEvent {
        let mods = key.modifiers;
        let ctrl_only = mods.contains(KeyModifiers::CONTROL)
            && !mods.contains(KeyModifiers::SHIFT)
            && !mods.contains(KeyModifiers::ALT);
        if ctrl_only {
            match key.code {
                KeyCode::Char('a') => {
                    self.move_home();
                }
                KeyCode::Char('e') => {
                    self.move_end();
                }
                KeyCode::Char('u') => self.clear(),
                KeyCode::Char('w') => {
                    self.delete_word_back();
                }
                KeyCode::Char('c') => return CommandLineEvent::Cancel,
                _ => {}
            }
            return CommandLineEvent::Consumed;
        }

        if mods.difference(KeyModifiers::SHIFT).is_empty() {
            match key.code {
                KeyCode::Enter => return CommandLineEvent::Submit(std::mem::take(&mut self.input)),
                KeyCode::Esc => return CommandLineEvent::Cancel,
                KeyCode::Backspace => {
                    if self.input.is_empty() {
                        return CommandLineEvent::Cancel;
                    }
                    self.backspace();
                }
                KeyCode::Delete => {
                    self.delete();
                }
                KeyCode::Left => {
                    self.move_left();
                }
                KeyCode::Right => {
                    self.move_right();
                }
                KeyCode::Home => {
                    self.move_home();
                }
                KeyCode::End => {
                    self.move_end();
                }
                KeyCode::Char(ch) if !ch.is_control() => {
                    self.input.insert(self.cursor, ch);
                    self.cursor += ch.len_utf8();
                }
                _ => {}
            }
        }
        CommandLineEvent::Consumed
    }

    pub fn insert_str(&mut self, text: &str) {
        self.input.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    pub fn backspace(&mut self) -> bool {
        let Some(previous) = previous_char_boundary(&self.input, self.cursor) else {
            return false;
        };
        self.input.drain(previous..self.cursor);
        self.cursor = previous;
        true
    }

    pub fn delete(&mut self) -> bool {
        let Some(next) = next_char_boundary(&self.input, self.cursor) else {
            return false;
        };
        self.input.drain(self.cursor..next);
        true
    }

    pub fn delete_word_back(&mut self) -> bool {
        let original = self.cursor;
        while self
            .input
            .get(..self.cursor)
            .and_then(|text| text.chars().next_back())
            .is_some_and(char::is_whitespace)
        {
            self.backspace();
        }
        while self
            .input
            .get(..self.cursor)
            .and_then(|text| text.chars().next_back())
            .is_some_and(|ch| !ch.is_whitespace())
        {
            self.backspace();
        }
        self.cursor != original
    }

    pub fn clear(&mut self) {
        self.input.clear();
        self.cursor = 0;
    }

    pub fn move_left(&mut self) -> bool {
        let Some(previous) = previous_char_boundary(&self.input, self.cursor) else {
            return false;
        };
        self.cursor = previous;
        true
    }

    pub fn move_right(&mut self) -> bool {
        let Some(next) = next_char_boundary(&self.input, self.cursor) else {
            return false;
        };
        self.cursor = next;
        true
    }

    pub fn move_home(&mut self) -> bool {
        let changed = self.cursor != 0;
        self.cursor = 0;
        changed
    }

    pub fn move_end(&mut self) -> bool {
        let changed = self.cursor != self.input.len();
        self.cursor = self.input.len();
        changed
    }

    pub fn visible_text_and_cursor_col(&self, max_cols: usize) -> (String, i32) {
        let max_cols = max_cols.max(1);
        let full = format!(":{}", self.input);
        let cursor_byte = 1 + self.cursor;
        let full_cells = display_cell_width(&full);
        let cursor_cells = display_cell_width(&full[..cursor_byte]);
        // Reserve a column for the caret when it sits at the very end of an
        // exactly-full line, matching pi's input behavior.
        if full_cells < max_cols || (full_cells == max_cols && cursor_cells < full_cells) {
            return (full, cursor_cells.min(max_cols.saturating_sub(1)) as i32);
        }

        let marker = "…";
        let marker_cells = display_cell_width(marker);
        if max_cols <= marker_cells {
            return (marker.to_string(), 0);
        }

        let left_overflow = cursor_cells > max_cols.saturating_sub(marker_cells);
        let start_byte = if left_overflow {
            byte_index_at_cell_width(
                &full,
                cursor_cells.saturating_sub(max_cols.saturating_sub(marker_cells)),
            )
        } else {
            0
        };
        let available_cols = if start_byte > 0 {
            max_cols.saturating_sub(marker_cells)
        } else {
            max_cols
        };
        let mut visible = String::new();
        if start_byte > 0 {
            visible.push_str(marker);
        }
        visible.push_str(&prefix_to_cells(&full[start_byte..], available_cols));

        let cursor_col = (if start_byte > 0 { marker_cells } else { 0 })
            + display_cell_width(&full[start_byte..cursor_byte]).min(available_cols);
        (visible, cursor_col.min(max_cols.saturating_sub(1)) as i32)
    }
}

fn previous_char_boundary(value: &str, index: usize) -> Option<usize> {
    if index == 0 {
        return None;
    }
    value[..index]
        .char_indices()
        .next_back()
        .map(|(index, _)| index)
}

fn next_char_boundary(value: &str, index: usize) -> Option<usize> {
    if index >= value.len() {
        return None;
    }
    value[index..]
        .char_indices()
        .nth(1)
        .map(|(offset, _)| index + offset)
        .or(Some(value.len()))
}

fn byte_index_at_cell_width(value: &str, target_cells: usize) -> usize {
    if target_cells == 0 {
        return 0;
    }
    let mut cells = 0;
    for (index, ch) in value.char_indices() {
        if cells >= target_cells {
            return index;
        }
        cells += command_char_width(ch);
    }
    value.len()
}

fn prefix_to_cells(value: &str, max_cols: usize) -> String {
    let mut output = String::new();
    let mut cells = 0;
    for ch in value.chars() {
        let width = command_char_width(ch);
        if cells + width > max_cols {
            break;
        }
        output.push(ch);
        cells += width;
    }
    output
}

fn command_char_width(ch: char) -> usize {
    unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1)
}

pub fn command_line_start_col(layout: &CellLayout, surface_cols: i32) -> i32 {
    if layout.sidebar.cols > 0 {
        return layout.sidebar.cols.min(surface_cols).max(0);
    }
    crate::MODE_LABEL_WIDTH.min(surface_cols).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn command_line_backspace_updates_utf8_cursor() {
        let mut command_line = CommandLineState::with_input("open café");
        assert!(command_line.backspace());
        assert_eq!(command_line.input, "open caf");
        command_line.move_left();
        command_line.insert_str("é");
        assert_eq!(command_line.input, "open caéf");
    }

    #[test]
    fn handle_key_submits_and_cancels() {
        let mut command_line = CommandLineState::with_input("quit");
        assert_eq!(
            command_line.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE)),
            CommandLineEvent::Submit("quit".to_string())
        );

        let mut command_line = CommandLineState::with_input("");
        assert_eq!(
            command_line.handle_key(&key(KeyCode::Backspace, KeyModifiers::NONE)),
            CommandLineEvent::Cancel
        );

        let mut command_line = CommandLineState::with_input("x");
        assert_eq!(
            command_line.handle_key(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            CommandLineEvent::Cancel
        );
    }

    #[test]
    fn handle_key_edits_with_ctrl_shortcuts() {
        let mut command_line = CommandLineState::with_input("open foo bar");
        command_line.handle_key(&key(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(command_line.input, "open foo ");
        command_line.handle_key(&key(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(command_line.input, "");
        command_line.handle_key(&key(KeyCode::Char('A'), KeyModifiers::SHIFT));
        assert_eq!(command_line.input, "A");
    }

    #[test]
    fn visible_text_reserves_space_for_end_cursor_on_exact_fit() {
        let command_line = CommandLineState::with_input("abcd");
        let (visible, col) = command_line.visible_text_and_cursor_col(5);
        assert_eq!(visible, "…abcd");
        assert_eq!(col, 4);
    }
}
