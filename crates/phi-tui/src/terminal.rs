//! Inline terminal renderer: finalized lines are flushed permanently into
//! scrollback, a live region at the bottom is redrawn each frame. No alt
//! screen — output stays in the terminal's history, like phi's TUI.

use std::io::Write;

use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";
const HIDE_CURSOR: &str = "\x1b[?25l";
const SHOW_CURSOR: &str = "\x1b[?25h";
const CLEAR_DOWN: &str = "\x1b[0J";
const CLEAR_LINE: &str = "\x1b[2K";

/// Enables raw mode, bracketed paste, and (where supported) the kitty
/// keyboard protocol so chords like shift+enter are distinguishable.
/// Restores the terminal on drop.
pub struct RawModeGuard {
    kitty: bool,
}

impl RawModeGuard {
    pub fn new() -> std::io::Result<Self> {
        enable_raw_mode()?;
        let kitty = matches!(
            crossterm::terminal::supports_keyboard_enhancement(),
            Ok(true)
        );
        let guard = Self { kitty };
        guard.setup();
        Ok(guard)
    }

    fn setup(&self) {
        let mut out = std::io::stdout();
        let _ = enable_raw_mode();
        let _ = crossterm::execute!(out, EnableBracketedPaste);
        if self.kitty {
            let _ = crossterm::execute!(
                out,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            );
        }
    }

    fn teardown(&self) {
        let mut out = std::io::stdout();
        if self.kitty {
            let _ = crossterm::execute!(out, PopKeyboardEnhancementFlags);
        }
        let _ = crossterm::execute!(out, DisableBracketedPaste);
        let _ = disable_raw_mode();
    }

    /// Stop the process (SIGTSTP) with the terminal restored; re-enters raw
    /// mode when the shell resumes us with SIGCONT.
    #[cfg(unix)]
    pub fn suspend(&self) {
        self.teardown();
        unsafe { libc::raise(libc::SIGTSTP) };
        self.setup();
    }

    /// Run `f` with the terminal restored to cooked mode (e.g. to hand the
    /// tty to an external editor), re-entering raw mode afterwards.
    pub fn with_suspended<R>(&self, f: impl FnOnce() -> R) -> R {
        self.teardown();
        let result = f();
        self.setup();
        result
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        self.teardown();
    }
}

/// Cursor position within the live region, in logical-line coordinates:
/// `line` indexes into the `live` slice, `col` is a display column.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LiveCursor {
    pub line: usize,
    pub col: usize,
}

pub struct Terminal<W: Write> {
    out: W,
    width: usize,
    height: usize,
    /// Wrapped live-region rows from the previous render, for line diffing.
    prev: Vec<String>,
    /// Row (0-based, within the live region) where the cursor was parked.
    cursor_row: usize,
    /// Column (0-based) where the cursor was parked.
    cursor_col: usize,
    /// Whether the cursor was left visible by the previous render.
    cursor_shown: bool,
}

