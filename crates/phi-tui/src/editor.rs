//! Multi-line input editor: emacs bindings, input history, bracketed paste.
//! Rendering is left to the caller (the editor reports logical lines + a
//! byte-offset cursor).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditorAction {
    None,
    /// Enter pressed with non-empty input; the buffer was cleared.
    Submit(String),
    /// Ctrl-C.
    Cancel,
    /// Ctrl-D on an empty buffer.
    Quit,
}

pub struct Editor {
    lines: Vec<String>,
    row: usize,
    /// Byte offset within `lines[row]`.
    col: usize,
    history: Vec<String>,
    history_idx: Option<usize>,
    draft: Option<Vec<String>>,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            row: 0,
            col: 0,
            history: Vec::new(),
            history_idx: None,
            draft: None,
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn set_text(&mut self, text: &str) {
        self.lines = text.split('\n').map(str::to_owned).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.row = self.lines.len() - 1;
        self.col = self.lines[self.row].len();
    }

    pub fn cursor_row(&self) -> usize {
        self.row
    }

    /// Text on the cursor's line, up to the cursor.
    pub fn before_cursor(&self) -> &str {
        &self.lines[self.row][..self.col]
    }

    /// Replace `prefix` (which must end at the cursor) with `replacement`,
    /// leaving the cursor after the inserted text. No-op when the text
    /// before the cursor does not end with `prefix`.
    pub fn replace_prefix(&mut self, prefix: &str, replacement: &str) {
        if !self.before_cursor().ends_with(prefix) {
            return;
        }
        let count = prefix.len();
        let line = &mut self.lines[self.row];
        let start = self.col - count;
        line.replace_range(start..self.col, "");
        self.col = start;
        self.insert_raw(replacement);
    }

    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.row = 0;
        self.col = 0;
        self.history_idx = None;
        self.draft = None;
    }

    /// Add a submitted prompt to the recall history used by Up/Down.
    /// Consecutive duplicates are ignored; blank/whitespace-only prompts are
    /// never recorded.
    pub fn add_history_entry(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        if self.history.last().is_some_and(|prev| prev == text) {
            return;
        }
        self.history.push(text.to_owned());
    }

    /// Replace the recall history used by Up/Down and leave the editor out of
    /// history-browsing mode.
    pub fn clear_history(&mut self) {
        self.history.clear();
        self.history_idx = None;
        self.draft = None;
    }

    /// Logical lines and the cursor as (line index, byte column). The
    /// caller prepends its prompt and adjusts the first line's column.
    pub fn render(&self) -> (&[String], (usize, usize)) {
        (&self.lines, (self.row, self.col))
    }

    pub fn insert_str(&mut self, text: &str) {
        self.insert_raw(text);
    }

    /// Insert pasted text, normalizing line endings and stripping control
    /// characters.
    pub fn insert_paste(&mut self, text: &str) {
        let clean = normalize_paste(text);
        self.insert_raw(&clean);
    }

    /// Buffer text.
    pub fn expanded_text(&self) -> String {
        self.text()
    }

    fn insert_raw(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        for (i, part) in text.split('\n').enumerate() {
            if i > 0 {
                self.insert_newline();
            }
            self.lines[self.row].insert_str(self.col, part);
            self.col += part.len();
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> EditorAction {
        if key.kind == KeyEventKind::Release {
            return EditorAction::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Char('c') if ctrl => return EditorAction::Cancel,
            KeyCode::Char('d') if ctrl => {
                if self.is_empty() {
                    return EditorAction::Quit;
                }
                self.delete_forward();
            }
            KeyCode::Enter if shift => {
                self.insert_newline();
            }
            KeyCode::Char('j') if ctrl => {
                self.insert_newline();
            }
            KeyCode::Enter => {
                // Backslash continuation: terminals without shift+enter
                // support type `\` then enter for a newline (pi-compatible).
                if self.before_cursor().ends_with('\\') {
                    self.delete_back();
                    self.insert_newline();
                    return EditorAction::None;
                }
                let text = self.text();
                if text.trim().is_empty() {
                    return EditorAction::None;
                }
                self.clear();
                self.add_history_entry(&text);
                return EditorAction::Submit(text);
            }
            KeyCode::Backspace if ctrl || alt => self.delete_word_back(),
            KeyCode::Backspace => {
                self.delete_back();
            }
            KeyCode::Delete if alt => self.delete_word_forward(),
            KeyCode::Delete => {
                self.delete_forward();
            }
            KeyCode::Char('w') if ctrl => self.delete_word_back(),
            KeyCode::Char('d') if alt => self.delete_word_forward(),
            KeyCode::Char('a') if ctrl => self.col = 0,
            KeyCode::Char('e') if ctrl => self.col = self.lines[self.row].len(),
            KeyCode::Char('k') if ctrl => self.kill_to_eol(),
            KeyCode::Char('u') if ctrl => self.kill_to_bol(),
            KeyCode::Char('b') if alt => self.move_word_back(),
            KeyCode::Char('f') if alt => self.move_word_forward(),
            KeyCode::Char('b') if ctrl => self.move_left(),
            KeyCode::Char('f') if ctrl => self.move_right(),
            KeyCode::Left if ctrl || alt => self.move_word_back(),
            KeyCode::Right if ctrl || alt => self.move_word_forward(),
            KeyCode::Left => self.move_left(),
            KeyCode::Right => self.move_right(),
            KeyCode::Up => self.move_up(),
            KeyCode::Down => self.move_down(),
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = self.lines[self.row].len(),
            KeyCode::PageUp => {
                self.row = 0;
                self.col = 0;
            }
            KeyCode::PageDown => {
                self.row = self.lines.len() - 1;
                self.col = self.lines[self.row].len();
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                let byte = self.col;
                self.lines[self.row].insert(byte, c);
                self.col += c.len_utf8();
            }
            _ => {}
        }
        EditorAction::None
    }

    fn insert_newline(&mut self) {
        let rest = self.lines[self.row].split_off(self.col);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
    }

    fn move_left(&mut self) {
        if self.col > 0 {
            self.col = prev_char_boundary(&self.lines[self.row], self.col);
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.lines[self.row].len();
        }
    }

    fn move_right(&mut self) {
        let line = &self.lines[self.row];
        if self.col < line.len() {
            self.col = next_char_boundary(line, self.col);
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    fn move_up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            self.col = self.col.min(self.lines[self.row].len());
        } else {
            self.history_prev();
        }
    }

    fn move_down(&mut self) {
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = self.col.min(self.lines[self.row].len());
        } else {
            self.history_next();
        }
    }

    fn history_prev(&mut self) {
        let idx = match self.history_idx {
            Some(0) => return,
            Some(i) => i - 1,
            None if self.history.is_empty() => return,
            None => {
                self.draft = Some(self.lines.clone());
                self.history.len() - 1
            }
        };
        self.history_idx = Some(idx);
        self.set_text(&self.history[idx].clone());
    }

    fn history_next(&mut self) {
        let Some(idx) = self.history_idx else { return };
        if idx + 1 < self.history.len() {
            self.history_idx = Some(idx + 1);
            self.set_text(&self.history[idx + 1].clone());
        } else {
            self.history_idx = None;
            let draft = self.draft.take().unwrap_or_else(|| vec![String::new()]);
            self.lines = draft;
            self.row = self.lines.len() - 1;
            self.col = self.lines[self.row].len();
        }
    }

    fn delete_back(&mut self) {
        if self.col > 0 {
            let line = &mut self.lines[self.row];
            let start = prev_char_boundary(line, self.col);
            line.replace_range(start..self.col, "");
            self.col = start;
        } else if self.row > 0 {
            let removed = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.lines[self.row].len();
            self.lines[self.row].push_str(&removed);
        }
    }

    fn delete_forward(&mut self) {
        let line_len = self.lines[self.row].len();
        if self.col < line_len {
            let line = &mut self.lines[self.row];
            let end = next_char_boundary(line, self.col);
            line.replace_range(self.col..end, "");
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
    }

    fn kill_to_eol(&mut self) {
        let line_len = self.lines[self.row].len();
        if self.col < line_len {
            self.lines[self.row].truncate(self.col);
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
    }

    fn kill_to_bol(&mut self) {
        if self.col == 0 {
            return;
        }
        self.lines[self.row].replace_range(..self.col, "");
        self.col = 0;
    }

    fn delete_word_back(&mut self) {
        if self.col == 0 {
            if self.row > 0 {
                self.delete_back();
            }
            return;
        }
        let target = self.word_back_col();
        let line = &mut self.lines[self.row];
        line.replace_range(target..self.col, "");
        self.col = target;
    }

    fn delete_word_forward(&mut self) {
        let line_len = self.lines[self.row].len();
        if self.col == line_len {
            if self.row + 1 < self.lines.len() {
                self.delete_forward();
            }
            return;
        }
        let target = self.word_forward_col();
        let line = &mut self.lines[self.row];
        line.replace_range(self.col..target, "");
    }

    fn move_word_back(&mut self) {
        if self.col == 0 {
            self.move_left();
        } else {
            self.col = self.word_back_col();
        }
    }

    fn move_word_forward(&mut self) {
        let line_len = self.lines[self.row].len();
        if self.col == line_len {
            self.move_right();
        } else {
            self.col = self.word_forward_col();
        }
    }

    fn word_back_col(&self) -> usize {
        let s = &self.lines[self.row];
        let chars: Vec<(usize, char)> = s.char_indices().collect();
        let mut i = chars
            .iter()
            .position(|(b, _)| *b >= self.col)
            .unwrap_or(chars.len());
        while i > 0 && !is_word_char(chars[i - 1].1) {
            i -= 1;
        }
        while i > 0 && is_word_char(chars[i - 1].1) {
            i -= 1;
        }
        if i < chars.len() { chars[i].0 } else { s.len() }
    }

    fn word_forward_col(&self) -> usize {
        let s = &self.lines[self.row];
        let chars: Vec<(usize, char)> = s.char_indices().collect();
        let mut i = chars
            .iter()
            .position(|(b, _)| *b >= self.col)
            .unwrap_or(chars.len());
        while i < chars.len() && !is_word_char(chars[i].1) {
            i += 1;
        }
        while i < chars.len() && is_word_char(chars[i].1) {
            i += 1;
        }
        if i < chars.len() { chars[i].0 } else { s.len() }
    }
}

/// Normalize pasted text pi-style: unify line endings, expand tabs to four
/// spaces, and strip control characters other than newline.
fn normalize_paste(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\t', "    ")
        .chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .collect()
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn prev_char_boundary(s: &str, idx: usize) -> usize {
    if idx == 0 || idx > s.len() {
        return 0;
    }
    let mut i = idx - 1;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn next_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx + 1;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn alt(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
    }

    fn type_str(ed: &mut Editor, s: &str) {
        for c in s.chars() {
            ed.handle_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn typing_and_submit() {
        let mut ed = Editor::new();
        type_str(&mut ed, "hello");
        assert_eq!(
            ed.handle_key(key(KeyCode::Enter)),
            EditorAction::Submit("hello".into())
        );
        assert!(ed.is_empty());
    }

    #[test]
    fn empty_submit_is_noop() {
        let mut ed = Editor::new();
        assert_eq!(ed.handle_key(key(KeyCode::Enter)), EditorAction::None);
        type_str(&mut ed, "   ");
        assert_eq!(ed.handle_key(key(KeyCode::Enter)), EditorAction::None);
    }

    #[test]
    fn ctrl_j_inserts_newline() {
        let mut ed = Editor::new();
        type_str(&mut ed, "line1");
        ed.handle_key(ctrl('j'));
        type_str(&mut ed, "line2");
        assert_eq!(ed.text(), "line1\nline2");
        let (_, (row, _)) = ed.render();
        assert_eq!(row, 1);
    }

    #[test]
    fn multibyte_cursor_and_backspace() {
        let mut ed = Editor::new();
        ed.insert_str("aé日");
        assert_eq!(ed.render().1, (0, 6)); // 1 + 2 + 3 bytes
        ed.handle_key(key(KeyCode::Backspace));
        assert_eq!(ed.text(), "aé");
        ed.handle_key(key(KeyCode::Left));
        ed.handle_key(key(KeyCode::Backspace));
        assert_eq!(ed.text(), "é");
    }

    #[test]
    fn emacs_line_bindings() {
        let mut ed = Editor::new();
        type_str(&mut ed, "hello world");
        ed.handle_key(ctrl('a'));
        assert_eq!(ed.render().1, (0, 0));
        ed.handle_key(ctrl('e'));
        assert_eq!(ed.render().1, (0, 11));
        ed.handle_key(ctrl('w'));
        assert_eq!(ed.text(), "hello ");
        ed.handle_key(ctrl('u'));
        assert_eq!(ed.text(), "");
    }

    #[test]
    fn kill_to_eol_joins_lines_at_eol() {
        let mut ed = Editor::new();
        ed.insert_str("ab\ncd");
        ed.handle_key(key(KeyCode::Up));
        ed.handle_key(ctrl('e'));
        ed.handle_key(ctrl('k'));
        assert_eq!(ed.text(), "abcd");
    }

    #[test]
    fn word_movement() {
        let mut ed = Editor::new();
        type_str(&mut ed, "foo bar_baz qux");
        ed.handle_key(alt('b'));
        assert_eq!(ed.render().1, (0, 12));
        ed.handle_key(alt('b'));
        assert_eq!(ed.render().1, (0, 4));
        ed.handle_key(alt('f'));
        assert_eq!(ed.render().1, (0, 11));
        ed.handle_key(alt('d'));
        assert_eq!(ed.text(), "foo bar_baz");
    }

    #[test]
    fn history_navigation_preserves_draft() {
        let mut ed = Editor::new();
        type_str(&mut ed, "first");
        ed.handle_key(key(KeyCode::Enter));
        type_str(&mut ed, "second");
        ed.handle_key(key(KeyCode::Enter));
        type_str(&mut ed, "draft");
        ed.handle_key(key(KeyCode::Up));
        assert_eq!(ed.text(), "second");
        ed.handle_key(key(KeyCode::Up));
        assert_eq!(ed.text(), "first");
        ed.handle_key(key(KeyCode::Down));
        assert_eq!(ed.text(), "second");
        ed.handle_key(key(KeyCode::Down));
        assert_eq!(ed.text(), "draft");
    }

    #[test]
    fn cancel_and_quit() {
        let mut ed = Editor::new();
        assert_eq!(ed.handle_key(ctrl('c')), EditorAction::Cancel);
        assert_eq!(ed.handle_key(ctrl('d')), EditorAction::Quit);
        type_str(&mut ed, "x");
        ed.handle_key(ctrl('a'));
        assert_eq!(ed.handle_key(ctrl('d')), EditorAction::None);
        assert_eq!(ed.text(), "");
    }

    #[test]
    fn before_cursor_and_replace_prefix() {
        let mut ed = Editor::new();
        type_str(&mut ed, "/mod");
        assert_eq!(ed.before_cursor(), "/mod");
        ed.replace_prefix("/mod", "/model ");
        assert_eq!(ed.text(), "/model ");
        assert_eq!(ed.before_cursor(), "/model ");

        type_str(&mut ed, "claude");
        ed.replace_prefix("claude", "anthropic/claude-opus-4-7");
        assert_eq!(ed.text(), "/model anthropic/claude-opus-4-7");

        // Prefix not at cursor: no-op.
        ed.replace_prefix("nope", "x");
        assert_eq!(ed.text(), "/model anthropic/claude-opus-4-7");
    }

    #[test]
    fn replace_prefix_mid_line_keeps_suffix() {
        let mut ed = Editor::new();
        type_str(&mut ed, "/lognow");
        ed.handle_key(key(KeyCode::Left));
        ed.handle_key(key(KeyCode::Left));
        ed.handle_key(key(KeyCode::Left));
        assert_eq!(ed.before_cursor(), "/log");
        ed.replace_prefix("/log", "/login ");
        assert_eq!(ed.text(), "/login now");
        assert_eq!(ed.before_cursor(), "/login ");
    }

    #[test]
    fn shift_enter_and_backslash_enter_insert_newline() {
        let mut ed = Editor::new();
        type_str(&mut ed, "one");
        ed.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        type_str(&mut ed, "two\\");
        assert_eq!(ed.handle_key(key(KeyCode::Enter)), EditorAction::None);
        type_str(&mut ed, "three");
        assert_eq!(ed.text(), "one\ntwo\nthree");
    }

    #[test]
    fn alt_enter_submits() {
        let mut ed = Editor::new();
        type_str(&mut ed, "hi");
        assert_eq!(
            ed.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
            EditorAction::Submit("hi".into())
        );
    }

    #[test]
    fn ctrl_arrows_move_by_word() {
        let mut ed = Editor::new();
        type_str(&mut ed, "foo bar baz");
        ed.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(ed.render().1, (0, 8));
        ed.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(ed.render().1, (0, 4));
        ed.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
        assert_eq!(ed.render().1, (0, 7));
    }

    #[test]
    fn page_keys_jump_to_buffer_ends() {
        let mut ed = Editor::new();
        ed.insert_str("one\ntwo\nthree");
        ed.handle_key(key(KeyCode::PageUp));
        assert_eq!(ed.render().1, (0, 0));
        ed.handle_key(key(KeyCode::PageDown));
        assert_eq!(ed.render().1, (2, 5));
    }

    #[test]
    fn paste_normalizes_newlines() {
        let mut ed = Editor::new();
        ed.insert_str("a\r\nb\rc");
        assert_eq!(ed.text(), "a\nb\nc");
    }

    #[test]
    fn small_paste_inserts_directly() {
        let mut ed = Editor::new();
        ed.insert_paste("hello\nworld");
        assert_eq!(ed.text(), "hello\nworld");
    }

    #[test]
    fn large_paste_inserts_directly() {
        let mut ed = Editor::new();
        let big: String = (0..20).map(|i| format!("line {i}\n")).collect();
        ed.insert_paste(&big);
        assert_eq!(ed.text(), big);
    }

    #[test]
    fn char_paste_inserts_directly() {
        let mut ed = Editor::new();
        let big = "x".repeat(1500);
        ed.insert_paste(&big);
        assert_eq!(ed.text(), big);
        assert_eq!(ed.expanded_text(), big);
    }

    #[test]
    fn paste_strips_control_chars_and_expands_tabs() {
        let mut ed = Editor::new();
        ed.insert_paste("a\tb\x07c\r\nd");
        assert_eq!(ed.text(), "a    bc\nd");
    }

    #[test]
    fn up_down_move_within_multiline_before_history() {
        let mut ed = Editor::new();
        type_str(&mut ed, "one");
        ed.handle_key(key(KeyCode::Enter));
        ed.insert_str("aa\nbb");
        ed.handle_key(key(KeyCode::Up));
        assert_eq!(ed.render().1, (0, 2));
        ed.handle_key(key(KeyCode::Up));
        assert_eq!(ed.text(), "one");
    }

    #[test]
    fn explicit_history_entries_recall_latest_first() {
        let mut ed = Editor::new();
        ed.add_history_entry("first");
        ed.add_history_entry("second");
        ed.handle_key(key(KeyCode::Up));
        assert_eq!(ed.text(), "second");
        ed.handle_key(key(KeyCode::Up));
        assert_eq!(ed.text(), "first");
    }

    #[test]
    fn explicit_history_skips_blank_and_consecutive_duplicates() {
        let mut ed = Editor::new();
        ed.add_history_entry("   \n");
        ed.add_history_entry("same");
        ed.add_history_entry("same");
        ed.handle_key(key(KeyCode::Up));
        assert_eq!(ed.text(), "same");
    }
}
