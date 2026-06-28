//! Simple braille-dot spinner for the live status line.
//!
//! A lightweight 10-frame braille spinner with no color blending, no trail
//! effects, and no pre-computed frames — just a static array of glyphs.

/// Braille dot spinner frames.
const FRAMES: [&str; 10] = [
    "\u{280B}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283C}", "\u{2834}", "\u{2826}", "\u{2827}",
    "\u{2807}", "\u{280F}",
];

#[derive(Default)]
pub struct Spinner {
    idx: usize,
}

impl Spinner {
    /// Construct a new spinner. Color arguments are accepted for API
    /// compatibility but ignored — the braille dots use terminal default fg.
    pub fn new(_accent: crate::palette::Rgb, _background: crate::palette::Rgb) -> Self {
        Self { idx: 0 }
    }

    /// Current frame as a plain glyph string.
    pub fn frame(&self) -> &str {
        FRAMES[self.idx]
    }

    /// Advance the frame by one step and return the current frame.
    pub fn tick(&mut self) -> &str {
        self.idx = (self.idx + 1) % FRAMES.len();
        FRAMES[self.idx]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_non_empty() {
        let s = Spinner::default();
        assert!(!s.frame().is_empty());
    }

    #[test]
    fn tick_advances() {
        let mut s = Spinner::default();
        let f0 = s.frame().to_owned();
        s.tick();
        let f1 = s.frame().to_owned();
        assert_ne!(f0, f1);
    }

    #[test]
    fn cycles_through_all_frames() {
        let mut s = Spinner::default();
        let first = s.frame().to_owned();
        for _ in 0..FRAMES.len() {
            s.tick();
        }
        // After exactly FRAMES.len() ticks we should be back at the start.
        assert_eq!(s.frame(), first);
    }
}
