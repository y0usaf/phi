//! Braille spinner for the live status line.

use std::time::{Duration, Instant};

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Wall-clock cadence: frames advance with time, not draw frequency, so the
/// spinner glides at the same speed however often the host repaints.
const FRAME_INTERVAL: Duration = Duration::from_millis(100);

pub struct Spinner {
    idx: usize,
    last_advance: Instant,
}

impl Default for Spinner {
    fn default() -> Self {
        Self {
            idx: 0,
            last_advance: Instant::now(),
        }
    }
}

impl Spinner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn frame(&self) -> &'static str {
        FRAMES[self.idx]
    }

    pub fn tick(&mut self) -> &'static str {
        let elapsed = self.last_advance.elapsed();
        if elapsed >= FRAME_INTERVAL {
            let steps = (elapsed.as_millis() / FRAME_INTERVAL.as_millis()) as usize;
            self.idx = (self.idx + steps) % FRAMES.len();
            self.last_advance = Instant::now();
        }
        FRAMES[self.idx]
    }
}
