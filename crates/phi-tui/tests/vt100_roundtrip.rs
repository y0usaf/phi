//! De-risking tests for mux mode: phi's inline `Terminal` renderer must be
//! faithfully emulated by `vt100::Parser` (the mux embeds each session by
//! pointing its Terminal at an in-memory writer feeding a vt100 screen).

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use phi_tui::{LiveCursor, Terminal};

#[derive(Clone, Default)]
struct Buf(Rc<RefCell<Vec<u8>>>);

impl Buf {
    fn take(&self) -> Vec<u8> {
        std::mem::take(&mut self.0.borrow_mut())
    }
}

impl Write for Buf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Harness {
    term: Terminal<Buf>,
    buf: Buf,
    parser: vt100::Parser,
}

impl Harness {
    fn new(width: u16, height: u16) -> Self {
        let buf = Buf::default();
        Self {
            term: Terminal::new(buf.clone(), width, height),
            buf,
            parser: vt100::Parser::new(height, width, 5000),
        }
    }

    fn render(&mut self, scrollback: &[&str], live: &[&str], cursor: Option<LiveCursor>) {
        let scrollback: Vec<String> = scrollback.iter().map(|s| s.to_string()).collect();
        let live: Vec<String> = live.iter().map(|s| s.to_string()).collect();
        self.term.render(&scrollback, &live, cursor).unwrap();
        self.parser.process(&self.buf.take());
    }

    fn rows(&self) -> Vec<String> {
        let screen = self.parser.screen();
        let (height, _) = screen.size();
        (0..height)
            .map(|r| {
                let mut line = screen.contents_between(r, 0, r + 1, 0);
                while line.ends_with('\n') || line.ends_with(' ') {
                    line.pop();
                }
                line
            })
            .collect()
    }

    fn non_empty_rows(&self) -> Vec<String> {
        let mut rows = self.rows();
        while rows.last().is_some_and(|r| r.is_empty()) {
            rows.pop();
        }
        rows
    }
}

#[test]
fn full_frame_lands_scrollback_live_and_cursor() {
    let mut h = Harness::new(20, 6);
    h.render(
        &["alpha", "beta"],
        &["> hi", "status"],
        Some(LiveCursor { line: 0, col: 4 }),
    );
    assert_eq!(h.non_empty_rows(), vec!["alpha", "beta", "> hi", "status"]);
    assert_eq!(h.parser.screen().cursor_position(), (2, 4));
    assert!(!h.parser.screen().hide_cursor());
}

#[test]
fn cursor_hidden_when_streaming() {
    let mut h = Harness::new(20, 6);
    h.render(&[], &["working..."], None);
    assert!(h.parser.screen().hide_cursor());
    h.render(&[], &["> done", "ok"], Some(LiveCursor { line: 0, col: 2 }));
    assert!(!h.parser.screen().hide_cursor());
    assert_eq!(h.parser.screen().cursor_position(), (0, 2));
}

#[test]
fn diff_rewrites_changed_row_in_place() {
    let mut h = Harness::new(20, 6);
    h.render(&[], &["alpha", "beta", "gamma"], None);
    h.render(&[], &["alpha", "BETA", "gamma"], None);
    assert_eq!(h.non_empty_rows(), vec!["alpha", "BETA", "gamma"]);
}

#[test]
fn diff_appends_rows_at_bottom() {
    let mut h = Harness::new(20, 6);
    h.render(&[], &["one"], None);
    h.render(&[], &["one", "two"], None);
    h.render(&[], &["one", "two", "three"], None);
    assert_eq!(h.non_empty_rows(), vec!["one", "two", "three"]);
}

#[test]
fn shrinking_live_region_clears_leftovers() {
    let mut h = Harness::new(20, 6);
    h.render(&[], &["a", "b", "c"], None);
    h.render(&[], &["x"], None);
    assert_eq!(h.non_empty_rows(), vec!["x"]);
}

#[test]
fn streaming_commits_scroll_history_correctly() {
    // Simulates a real session: each frame commits one paragraph to
    // scrollback while the live region shows a spinner, overflowing the
    // 5-row screen so vt100 must scroll exactly like a real terminal.
    let mut h = Harness::new(20, 5);
    for i in 0..8 {
        let line = format!("para {i}");
        h.render(&[&line], &["* spinner"], None);
    }
    h.render(&[], &["> ", "done"], Some(LiveCursor { line: 0, col: 2 }));
    // Visible screen: the last committed paragraphs plus the live region.
    assert_eq!(
        h.non_empty_rows(),
        vec!["para 5", "para 6", "para 7", ">", "done"]
    );
    // Earlier paragraphs scrolled into vt100's scrollback buffer.
    h.parser.screen_mut().set_scrollback(3);
    let top = h.parser.screen().contents_between(0, 0, 1, 0);
    assert!(top.starts_with("para 2"), "scrollback top was {top:?}");
    h.parser.screen_mut().set_scrollback(0);
    assert_eq!(h.parser.screen().cursor_position(), (3, 2));
}

#[test]
fn wrapped_live_lines_round_trip() {
    let mut h = Harness::new(6, 6);
    h.render(&[], &["abcdefgh"], Some(LiveCursor { line: 0, col: 7 }));
    assert_eq!(h.non_empty_rows(), vec!["abcdef", "gh"]);
    // col 7 wraps to row 1, col 1.
    assert_eq!(h.parser.screen().cursor_position(), (1, 1));
}

#[test]
fn sgr_styles_survive_round_trip() {
    let mut h = Harness::new(20, 4);
    h.render(&[], &["\x1b[1;31mred bold\x1b[0m end"], None);
    let screen = h.parser.screen();
    let cell = screen.cell(0, 0).unwrap();
    assert_eq!(cell.contents(), "r");
    assert!(cell.bold());
    assert_eq!(cell.fgcolor(), vt100::Color::Idx(1));
    let plain = screen.cell(0, 9).unwrap();
    assert!(!plain.bold());
}

#[test]
fn resize_then_full_repaint_recovers() {
    let mut h = Harness::new(20, 6);
    h.render(&["old"], &["live"], None);
    h.term.resize(10, 4);
    h.parser.screen_mut().set_size(4, 10);
    // After resize Terminal drops its accounting; next render repaints from
    // the reflowed cursor position. Feed a frame and ensure no panic and the
    // live region is visible.
    h.render(&[], &["fresh"], Some(LiveCursor { line: 0, col: 5 }));
    let rows = h.non_empty_rows();
    assert!(rows.iter().any(|r| r.contains("fresh")), "rows: {rows:?}");
}