impl<W: Write> Terminal<W> {
    pub fn new(out: W, width: u16, height: u16) -> Self {
        Self {
            out,
            width: width.max(1) as usize,
            height: height.max(1) as usize,
            prev: Vec::new(),
            cursor_row: 0,
            cursor_col: 0,
            cursor_shown: false,
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        self.width = width.max(1) as usize;
        self.height = height.max(1) as usize;
        // Reflow invalidates row accounting; drop the live region and let the
        // next render redraw from wherever the terminal reflowed us to.
        self.prev.clear();
        self.cursor_row = 0;
        self.cursor_col = 0;
    }

    /// Flush `scrollback` lines permanently above and redraw the live region.
    /// `cursor: None` hides the cursor (e.g. while the model is streaming).
    ///
    /// When no scrollback is pending, the new frame is diffed against the
    /// previous one and only the changed rows are rewritten (pi-style), so
    /// streaming deltas and spinner ticks touch a handful of lines instead of
    /// repainting the whole region.
    pub fn render(
        &mut self,
        scrollback: &[String],
        live: &[String],
        cursor: Option<LiveCursor>,
    ) -> std::io::Result<()> {
        let mut rows = Vec::new();
        let mut cursor_target = None;
        for (i, line) in live.iter().enumerate() {
            let wrapped = wrap_ansi(line, self.width);
            if let Some(c) = cursor
                && c.line == i
            {
                let col = c.col.min(visible_width(line));
                cursor_target = Some((rows.len() + col / self.width, col % self.width));
            }
            rows.extend(wrapped);
        }
        // The live region is repainted by moving the cursor back up, which
        // cannot cross the top of the viewport: drop overflow rows from the
        // top so accounting stays truthful.
        if rows.len() > self.height {
            let dropped = rows.len() - self.height;
            rows.drain(..dropped);
            cursor_target = cursor_target.map(|(row, col)| (row.saturating_sub(dropped), col));
        }

        // Park the cursor: either at the requested position or at the end of
        // the last row.
        let last_row = rows.len().saturating_sub(1);
        let (target_row, target_col) = match cursor_target {
            Some((row, col)) => (row.min(last_row), col),
            None => (last_row, 0),
        };

        // Scrollback pushes the live region down, so it forces a repaint from
        // the origin; otherwise diff against the previous frame.
        let full = !scrollback.is_empty() || self.prev.is_empty();
        if !full
            && rows == self.prev
            && target_row == self.cursor_row
            && target_col == self.cursor_col
            && cursor.is_some() == self.cursor_shown
        {
            return Ok(());
        }

        let mut buf = String::new();
        buf.push_str(SYNC_BEGIN);
        buf.push_str(HIDE_CURSOR);
        let mut cur = self.cursor_row;

        if full {
            self.move_to_origin(&mut buf);
            buf.push_str(CLEAR_DOWN);
            for line in scrollback {
                buf.push_str(line);
                buf.push_str("\r\n");
            }
            for (i, row) in rows.iter().enumerate() {
                if i > 0 {
                    buf.push_str("\r\n");
                }
                buf.push_str(row);
            }
            cur = last_row;
        } else {
            let common = rows.len().min(self.prev.len());
            let first = (0..common).find(|&i| rows[i] != self.prev[i]);
            let last = (0..common).rev().find(|&i| rows[i] != self.prev[i]);
            let write = match (first, rows.len() > self.prev.len()) {
                (Some(i), true) => Some((i, last_row)),
                (None, true) => Some((common, last_row)),
                (Some(i), false) => Some((i, last.unwrap_or(i))),
                (None, false) => None,
            };
            if let Some((start, end)) = write {
                // Position on the last row that already exists on screen so
                // the cursor can reach it; rows appended past the previous
                // frame are created by the \r\n writes (scrolling at the
                // bottom of the viewport).
                let anchor = start.min(self.prev.len() - 1);
                move_rows(&mut buf, cur, anchor);
                for (i, row) in rows.iter().enumerate().take(end + 1).skip(start) {
                    if i > anchor {
                        buf.push_str("\r\n");
                    }
                    buf.push_str(CLEAR_LINE);
                    buf.push_str(row);
                }
                cur = end;
            }
            if rows.len() < self.prev.len() {
                // The region shrank: blank the leftover rows below.
                move_rows(&mut buf, cur, rows.len());
                buf.push_str(CLEAR_DOWN);
                cur = rows.len();
            }
        }

        move_rows(&mut buf, cur, target_row);
        buf.push_str(&format!("\x1b[{}G", target_col + 1));
        if cursor.is_some() {
            buf.push_str(SHOW_CURSOR);
        }
        buf.push_str(SYNC_END);

        self.prev = rows;
        self.cursor_row = target_row;
        self.cursor_col = target_col;
        self.cursor_shown = cursor.is_some();
        self.out.write_all(buf.as_bytes())?;
        self.out.flush()
    }

    /// Clear the live region and leave the cursor at its origin (call before
    /// exiting so the shell prompt lands cleanly under the scrollback).
    pub fn finish(&mut self) -> std::io::Result<()> {
        let mut buf = String::new();
        self.move_to_origin(&mut buf);
        buf.push_str(CLEAR_DOWN);
        buf.push_str(SHOW_CURSOR);
        self.prev.clear();
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.cursor_shown = true;
        self.out.write_all(buf.as_bytes())?;
        self.out.flush()
    }

    fn move_to_origin(&self, buf: &mut String) {
        buf.push('\r');
        if self.cursor_row > 0 {
            buf.push_str(&format!("\x1b[{}A", self.cursor_row));
        }
    }
}

/// Emit cursor movement from live-region row `from` to row `to`, landing in
/// column 0. Both rows must already exist on screen.
fn move_rows(buf: &mut String, from: usize, to: usize) {
    if to < from {
        buf.push_str(&format!("\x1b[{}A", from - to));
    } else if to > from {
        buf.push_str(&format!("\x1b[{}B", to - from));
    }
    buf.push('\r');
}

/// Display width of `s`, ignoring ANSI escape sequences.
pub fn visible_width(s: &str) -> usize {
    strip_ansi(s).width()
}

/// Remove ANSI escape sequences (CSI and OSC) from `s`.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            // CSI: ESC [ ... final byte in @..~
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: ESC ] ... BEL or ESC \
            Some(']') => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {
                chars.next();
            }
        }
    }
    out
}

