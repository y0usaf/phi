//! Inline terminal renderer: finalized lines are flushed permanently into
//! scrollback, a live region at the bottom is redrawn each frame. No alt
//! screen — output stays in the terminal's history, like phi's TUI.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
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

/// True when PHI_DEBUG_REDRAW=1 is set in the environment. Checked once
/// (not per-frame) for speed.
fn debug_redraw_enabled() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var("PHI_DEBUG_REDRAW").is_ok_and(|v| v == "1"))
}

/// Log a debug message to stderr (so it doesn't corrupt the terminal).
fn debug_log(msg: &str) {
    eprintln!("phi:debug: {msg}");
}

/// Enables raw mode, bracketed paste, and (where supported) the kitty
/// keyboard protocol so chords like shift+enter are distinguishable.
/// Restores the terminal on drop.
pub struct RawModeGuard {
    kitty: bool,
}

/// Whether a `RawModeGuard` currently has the terminal in raw mode, and
/// whether kitty flags were pushed — recorded globally so a panic hook or
/// signal handler can restore the terminal without a guard reference
/// (signals and panics don't run `Drop`).
static RESTORE_ACTIVE: AtomicBool = AtomicBool::new(false);
static RESTORE_KITTY: AtomicBool = AtomicBool::new(false);

/// Restore the terminal to cooked mode from anywhere: panic hooks and
/// signal handlers, where the guard itself is out of reach. Idempotent,
/// and a no-op while no guard is active (e.g. embedded mode). Goes beyond
/// the guard's own teardown because the process may die mid-frame: closes
/// a possibly-open synchronized update, re-shows the cursor, and drops
/// mouse capture (the mux enables that outside the guard).
pub fn emergency_restore() {
    if !RESTORE_ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut out = std::io::stdout();
    if RESTORE_KITTY.load(Ordering::SeqCst) {
        let _ = crossterm::execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = crossterm::execute!(out, DisableMouseCapture);
    let _ = crossterm::execute!(out, DisableFocusChange);
    let _ = crossterm::execute!(out, DisableBracketedPaste);
    let _ = write!(out, "{SYNC_END}{SHOW_CURSOR}");
    let _ = out.flush();
    let _ = disable_raw_mode();
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
        let _ = crossterm::execute!(out, EnableFocusChange);
        if self.kitty {
            let _ = crossterm::execute!(
                out,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            );
        }
        RESTORE_KITTY.store(self.kitty, Ordering::SeqCst);
        RESTORE_ACTIVE.store(true, Ordering::SeqCst);
    }

    fn teardown(&self) {
        RESTORE_ACTIVE.store(false, Ordering::SeqCst);
        let mut out = std::io::stdout();
        if self.kitty {
            let _ = crossterm::execute!(out, PopKeyboardEnhancementFlags);
        }
        let _ = crossterm::execute!(out, DisableFocusChange);
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
        // After SIGCONT, send SIGWINCH to ourselves so the event loop
        // re-reads the terminal size. SIGWINCH may have been lost while
        // suspended (the process wasn't receiving signals).
        unsafe { libc::raise(libc::SIGWINCH) };
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
    /// When true, output goes to an embedded vt100 parser (via ParserWriter),
    /// not a real terminal. In this mode the line-diff and SYNC markers are
    /// pure waste: the mux re-derives everything from vt100 cells and emits
    /// its own outer SYNC wrapper, and the vt100 parser ignores ?2026.
    embedded: bool,
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
            embedded: false,
        }
    }

    /// Mark this terminal as embedded in a mux's vt100 parser. In embedded
    /// mode, the line-diff and synchronized-update markers are skipped:
    /// the mux composites from vt100 cells and emits its own SYNC wrapper.
    pub fn set_embedded(&mut self, embedded: bool) {
        self.embedded = embedded;
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        // Skip same-dimension resize events: terminals fire several per
        // window adjustment and each would leave a stale chrome copy.
        let (w, h) = (width.max(1) as usize, height.max(1) as usize);
        if self.width == w && self.height == h {
            return;
        }
        self.width = w;
        self.height = h;
        // Reflow invalidates row accounting; the caller (events.rs) must
        // run `clear_viewport()` so the physical cursor is homed to (0,0)
        // — matching `cursor_row = 0` below — before the next render.
        // Saved scrollback is intentionally preserved.
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
            let wrapped = if let Some(c) = cursor
                && c.line == i
            {
                // Locate the cursor through the word-wrap layout (pi-style);
                // dividing the column by the width is wrong as soon as a
                // break drops whitespace or refuses to split a word.
                let col = c.col.min(visible_width(line));
                let (wrapped, (row, col)) = wrap_ansi_locate(line, self.width, col);
                cursor_target = Some((rows.len() + row, col.min(self.width)));
                wrapped
            } else {
                wrap_ansi(line, self.width)
            };
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

        if debug_redraw_enabled() {
            for (i, row) in rows.iter().enumerate() {
                let w = visible_width(row);
                if w > self.width {
                    debug_log(&format!(
                        "width overflow: row {i} is {w} cols (term {})",
                        self.width
                    ));
                }
            }
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
        // In embedded mode, skip the early-return: the mux composites from
        // vt100 cells, so this diff is pure waste. The vt100 parser handles
        // cell-level diffing.
        let full = !scrollback.is_empty() || self.prev.is_empty() || self.embedded;
        if !full
            && rows == self.prev
            && target_row == self.cursor_row
            && target_col == self.cursor_col
            && cursor.is_some() == self.cursor_shown
        {
            return Ok(());
        }

        if debug_redraw_enabled() && full {
            debug_log(&format!(
                "full repaint: scrollback={} prev_empty={} embedded={}",
                scrollback.len(),
                self.prev.is_empty(),
                self.embedded
            ));
        }

        let mut buf = String::new();
        // Skip SYNC markers in embedded mode: the vt100 parser ignores ?2026
        // and the mux emits its own outer SYNC wrapper.
        if !self.embedded {
            buf.push_str(SYNC_BEGIN);
        }
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
                if debug_redraw_enabled() {
                    debug_log(&format!("diff repaint: rows {start}..{end}"));
                }
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
        if target_col >= self.width && target_row < rows.len() {
            // CHA (\x1b[NG) clamps to width-1, but the cursor may need
            // to sit at the "phantom" column == width (deferred-wrap
            // position) when text fills the row exactly. Re-emit the
            // row content to push the cursor there via the terminal's
            // deferred wrap instead of CHA, which would clamp to
            // width-1 and freeze the cursor one cell short.
            buf.push_str(CLEAR_LINE);
            buf.push_str(&rows[target_row]);
        } else {
            buf.push_str(&format!("\x1b[{}G", target_col + 1));
        }
        if cursor.is_some() {
            buf.push_str(SHOW_CURSOR);
        }
        if !self.embedded {
            buf.push_str(SYNC_END);
        }

        self.out.write_all(buf.as_bytes())?;
        self.out.flush()?;
        self.prev = rows;
        self.cursor_row = target_row;
        self.cursor_col = target_col;
        self.cursor_shown = cursor.is_some();
        Ok(())
    }

    /// Clear the live region and leave the cursor at its origin (call before
    /// exiting so the shell prompt lands cleanly under the scrollback).
    pub fn finish(&mut self) -> std::io::Result<()> {
        let mut buf = String::new();
        self.move_to_origin(&mut buf);
        buf.push_str(CLEAR_DOWN);
        buf.push_str(SHOW_CURSOR);
        self.out.write_all(buf.as_bytes())?;
        self.out.flush()?;
        self.prev.clear();
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.cursor_shown = true;
        Ok(())
    }

    /// Wipe the whole screen (viewport + scrollback) and home the cursor,
    /// resetting frame accounting so the next render repaints from a blank
    /// terminal. Used when replacing the transcript wholesale (e.g. /resume),
    /// so old sessions don't pile up in scrollback.
    pub fn clear_all(&mut self) -> std::io::Result<()> {
        // \x1b[2J clears the viewport, \x1b[3J drops saved scrollback,
        // \x1b[H homes the cursor.
        self.out.write_all(b"\x1b[2J\x1b[3J\x1b[H")?;
        self.out.flush()?;
        self.prev.clear();
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.cursor_shown = false;
        Ok(())
    }

    /// Clear the viewport and home the cursor WITHOUT touching saved
    /// scrollback (no `\x1b[3J`). Used on resize: the transcript re-renders
    /// at the new width, but the user's scrollback — including everything
    /// from before phi started — survives. A stale copy of the transcript
    /// may remain above, which is how ordinary CLI output behaves too.
    pub fn clear_viewport(&mut self) -> std::io::Result<()> {
        self.out.write_all(b"\x1b[2J\x1b[H")?;
        self.out.flush()?;
        self.prev.clear();
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.cursor_shown = false;
        Ok(())
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

/// Drain lingering input from stdin. Call AFTER terminal restoration
/// (teardown) to absorb straggler key events that are still in flight over
/// slow SSH, preventing them from leaking into the parent shell as garbage
/// characters or unintended commands.
#[cfg(unix)]
pub fn drain_input() {
    use std::os::fd::AsRawFd;
    let fd = std::io::stdin().as_raw_fd();
    let mut buf = [0u8; 4096];
    let mut total = 0u32;
    loop {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ret = unsafe { libc::poll(&mut pfd, 1, 50) }; // 50ms timeout
        if ret <= 0 || pfd.revents & libc::POLLIN == 0 {
            break;
        }
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
        total += n as u32;
        if total > 65536 {
            break; // Safety cap: don't drain forever
        }
    }
}

#[cfg(not(unix))]
pub fn drain_input() {}

/// Paint `row` over the SGR background sequence `bg`, padding with spaces to
/// `width` visible columns. Full resets inside the row re-open the
/// background so it spans the whole padded row.
pub fn fill_bg(row: &str, bg: &str, width: usize) -> String {
    let pad = width.saturating_sub(visible_width(row));
    let body = row.replace("\x1b[0m", &format!("\x1b[0m{bg}"));
    format!("{bg}{body}{:pad$}\x1b[0m", "")
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

/// Active terminal styling at a point in a styled string: SGR attributes
/// since the last reset, plus any open OSC 8 hyperlink. Re-emitted at the
/// start of wrapped continuation rows so styles survive the break even when
/// callers interleave their own styled prefixes.
type WrapMarks = Vec<(usize, usize, usize)>;
type WrappedAnsi = (Vec<String>, WrapMarks, (usize, usize));

#[derive(Clone, Default)]
struct AnsiState {
    sgr: Vec<String>,
    link: Option<String>,
}

impl AnsiState {
    fn apply(&mut self, esc: &str) {
        if let Some(body) = esc.strip_prefix("\x1b[") {
            if body.ends_with('m') {
                if matches!(body, "m" | "0m") {
                    self.sgr.clear();
                } else {
                    self.sgr.push(esc.to_owned());
                }
            }
        } else if let Some(body) = esc.strip_prefix("\x1b]8;") {
            let uri = body
                .trim_end_matches('\x07')
                .trim_end_matches("\x1b\\")
                .split_once(';')
                .map_or("", |(_, uri)| uri);
            self.link = (!uri.is_empty()).then(|| esc.to_owned());
        }
    }

    fn reopen(&self) -> String {
        let mut s = self.link.clone().unwrap_or_default();
        for code in &self.sgr {
            s.push_str(code);
        }
        s
    }
}

/// Row-building cursor for `wrap_ansi`.
struct Wrapper {
    width: usize,
    rows: Vec<String>,
    row: String,
    col: usize,
    /// `(source display column, row index, row column)` for every visible
    /// grapheme placed, plus trailing-space and end-of-text marks. Maps a
    /// display column of the unwrapped line to its wrapped position
    /// (pi-style: the cursor is located through the wrap layout, never by
    /// dividing the column by the width, which breaks under word-wrap).
    marks: Vec<(usize, usize, usize)>,
}

impl Wrapper {
    /// Emit one committed word (escapes included), preceded by its pending
    /// spaces; wraps before the word when it would overflow, mid-word when
    /// it cannot fit on a row of its own. `src` is the source display column
    /// of the first pending space.
    fn place(
        &mut self,
        word: &str,
        word_w: usize,
        spaces: usize,
        word_state: &AnsiState,
        src: usize,
    ) {
        let mut src = src;
        if self.col > 0 && self.col + spaces + word_w > self.width {
            // The break drops the spaces from the output. Keep them
            // addressable on the continuation row (`break_row` is about to
            // create) at columns 0..spaces, so the cursor advances to the
            // next row when it sits on a space that triggers a wrap rather
            // than freezing at the last column of the current row.
            for i in 0..spaces {
                self.marks.push((src + i, self.rows.len() + 1, i));
            }
            src += spaces;
            self.break_row(word_state);
        } else {
            for _ in 0..spaces {
                self.marks.push((src, self.rows.len(), self.col));
                self.row.push(' ');
                self.col += 1;
                src += 1;
            }
        }
        let mut local = word_state.clone();
        let mut rest = word;
        while !rest.is_empty() {
            if let Some(stripped) = rest.strip_prefix('\x1b') {
                let len = escape_len(stripped) + 1;
                self.row.push_str(&rest[..len]);
                local.apply(&rest[..len]);
                rest = &rest[len..];
                continue;
            }
            let grapheme = rest.graphemes(true).next().unwrap();
            let gw = grapheme.width();
            if self.col + gw > self.width && self.col > 0 {
                self.break_row(&local);
            }
            self.marks.push((src, self.rows.len(), self.col));
            self.row.push_str(grapheme);
            self.col += gw;
            src += gw;
            rest = &rest[grapheme.len()..];
        }
    }

    fn break_row(&mut self, state: &AnsiState) {
        self.rows.push(std::mem::take(&mut self.row));
        self.row.push_str(&state.reopen());
        self.col = 0;
    }
}

/// Wrap a (possibly ANSI-styled) line into rows of at most `width` display
/// columns. Breaks at word boundaries when possible, splitting per grapheme
/// only for words wider than a row; the whitespace at each break is dropped
/// (leading indentation is preserved) and active SGR/hyperlink state is
/// re-opened on every continuation row. Escape sequences carry zero width.
pub fn wrap_ansi(line: &str, width: usize) -> Vec<String> {
    wrap_ansi_core(line, width).0
}

/// `wrap_ansi`, plus the wrapped (row, column) for a display column of the
/// source line — the position the cursor must take after word-wrap. Columns
/// past the end of the text park after the last grapheme.
pub fn wrap_ansi_locate(line: &str, width: usize, col: usize) -> (Vec<String>, (usize, usize)) {
    let (rows, marks, end) = wrap_ansi_core(line, width);
    let pos = marks
        .iter()
        .find(|&&(src, _, _)| src >= col)
        .map(|&(_, row, c)| (row, c))
        .unwrap_or(end);
    (rows, pos)
}

fn wrap_ansi_core(line: &str, width: usize) -> WrappedAnsi {
    let mut w = Wrapper {
        width: width.max(1),
        rows: Vec::new(),
        row: String::new(),
        col: 0,
        marks: Vec::new(),
    };

    let mut state = AnsiState::default();
    let mut word_state = AnsiState::default();
    let mut word = String::new();
    let mut word_w = 0;
    let mut spaces = 0;
    // Source display column of the first pending space (or of the pending
    // word when there are no pending spaces).
    let mut src = 0;
    let mut consumed = 0;

    let mut rest = line;
    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix('\x1b') {
            // Escapes attach to the upcoming word so they are never dropped
            // with the whitespace at a wrap point.
            let len = escape_len(stripped) + 1;
            word.push_str(&rest[..len]);
            state.apply(&rest[..len]);
            rest = &rest[len..];
            continue;
        }
        if let Some(r) = rest.strip_prefix(' ') {
            if !word.is_empty() {
                w.place(&word, word_w, spaces, &word_state, src);
                src = consumed;
                word.clear();
                word_w = 0;
                spaces = 0;
            }
            spaces += 1;
            consumed += 1;
            word_state = state.clone();
            rest = r;
            continue;
        }
        let grapheme = rest.graphemes(true).next().unwrap();
        word.push_str(grapheme);
        word_w += grapheme.width();
        consumed += grapheme.width();
        rest = &rest[grapheme.len()..];
    }
    if !word.is_empty() {
        w.place(&word, word_w, spaces, &word_state, src);
        src = consumed - spaces;
        spaces = 0;
    }
    // Trailing spaces remain addressable so the cursor can sit on / after
    // them. Include them in the row content so re-emitting the row positions
    // the cursor at the phantom column (width) via deferred wrap, which CHA
    // cannot reach (it clamps to width-1). Wrap excess spaces to the next row.
    let _ = src;
    for i in 0..spaces {
        if w.col >= w.width {
            w.break_row(&word_state);
        }
        w.marks.push((consumed - spaces + i, w.rows.len(), w.col));
        w.row.push(' ');
        w.col += 1;
    }
    let end_col = w.col;
    w.rows.push(w.row);
    let end = (w.rows.len() - 1, end_col);
    (w.rows, w.marks, end)
}

/// Byte length of an escape sequence body (after the ESC byte).
pub fn escape_len(s: &str) -> usize {
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
    fn wrap_ansi_breaks_at_word_boundaries() {
        assert_eq!(wrap_ansi("hello world", 8), vec!["hello", "world"]);
        assert_eq!(
            wrap_ansi("the quick brown fox", 10),
            vec!["the quick", "brown fox"]
        );
        // Leading indentation is preserved; the wrap space is dropped.
        assert_eq!(wrap_ansi("  foo barbaz", 7), vec!["  foo", "barbaz"]);
        // Words wider than a row still split per grapheme.
        assert_eq!(wrap_ansi("ab cdefgh", 4), vec!["ab", "cdef", "gh"]);
    }

    #[test]
    fn wrap_ansi_reopens_styles_across_rows() {
        let rows = wrap_ansi("\x1b[1mhello world\x1b[0m", 8);
        assert_eq!(rows, vec!["\x1b[1mhello", "\x1b[1mworld\x1b[0m"]);
        // A reset before the break is not re-opened.
        let rows = wrap_ansi("\x1b[2mdim\x1b[0m plain word", 9);
        assert_eq!(rows, vec!["\x1b[2mdim\x1b[0m plain", "word"]);
        // Open hyperlinks are re-established on continuation rows.
        let link = "\x1b]8;;http://x\x07aaaa bbbb\x1b]8;;\x07";
        let rows = wrap_ansi(link, 5);
        assert_eq!(
            rows,
            vec![
                "\x1b]8;;http://x\x07aaaa",
                "\x1b]8;;http://x\x07bbbb\x1b]8;;\x07"
            ]
        );
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
    fn wrap_ansi_locate_follows_word_wrap() {
        // "hello world" at width 8 wraps to ["hello", "world"]; the old
        // col/width arithmetic put col 7 ("o" of world, source col 7) at
        // row 0 col 7 — it must be row 1 col 1.
        let (rows, pos) = wrap_ansi_locate("hello world", 8, 7);
        assert_eq!(rows, vec!["hello", "world"]);
        assert_eq!(pos, (1, 1));
        // Start of "world" (source col 6) is row 1 col 0.
        assert_eq!(wrap_ansi_locate("hello world", 8, 6).1, (1, 0));
        // The dropped break space (source col 5) wraps to the start of row 1
        // so the cursor advances past it instead of freezing at row 0 last
        // column (which render would clamp back to col 4).
        assert_eq!(wrap_ansi_locate("hello world", 8, 5).1, (1, 0));
        // End of text parks after the last grapheme.
        assert_eq!(wrap_ansi_locate("hello world", 8, 11).1, (1, 5));
        // Hard character wrap still matches the plain arithmetic.
        assert_eq!(wrap_ansi_locate("abcdefgh", 4, 6).1, (1, 2));
        assert_eq!(wrap_ansi_locate("abcdefgh", 4, 8).1, (1, 4));
        // Wide graphemes never split: 日本語 at width 4 → ["日本", "語"].
        assert_eq!(wrap_ansi_locate("日本語", 4, 4).1, (1, 0));
        // Empty line: cursor at origin.
        assert_eq!(wrap_ansi_locate("", 4, 0).1, (0, 0));
        // Trailing spaces stay addressable.
        assert_eq!(wrap_ansi_locate("ab  ", 8, 4).1, (0, 4));
        // Regression: typing a space that fills the row and triggers a wrap
        // must advance the cursor to the next row, not freeze it on the last
        // column of the current row (which render clamps to width-1).
        // Before: "hello" cursor at end (col 5) -> (0, 5).
        assert_eq!(wrap_ansi_locate("hello", 6, 5).1, (0, 5));
        // After typing the wrap-triggering space (cursor on it, col 5):
        // must move to row 1, not stay at (0, 5).
        assert_eq!(wrap_ansi_locate("hello world", 6, 5).1, (1, 0));
    }

    #[test]
    fn render_cursor_tracks_word_wrapped_live_lines() {
        let mut term = Terminal::new(Vec::new(), 8, 24);
        term.render(
            &[],
            &["hello world".to_owned()],
            Some(LiveCursor { line: 0, col: 8 }),
        )
        .unwrap();
        // ["hello", "world"]: source col 8 is "r" → row 1, col 2.
        assert_eq!(term.prev.len(), 2);
        assert_eq!(term.cursor_row, 1);
        assert_eq!(term.cursor_col, 2);
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
