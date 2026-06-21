//! Temporary experiment: how does vt100 handle cursor at/beyond width?

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

#[derive(Clone, Default)]
struct Buf(Rc<RefCell<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

#[test]
#[ignore]
fn experiment_cursor_beyond_width() {
    // After writing "hello" on a 5-col terminal, where is the cursor?
    let mut p = vt100::Parser::new(6, 5, 5000);
    p.process(b"hello");
    println!("after 'hello' on width 5: {:?}", p.screen().cursor_position());

    // CHA to col 6 (1-indexed = 0-indexed col 5) on width 5
    let mut p2 = vt100::Parser::new(6, 5, 5000);
    p2.process(b"hello\x1b[6G");
    println!("after 'hello' + CHA 6 on width 5: {:?}", p2.screen().cursor_position());

    // CHA to col 5 (1-indexed = 0-indexed col 4) on width 5
    let mut p3 = vt100::Parser::new(6, 5, 5000);
    p3.process(b"hello\x1b[5G");
    println!("after 'hello' + CHA 5 on width 5: {:?}", p3.screen().cursor_position());

    // What about width 6, cursor at col 5?
    let mut p4 = vt100::Parser::new(6, 6, 5000);
    p4.process(b"hello\x1b[6G");
    println!("after 'hello' + CHA 6 on width 6: {:?}", p4.screen().cursor_position());

    // CHA to col 7 (1-indexed = 0-indexed col 6) on width 6
    let mut p5 = vt100::Parser::new(6, 6, 5000);
    p5.process(b"hello \x1b[7G");
    println!("after 'hello ' + CHA 7 on width 6: {:?}", p5.screen().cursor_position());

    // After writing "hello " (6 chars) on width 6
    let mut p6 = vt100::Parser::new(6, 6, 5000);
    p6.process(b"hello ");
    println!("after 'hello ' on width 6: {:?}", p6.screen().cursor_position());

    // CUF (cursor forward) from end of "hello" on width 5
    let mut p7 = vt100::Parser::new(6, 5, 5000);
    p7.process(b"hello\x1b[1C");
    println!("after 'hello' + CUF 1 on width 5: {:?}", p7.screen().cursor_position());

    assert!(false); // force output
}