/// Hard-wrap a (possibly ANSI-styled) line into rows of at most `width`
/// display columns. Escape sequences carry zero width and are never split.
pub fn wrap_ansi(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut col = 0;

    let mut rest = line;
    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix('\x1b') {
            let len = escape_len(stripped) + 1;
            row.push_str(&rest[..len]);
            rest = &rest[len..];
            continue;
        }
        let grapheme = rest.graphemes(true).next().unwrap();
        let gw = grapheme.width();
        if col + gw > width && col > 0 {
            rows.push(std::mem::take(&mut row));
            col = 0;
        }
        row.push_str(grapheme);
        col += gw;
        rest = &rest[grapheme.len()..];
    }
    rows.push(row);
    rows
}

/// Byte length of an escape sequence body (after the ESC byte).
fn escape_len(s: &str) -> usize {
    let mut chars = s.char_indices();
    match chars.next() {
        Some((_, '[')) => {
            for (i, c) in chars {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    return i + c.len_utf8();
                }
            }
            s.len()
        }
        Some((_, ']')) => {
            let mut prev_esc = false;
            for (i, c) in chars {
                if c == '\x07' {
                    return i + 1;
                }
                if prev_esc && c == '\\' {
                    return i + 1;
                }
                prev_esc = c == '\x1b';
            }
            s.len()
        }
        Some((_, c)) => c.len_utf8(),
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_width_ignores_ansi() {
        assert_eq!(visible_width("plain"), 5);
        assert_eq!(visible_width("\x1b[1mbold\x1b[0m"), 4);
        assert_eq!(visible_width("\x1b]8;;http://x\x07link\x1b]8;;\x07"), 4);
        assert_eq!(visible_width("héllo"), 5);
        assert_eq!(visible_width("日本"), 4);
    }

    #[test]
    fn wrap_ansi_wraps_by_display_columns() {
        assert_eq!(wrap_ansi("abcdef", 3), vec!["abc", "def"]);
        assert_eq!(wrap_ansi("", 3), vec![""]);
        assert_eq!(wrap_ansi("ab", 3), vec!["ab"]);
        // Wide chars never split across rows.
        assert_eq!(wrap_ansi("日本語", 4), vec!["日本", "語"]);
    }

    #[test]
    fn wrap_ansi_keeps_escapes_zero_width() {
        let rows = wrap_ansi("\x1b[1mabc\x1b[0mdef", 3);
        assert_eq!(rows, vec!["\x1b[1mabc\x1b[0m", "def"]);
    }

    #[test]
    fn render_tracks_live_rows_and_cursor() {
        let mut term = Terminal::new(Vec::new(), 10, 24);
        term.render(
            &["done line".to_owned()],
            &["> hello".to_owned(), "status".to_owned()],
            Some(LiveCursor { line: 0, col: 7 }),
        )
        .unwrap();
        assert_eq!(term.prev.len(), 2);
        assert_eq!(term.cursor_row, 0);
        let out = String::from_utf8(term.out.clone()).unwrap();
        assert!(out.contains("done line\r\n"));
        assert!(out.contains("> hello"));
        assert!(out.contains(SYNC_BEGIN) && out.contains(SYNC_END));
        // Cursor moved up from row 1 to row 0, column 8 (1-based).
        assert!(out.contains("\x1b[1A"));
        assert!(out.contains("\x1b[8G"));
    }

    #[test]
    fn render_accounts_for_wrapped_live_lines() {
        let mut term = Terminal::new(Vec::new(), 4, 24);
        term.render(
            &[],
            &["abcdefgh".to_owned()],
            Some(LiveCursor { line: 0, col: 6 }),
        )
        .unwrap();
        // 8 cols at width 4 → 2 rows; cursor at col 6 → row 1, col 2.
        assert_eq!(term.prev.len(), 2);
        assert_eq!(term.cursor_row, 1);
    }

    #[test]
    fn render_clamps_live_region_to_height() {
        let mut term = Terminal::new(Vec::new(), 10, 3);
        let live: Vec<String> = (0..6).map(|i| format!("line{i}")).collect();
        term.render(&[], &live, Some(LiveCursor { line: 5, col: 2 }))
            .unwrap();
        // Only the bottom 3 rows survive; cursor stays addressable.
        assert_eq!(term.prev.len(), 3);
        assert_eq!(term.cursor_row, 2);
        let out = String::from_utf8(term.out.clone()).unwrap();
        assert!(!out.contains("line0"));
        assert!(out.contains("line5"));
    }

    #[test]
    fn second_render_returns_to_origin_first() {
        let mut term = Terminal::new(Vec::new(), 20, 24);
        term.render(
            &[],
            &["a".to_owned(), "b".to_owned(), "c".to_owned()],
            Some(LiveCursor { line: 0, col: 1 }),
        )
        .unwrap();
        term.out.clear();
        term.render(&[], &["x".to_owned()], None).unwrap();
        let out = String::from_utf8(term.out.clone()).unwrap();
        // The region shrank from 3 rows to 1: row 0 is rewritten in place and
        // the leftover rows below are cleared.
        assert!(out.starts_with(SYNC_BEGIN));
        assert!(out.contains(CLEAR_DOWN));
        assert!(out.contains("x"));
        assert_eq!(term.prev.len(), 1);
    }

    #[test]
    fn diff_rewrites_only_changed_rows() {
        let mut term = Terminal::new(Vec::new(), 20, 24);
        let live = |s: &str| vec!["alpha".to_owned(), s.to_owned(), "gamma".to_owned()];
        term.render(&[], &live("beta"), None).unwrap();
        term.out.clear();
        term.render(&[], &live("BETA"), None).unwrap();
        let out = String::from_utf8(term.out.clone()).unwrap();
        assert!(out.contains("BETA"));
        assert!(!out.contains("alpha"));
        assert!(!out.contains("gamma"));
        assert!(!out.contains(CLEAR_DOWN));
    }

    #[test]
    fn diff_appends_without_rewriting_existing_rows() {
        let mut term = Terminal::new(Vec::new(), 20, 24);
        term.render(&[], &["one".to_owned()], None).unwrap();
        term.out.clear();
        term.render(&[], &["one".to_owned(), "two".to_owned()], None)
            .unwrap();
        let out = String::from_utf8(term.out.clone()).unwrap();
        assert!(out.contains("two"));
        assert!(!out.contains("one"));
        assert_eq!(term.prev.len(), 2);
        assert_eq!(term.cursor_row, 1);
    }

    #[test]
    fn unchanged_frame_writes_nothing() {
        let mut term = Terminal::new(Vec::new(), 20, 24);
        let live = vec!["a".to_owned(), "b".to_owned()];
        let cursor = Some(LiveCursor { line: 0, col: 1 });
        term.render(&[], &live, cursor).unwrap();
        term.out.clear();
        term.render(&[], &live, cursor).unwrap();
        assert!(term.out.is_empty());
    }

    #[test]
    fn scrollback_forces_full_repaint() {
        let mut term = Terminal::new(Vec::new(), 20, 24);
        let live = vec!["live".to_owned()];
        term.render(&[], &live, None).unwrap();
        term.out.clear();
        term.render(&["flushed".to_owned()], &live, None).unwrap();
        let out = String::from_utf8(term.out.clone()).unwrap();
        assert!(out.contains("flushed\r\n"));
        assert!(out.contains(CLEAR_DOWN));
        assert!(out.contains("live"));
    }
}
