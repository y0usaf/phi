//! Reproduces the "cursor doesn't move on spaces" bug: a trailing space
//! that fills the row exactly leaves the terminal cursor frozen because
//! Terminal::render clamps the cursor column to width-1.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use phi_tui::{LiveCursor, Terminal};

#[derive(Clone, Default)]
struct Buf(Rc<RefCell<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Buf {
    fn take(&self) -> Vec<u8> {
        std::mem::take(&mut self.0.borrow_mut())
    }
}

fn cursor_at(width: u16, live_text: &str, col: usize) -> (u16, u16) {
    let buf = Buf::default();
    let mut term = Terminal::new(buf.clone(), width, 6);
    let live = vec![live_text.to_owned()];
    term.render(&[], &live, Some(LiveCursor { line: 0, col })).unwrap();
    let mut parser = vt100::Parser::new(6, width, 5000);
    parser.process(&buf.take());
    let (r, c) = parser.screen().cursor_position();
    (r, c)
}

#[test]
fn trailing_space_advances_cursor() {
    // "hello" exactly fills a width-5 row; cursor sits at col 5.
    let before = cursor_at(5, "hello", 5);
    assert_eq!(before, (0, 5));
    // Typing a space should move the cursor; it must not stay at (0, 5).
    let after = cursor_at(5, "hello ", 6);
    assert_ne!(after, (0, 5), "cursor froze at (0,5) after typing a space: {after:?}");
}

#[test]
fn mid_line_space_advances_past_word_at_edge() {
    // "hello" fills width 6 but the space should still advance the cursor.
    let before = cursor_at(6, "hello", 5);
    assert_eq!(before, (0, 5));
    let after = cursor_at(6, "hello ", 6);
    assert_ne!(after, (0, 5), "cursor froze at (0,5): {after:?}");
}
