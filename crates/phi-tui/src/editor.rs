//! Multi-line input editor: grapheme-aware cursor movement, emacs bindings,
//! input history, bracketed paste. Rendering is left to the caller (the
//! editor reports logical lines + a display-column cursor).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

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

const KILL_RING_MAX: usize = 50;
const UNDO_MAX: usize = 100;
/// Pastes longer than this many lines or chars collapse to a marker
/// (pi-compatible thresholds).
const PASTE_LINES_MAX: usize = 10;
const PASTE_CHARS_MAX: usize = 1000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JumpDir {
    Forward,
    Backward,
}

pub struct Editor {
    lines: Vec<String>,
    row: usize,
    /// Grapheme index within `lines[row]`.
    col: usize,
    history: Vec<String>,
    history_idx: Option<usize>,
    draft: Option<Vec<String>>,
    kill_ring: Vec<String>,
    /// Region of the last yank (start row/col, end row/col, ring index);
    /// present only while the previous key was a yank, enabling alt-y.
    last_yank: Option<(usize, usize, usize, usize, usize)>,
    undo_stack: Vec<(Vec<String>, usize, usize)>,
    /// Large pastes collapsed to `[paste #N …]` markers: (marker, content).
    /// Markers are expanded on submit; an edited marker stays literal text.
    pastes: Vec<(String, String)>,
    paste_counter: usize,
    /// Char-jump mode (ctrl+] / ctrl+alt+]): the next printable key jumps
    /// the cursor to that character within the current line, vi f/F style.
    jump: Option<JumpDir>,
    /// Preferred visual column for vertical cursor movement, pi-style: when a
    /// short line clamps the cursor, keep the original display column and
    /// restore it once a later line is wide enough again.
    preferred_col: Option<usize>,
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
            kill_ring: Vec::new(),
            last_yank: None,
            undo_stack: Vec::new(),
            pastes: Vec::new(),
            paste_counter: 0,
            jump: None,
            preferred_col: None,
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
        self.col = grapheme_count(&self.lines[self.row]);
        self.preferred_col = None;
    }

    pub fn cursor_row(&self) -> usize {
        self.row
    }

    /// Text on the cursor's line, up to the cursor.
    pub fn before_cursor(&self) -> &str {
        let line = &self.lines[self.row];
        &line[..byte_index(line, self.col)]
    }

    /// Replace `prefix` (which must end at the cursor) with `replacement`,
    /// leaving the cursor after the inserted text. No-op when the text
    /// before the cursor does not end with `prefix`.
    pub fn replace_prefix(&mut self, prefix: &str, replacement: &str) {
        if !self.before_cursor().ends_with(prefix) {
            return;
        }
        self.snapshot();
        let count = grapheme_count(prefix);
        let line = &mut self.lines[self.row];
        let start = byte_index(line, self.col - count);
        let end = byte_index(line, self.col);
        line.replace_range(start..end, "");
        self.col -= count;
        self.insert_raw(replacement);
    }

    pub fn clear(&mut self) {
        if !self.is_empty() {
            self.snapshot();
        }
        self.lines = vec![String::new()];
        self.row = 0;
        self.col = 0;
        self.history_idx = None;
        self.draft = None;
        self.preferred_col = None;
    }

    /// Logical lines and the cursor as (line index, display column). The
    /// caller prepends its prompt and adjusts the first line's column.
    pub fn render(&self) -> (&[String], (usize, usize)) {
        let line = &self.lines[self.row];
        let col = width_of_graphemes(line, self.col);
        (&self.lines, (self.row, col))
    }

    pub fn insert_str(&mut self, text: &str) {
        self.snapshot();
        self.insert_raw(text);
    }

    /// Insert pasted text. Large pastes (pi thresholds: >10 lines or >1000
    /// chars) collapse into a `[paste #N +M lines]` / `[paste #N M chars]`
    /// marker that expands back to the full content on submit.
    pub fn insert_paste(&mut self, text: &str) {
        self.snapshot();
        let clean = normalize_paste(text);
        let line_count = clean.split('\n').count();
        if line_count <= PASTE_LINES_MAX && clean.chars().count() <= PASTE_CHARS_MAX {
            self.insert_raw(&clean);
            return;
        }
        self.paste_counter += 1;
        let id = self.paste_counter;
        let marker = if line_count > PASTE_LINES_MAX {
            format!("[paste #{id} +{line_count} lines]")
        } else {
            format!("[paste #{id} {} chars]", clean.chars().count())
        };
        self.pastes.push((marker.clone(), clean));
        self.insert_raw(&marker);
    }

    /// Buffer text with paste markers expanded to their stored content.
    pub fn expanded_text(&self) -> String {
        self.expand_markers(&self.text())
    }

    fn expand_markers(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for (marker, content) in &self.pastes {
            out = out.replace(marker, content);
        }
        out
    }

    fn insert_raw(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        for (i, part) in text.split('\n').enumerate() {
            if i > 0 {
                self.insert_newline();
            }
            let byte = byte_index(&self.lines[self.row], self.col);
            self.lines[self.row].insert_str(byte, part);
            self.col += grapheme_count(part);
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> EditorAction {
        if key.kind == KeyEventKind::Release {
            return EditorAction::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        if !matches!(key.code, KeyCode::Up | KeyCode::Down) {
            self.preferred_col = None;
        }
        // Yank state survives only across consecutive yanks (alt-y).
        let prev_yank = self.last_yank.take();

        // Char-jump mode: the next printable key jumps; the trigger key
        // cancels; anything else cancels and is processed normally.
        if let Some(dir) = self.jump.take() {
            match key.code {
                KeyCode::Char(']') if ctrl => return EditorAction::None,
                KeyCode::Char(c) if !ctrl && !alt => {
                    self.jump_to_char(c, dir);
                    return EditorAction::None;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Char(']') if ctrl => {
                self.jump = Some(if alt {
                    JumpDir::Backward
                } else {
                    JumpDir::Forward
                });
            }
            KeyCode::Char('c') if ctrl => return EditorAction::Cancel,
            KeyCode::Char('d') if ctrl => {
                if self.is_empty() {
                    return EditorAction::Quit;
                }
                self.snapshot();
                self.delete_forward();
            }
            KeyCode::Enter if shift => {
                self.snapshot();
                self.insert_newline();
            }
            KeyCode::Char('j') if ctrl => {
                self.snapshot();
                self.insert_newline();
            }
            KeyCode::Enter => {
                // Backslash continuation: terminals without shift+enter
                // support type `\` then enter for a newline (pi-compatible).
                if self.before_cursor().ends_with('\\') {
                    self.snapshot();
                    self.delete_back();
                    self.insert_newline();
                    return EditorAction::None;
                }
                let text = self.text();
                if text.trim().is_empty() {
                    return EditorAction::None;
                }
                self.clear();
                if self.history.last() != Some(&text) {
                    self.history.push(text.clone());
                }
                return EditorAction::Submit(self.expand_markers(&text));
            }
            KeyCode::Backspace if ctrl || alt => self.delete_word_back(),
            KeyCode::Backspace => {
                self.snapshot();
                self.delete_back();
            }
            KeyCode::Delete if alt => self.delete_word_forward(),
            KeyCode::Delete => {
                self.snapshot();
                self.delete_forward();
            }
            KeyCode::Char('w') if ctrl => self.delete_word_back(),
            KeyCode::Char('d') if alt => self.delete_word_forward(),
            KeyCode::Char('a') if ctrl => self.col = 0,
            KeyCode::Char('e') if ctrl => self.col = grapheme_count(&self.lines[self.row]),
            KeyCode::Char('k') if ctrl => self.kill_to_eol(),
            KeyCode::Char('u') if ctrl => self.kill_to_bol(),
            KeyCode::Char('y') if ctrl => self.yank(),
            KeyCode::Char('y') if alt => self.yank_pop(prev_yank),
            KeyCode::Char('-' | '_') if ctrl => self.undo(),
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
            KeyCode::End => self.col = grapheme_count(&self.lines[self.row]),
            KeyCode::PageUp => {
                self.row = 0;
                self.col = 0;
            }
            KeyCode::PageDown => {
                self.row = self.lines.len() - 1;
                self.col = grapheme_count(&self.lines[self.row]);
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                self.snapshot();
                let byte = byte_index(&self.lines[self.row], self.col);
                self.lines[self.row].insert(byte, c);
                self.col += 1;
            }
            _ => {}
        }
        EditorAction::None
    }

    fn insert_newline(&mut self) {
        let byte = byte_index(&self.lines[self.row], self.col);
        let rest = self.lines[self.row].split_off(byte);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
    }

    fn move_left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = grapheme_count(&self.lines[self.row]);
        }
    }

    fn move_right(&mut self) {
        if self.col < grapheme_count(&self.lines[self.row]) {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    fn move_up(&mut self) {
        if self.row > 0 {
            let current_visual = width_of_graphemes(&self.lines[self.row], self.col);
            self.row -= 1;
            let target_line = &self.lines[self.row];
            let target_visual = width_of_graphemes(target_line, grapheme_count(target_line));
            let desired = self.preferred_col.unwrap_or(current_visual);
            if desired > target_visual {
                self.preferred_col.get_or_insert(current_visual);
                self.col = grapheme_count(target_line);
            } else {
                self.col = grapheme_index_at_width(target_line, desired);
                self.preferred_col = None;
            }
        } else {
            self.preferred_col = None;
            self.history_prev();
        }
    }

    fn move_down(&mut self) {
        if self.row + 1 < self.lines.len() {
            let current_visual = width_of_graphemes(&self.lines[self.row], self.col);
            self.row += 1;
            let target_line = &self.lines[self.row];
            let target_visual = width_of_graphemes(target_line, grapheme_count(target_line));
            let desired = self.preferred_col.unwrap_or(current_visual);
            if desired > target_visual {
                self.preferred_col.get_or_insert(current_visual);
                self.col = grapheme_count(target_line);
            } else {
                self.col = grapheme_index_at_width(target_line, desired);
                self.preferred_col = None;
            }
        } else {
            self.preferred_col = None;
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
            self.col = grapheme_count(&self.lines[self.row]);
            self.preferred_col = None;
        }
    }

    fn delete_back(&mut self) {
        if self.col > 0 {
            let line = &mut self.lines[self.row];
            let start = byte_index(line, self.col - 1);
            let end = byte_index(line, self.col);
            line.replace_range(start..end, "");
            self.col -= 1;
        } else if self.row > 0 {
            let removed = self.lines.remove(self.row);
            self.row -= 1;
            self.col = grapheme_count(&self.lines[self.row]);
            self.lines[self.row].push_str(&removed);
        }
    }

    fn delete_forward(&mut self) {
        let count = grapheme_count(&self.lines[self.row]);
        if self.col < count {
            let line = &mut self.lines[self.row];
            let start = byte_index(line, self.col);
            let end = byte_index(line, self.col + 1);
            line.replace_range(start..end, "");
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
    }

    fn kill_to_eol(&mut self) {
        let count = grapheme_count(&self.lines[self.row]);
        if self.col < count {
            self.snapshot();
            let byte = byte_index(&self.lines[self.row], self.col);
            let killed = self.lines[self.row].split_off(byte);
            self.kill(killed);
        } else if self.row + 1 < self.lines.len() {
            self.snapshot();
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
            self.kill("\n".to_owned());
        }
    }

    fn kill_to_bol(&mut self) {
        if self.col == 0 {
            return;
        }
        self.snapshot();
        let byte = byte_index(&self.lines[self.row], self.col);
        let killed = self.lines[self.row][..byte].to_owned();
        self.lines[self.row].replace_range(..byte, "");
        self.col = 0;
        self.kill(killed);
    }

    fn delete_word_back(&mut self) {
        if self.col == 0 {
            if self.row > 0 {
                self.snapshot();
                self.delete_back();
            }
            return;
        }
        self.snapshot();
        let target = self.word_back_col();
        let line = &mut self.lines[self.row];
        let start = byte_index(line, target);
        let end = byte_index(line, self.col);
        let killed = line[start..end].to_owned();
        line.replace_range(start..end, "");
        self.col = target;
        self.kill(killed);
    }

    fn delete_word_forward(&mut self) {
        let count = grapheme_count(&self.lines[self.row]);
        if self.col == count {
            if self.row + 1 < self.lines.len() {
                self.snapshot();
                self.delete_forward();
            }
            return;
        }
        self.snapshot();
        let target = self.word_forward_col();
        let line = &mut self.lines[self.row];
        let start = byte_index(line, self.col);
        let end = byte_index(line, target);
        let killed = line[start..end].to_owned();
        line.replace_range(start..end, "");
        self.kill(killed);
    }

    fn kill(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.kill_ring.push(text);
        if self.kill_ring.len() > KILL_RING_MAX {
            self.kill_ring.remove(0);
        }
    }

    fn yank(&mut self) {
        let Some(text) = self.kill_ring.last().cloned() else {
            return;
        };
        self.snapshot();
        let (srow, scol) = (self.row, self.col);
        self.insert_raw(&text);
        self.last_yank = Some((srow, scol, self.row, self.col, self.kill_ring.len() - 1));
    }

    /// Replace the text inserted by the previous yank with the next-older
    /// kill-ring entry (wrapping). No-op unless the previous key was a yank.
    fn yank_pop(&mut self, prev: Option<(usize, usize, usize, usize, usize)>) {
        let Some((srow, scol, erow, ecol, idx)) = prev else {
            return;
        };
        self.snapshot();
        self.delete_region(srow, scol, erow, ecol);
        let idx = idx.checked_sub(1).unwrap_or(self.kill_ring.len() - 1);
        let text = self.kill_ring[idx].clone();
        self.insert_raw(&text);
        self.last_yank = Some((srow, scol, self.row, self.col, idx));
    }

    fn delete_region(&mut self, srow: usize, scol: usize, erow: usize, ecol: usize) {
        if srow == erow {
            let line = &mut self.lines[srow];
            let start = byte_index(line, scol);
            let end = byte_index(line, ecol);
            line.replace_range(start..end, "");
        } else {
            let tail_byte = byte_index(&self.lines[erow], ecol);
            let tail = self.lines[erow][tail_byte..].to_owned();
            let head_byte = byte_index(&self.lines[srow], scol);
            self.lines[srow].truncate(head_byte);
            self.lines[srow].push_str(&tail);
            self.lines.drain(srow + 1..=erow);
        }
        self.row = srow;
        self.col = scol;
    }

    fn snapshot(&mut self) {
        self.undo_stack
            .push((self.lines.clone(), self.row, self.col));
        if self.undo_stack.len() > UNDO_MAX {
            self.undo_stack.remove(0);
        }
    }

    fn undo(&mut self) {
        if let Some((lines, row, col)) = self.undo_stack.pop() {
            self.lines = lines;
            self.row = row;
            self.col = col;
        }
    }

    fn move_word_back(&mut self) {
        if self.col == 0 {
            self.move_left();
        } else {
            self.col = self.word_back_col();
        }
    }

    fn move_word_forward(&mut self) {
        let count = grapheme_count(&self.lines[self.row]);
        if self.col == count {
            self.move_right();
        } else {
            self.col = self.word_forward_col();
        }
    }

    fn jump_to_char(&mut self, target: char, dir: JumpDir) {
        let graphemes: Vec<&str> = self.lines[self.row].graphemes(true).collect();
        let hit = |g: &&str| g.starts_with(target);
        let found = match dir {
            JumpDir::Forward => graphemes
                .iter()
                .enumerate()
                .skip(self.col + 1)
                .find(|(_, g)| hit(g))
                .map(|(i, _)| i),
            JumpDir::Backward => graphemes
                .iter()
                .enumerate()
                .take(self.col)
                .rev()
                .find(|(_, g)| hit(g))
                .map(|(i, _)| i),
        };
        if let Some(i) = found {
            self.col = i;
        }
    }

    fn word_back_col(&self) -> usize {
        let graphemes: Vec<&str> = self.lines[self.row].graphemes(true).collect();
        let mut i = self.col;
        while i > 0 && !is_word(graphemes[i - 1]) {
            i -= 1;
        }
        while i > 0 && is_word(graphemes[i - 1]) {
            i -= 1;
        }
        i
    }

    fn word_forward_col(&self) -> usize {
        let graphemes: Vec<&str> = self.lines[self.row].graphemes(true).collect();
        let mut i = self.col;
        while i < graphemes.len() && !is_word(graphemes[i]) {
            i += 1;
        }
        while i < graphemes.len() && is_word(graphemes[i]) {
            i += 1;
        }
        i
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

fn is_word(grapheme: &str) -> bool {
    grapheme.chars().any(|c| c.is_alphanumeric() || c == '_')
}

fn grapheme_count(s: &str) -> usize {
    s.graphemes(true).count()
}

fn byte_index(s: &str, grapheme_idx: usize) -> usize {
    s.grapheme_indices(true)
        .nth(grapheme_idx)
        .map_or(s.len(), |(i, _)| i)
}

fn width_of_graphemes(s: &str, count: usize) -> usize {
    s.graphemes(true).take(count).map(|g| g.width()).sum()
}

fn grapheme_index_at_width(s: &str, target_width: usize) -> usize {
    if target_width == 0 {
        return 0;
    }
    let mut width = 0;
    let mut index = 0;
    for grapheme in s.graphemes(true) {
        width += grapheme.width();
        index += 1;
        if width >= target_width {
            return index;
        }
    }
    index
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
    fn grapheme_aware_cursor_and_backspace() {
        let mut ed = Editor::new();
        ed.insert_str("aé日");
        assert_eq!(ed.render().1, (0, 4)); // 1 + 1 + 2 display cols
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
    fn vertical_movement_preserves_preferred_column() {
        let mut ed = Editor::new();
        ed.set_text("abcde\nx\nabcde");
        assert_eq!(ed.render().1, (2, 5));

        ed.handle_key(key(KeyCode::Up));
        assert_eq!(ed.render().1, (1, 1));

        ed.handle_key(key(KeyCode::Up));
        assert_eq!(ed.render().1, (0, 5));

        ed.handle_key(key(KeyCode::Down));
        assert_eq!(ed.render().1, (1, 1));

        ed.handle_key(key(KeyCode::Down));
        assert_eq!(ed.render().1, (2, 5));
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
    fn kill_ring_yank_and_pop() {
        let mut ed = Editor::new();
        type_str(&mut ed, "alpha beta");
        ed.handle_key(ctrl('w')); // kill "beta"
        ed.handle_key(ctrl('u')); // kill "alpha "
        assert_eq!(ed.text(), "");
        ed.handle_key(ctrl('y'));
        assert_eq!(ed.text(), "alpha ");
        ed.handle_key(alt('y')); // rotate to "beta"
        assert_eq!(ed.text(), "beta");
        ed.handle_key(alt('y')); // wrap back to "alpha "
        assert_eq!(ed.text(), "alpha ");
        // alt-y without a preceding yank is a no-op.
        type_str(&mut ed, "x");
        ed.handle_key(alt('y'));
        assert_eq!(ed.text(), "alpha x");
    }

    #[test]
    fn undo_reverts_edits() {
        let mut ed = Editor::new();
        type_str(&mut ed, "ab");
        ed.handle_key(ctrl('w'));
        assert_eq!(ed.text(), "");
        let undo = KeyEvent::new(KeyCode::Char('-'), KeyModifiers::CONTROL);
        ed.handle_key(undo);
        assert_eq!(ed.text(), "ab");
        ed.handle_key(undo);
        assert_eq!(ed.text(), "a");
        ed.handle_key(undo);
        assert_eq!(ed.text(), "");
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
    fn char_jump_forward_and_backward() {
        let mut ed = Editor::new();
        type_str(&mut ed, "abc def abc");
        ed.handle_key(ctrl('a'));
        let trigger = KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL);
        ed.handle_key(trigger);
        ed.handle_key(key(KeyCode::Char('d')));
        assert_eq!(ed.render().1, (0, 4));
        // Backward jump (ctrl+alt+]).
        ed.handle_key(ctrl('e'));
        ed.handle_key(KeyEvent::new(
            KeyCode::Char(']'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        ed.handle_key(key(KeyCode::Char('d')));
        assert_eq!(ed.render().1, (0, 4));
        // Trigger again cancels: the 'x' is not inserted by the jump path
        // but typed normally afterwards.
        ed.handle_key(trigger);
        ed.handle_key(trigger);
        ed.handle_key(key(KeyCode::Char('x')));
        assert!(ed.text().contains('x'));
    }

    #[test]
    fn small_paste_inserts_directly() {
        let mut ed = Editor::new();
        ed.insert_paste("hello\nworld");
        assert_eq!(ed.text(), "hello\nworld");
    }

    #[test]
    fn large_paste_collapses_to_marker_and_expands_on_submit() {
        let mut ed = Editor::new();
        let big: String = (0..20).map(|i| format!("line {i}\n")).collect();
        ed.insert_paste(&big);
        assert_eq!(ed.text(), "[paste #1 +21 lines]");
        type_str(&mut ed, " tail");
        let action = ed.handle_key(key(KeyCode::Enter));
        let EditorAction::Submit(text) = action else {
            panic!("expected submit");
        };
        assert!(text.starts_with("line 0\n"));
        assert!(text.ends_with(" tail"));
        assert!(!text.contains("[paste"));
    }

    #[test]
    fn char_paste_marker_counts_chars() {
        let mut ed = Editor::new();
        let big = "x".repeat(1500);
        ed.insert_paste(&big);
        assert_eq!(ed.text(), "[paste #1 1500 chars]");
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
}

