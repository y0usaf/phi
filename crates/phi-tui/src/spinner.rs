//! Braille spinner for the live status line.

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Default)]
pub struct Spinner {
    idx: usize,
}

impl Spinner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn frame(&self) -> &'static str {
        FRAMES[self.idx]
    }

    pub fn tick(&mut self) -> &'static str {
        let frame = FRAMES[self.idx];
        self.idx = (self.idx + 1) % FRAMES.len();
        frame
    }
}
