//! Tail-follow catch-up animation.
//!
//! Ports jcode's scroll catch-up concept (bounded-step advance rather than
//! instant snapping) into the phi mux cell-surface rendering pipeline. When
//! new content arrives and the tail-follow target jumps forward, the scroll
//! position advances in discrete bounded steps each tick instead of snapping
//! to the new target in a single frame. This produces a smooth visual catch-up
//! that avoids the jarring discontinuity of a full-frame jump.
//!
//! The animation is self-contained and can be called from scene rendering to
//! animate the scroll position when new content arrives, but is NOT yet wired
//! into `scene.rs` — it is registered as a module only.

/// Default number of lines advanced per tick (≈6% of a typical 24-row viewport).
pub const DEFAULT_STEP_SIZE: usize = 2;
/// Default tick interval in milliseconds (≈60 FPS).
pub const DEFAULT_INTERVAL_MS: u64 = 16;
/// Environment variable to force reduced motion (instant snap, no catch-up).
pub const REDUCED_MOTION_ENV: &str = "PHI_REDUCED_MOTION";

/// Tail-follow catch-up animation state.
///
/// Tracks the distance between the current scroll position and the target
/// scroll position (where the tail currently is). Each [`tick`](Self::tick)
/// advances `current` toward `target` by at most `step_size` lines, producing
/// a bounded-step catch-up animation.
#[derive(Clone, Debug)]
pub struct CatchUpAnimation {
    /// Current animated scroll position (lines from top).
    current: usize,
    /// Target scroll position the animation is catching up to.
    target: usize,
    /// Maximum lines to advance per tick.
    step_size: usize,
    /// Wall-clock interval between ticks in milliseconds.
    interval_ms: u64,
    /// Monotonic timestamp of the last tick (ms).
    last_tick_ms: u64,
    /// Whether the animation has been initialized with a real position.
    initialized: bool,
}

impl Default for CatchUpAnimation {
    fn default() -> Self {
        Self::new()
    }
}

impl CatchUpAnimation {
    /// Create a new catch-up animation with default settings.
    pub fn new() -> Self {
        Self {
            current: 0,
            target: 0,
            step_size: DEFAULT_STEP_SIZE,
            interval_ms: DEFAULT_INTERVAL_MS,
            last_tick_ms: 0,
            initialized: false,
        }
    }

    /// Set a custom step size (lines per tick).
    pub fn with_step_size(mut self, step: usize) -> Self {
        self.step_size = step.max(1);
        self
    }

    /// Set a custom tick interval in milliseconds.
    pub fn with_interval(mut self, ms: u64) -> Self {
        self.interval_ms = ms.max(1);
        self
    }

    /// Initialize the scroll position without triggering an animation.
    /// Use this for the first frame or after a reset so the first `set_target`
    /// animates from the correct starting position rather than from 0.
    pub fn initialize(&mut self, position: usize, now_ms: u64) {
        self.current = position;
        self.target = position;
        self.last_tick_ms = now_ms;
        self.initialized = true;
    }

    /// Set the target scroll position. If the target differs from `current`,
    /// the animation becomes active and [`tick`](Self::tick) will advance.
    pub fn set_target(&mut self, target: usize) {
        self.target = target;
    }

    /// Advance the animation by one bounded step, if enough wall-clock time
    /// has elapsed since the last tick.
    ///
    /// Returns the scroll position to use for rendering this frame — either
    /// the newly advanced `current` or, when reduced motion is enabled, the
    /// target itself (instant snap).
    pub fn tick(&mut self, now_ms: u64) -> usize {
        if !self.initialized {
            self.initialize(self.target, now_ms);
            return self.current;
        }

        if reduced_motion_enabled() {
            self.current = self.target;
            self.last_tick_ms = now_ms;
            return self.current;
        }

        // Only advance on tick interval boundaries.
        if now_ms.saturating_sub(self.last_tick_ms) < self.interval_ms {
            return self.current;
        }
        self.last_tick_ms = now_ms;
        self.advance()
    }

    /// Advance `current` toward `target` by at most `step_size` lines.
    /// This is the bounded-step core: each call moves no more than
    /// `step_size` regardless of the remaining distance.
    fn advance(&mut self) -> usize {
        if self.current < self.target {
            let step = self.step_size.min(self.target - self.current);
            self.current += step;
        } else if self.current > self.target {
            let step = self.step_size.min(self.current - self.target);
            self.current -= step;
        }
        self.current
    }

    /// Force an immediate snap to the target without animation.
    pub fn snap_to_target(&mut self) {
        self.current = self.target;
    }

    /// Reset back to a position, clearing any pending target.
    pub fn reset(&mut self, position: usize) {
        self.current = position;
        self.target = position;
        self.initialized = false;
    }

    /// Whether the animation is still catching up to the target.
    pub fn is_animating(&self) -> bool {
        self.current != self.target
    }

    /// Remaining distance to the target scroll position.
    pub fn distance(&self) -> usize {
        self.target.abs_diff(self.current)
    }

    /// Current animated scroll position.
    pub fn current(&self) -> usize {
        self.current
    }

    /// Target scroll position.
    pub fn target(&self) -> usize {
        self.target
    }

    /// Configured step size.
    pub fn step_size(&self) -> usize {
        self.step_size
    }

    /// Configured interval in milliseconds.
    pub fn interval_ms(&self) -> u64 {
        self.interval_ms
    }
}

/// Whether reduced motion is requested via environment variable.
/// When enabled, catch-up animations snap instantly to the target.
fn reduced_motion_enabled() -> bool {
    matches!(
        std::env::var(REDUCED_MOTION_ENV).as_deref(),
        Ok("1") | Ok("true")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_zero_with_defaults() {
        let anim = CatchUpAnimation::default();
        assert_eq!(anim.current(), 0);
        assert_eq!(anim.target(), 0);
        assert_eq!(anim.step_size(), DEFAULT_STEP_SIZE);
        assert_eq!(anim.interval_ms(), DEFAULT_INTERVAL_MS);
        assert!(!anim.is_animating());
    }

    #[test]
    fn bounded_step_advances_toward_target() {
        let mut anim = CatchUpAnimation::new().with_step_size(3);
        anim.initialize(0, 0);
        anim.set_target(10);

        // Each tick advances by at most 3. Timestamps must exceed the
        // interval (16ms) for advancement to occur.
        assert_eq!(anim.tick(16), 3);
        assert!(anim.is_animating());

        assert_eq!(anim.tick(32), 6);
        assert_eq!(anim.tick(48), 9);

        // Final step is bounded — does not overshoot.
        assert_eq!(anim.tick(64), 10);
        assert!(!anim.is_animating());
    }

    #[test]
    fn step_does_not_overshoot() {
        let mut anim = CatchUpAnimation::new().with_step_size(5).with_interval(1);
        anim.initialize(0, 0);
        anim.set_target(3);
        // Target is smaller than step_size: advance to exactly 3.
        assert_eq!(anim.tick(1), 3);
        assert!(!anim.is_animating());
    }

    #[test]
    fn backward_advance_toward_lower_target() {
        let mut anim = CatchUpAnimation::new().with_step_size(2).with_interval(1);
        anim.initialize(10, 0);
        anim.set_target(4);
        assert_eq!(anim.tick(1), 8);
        assert_eq!(anim.tick(2), 6);
        assert_eq!(anim.tick(3), 4);
        assert!(!anim.is_animating());
    }

    #[test]
    fn tick_respects_interval() {
        let mut anim = CatchUpAnimation::new().with_step_size(1).with_interval(16);
        anim.initialize(0, 0);
        anim.set_target(5);

        // Not enough time elapsed: no advance.
        assert_eq!(anim.tick(10), 0);
        assert_eq!(anim.tick(15), 0);

        // At interval boundary: advance.
        assert_eq!(anim.tick(16), 1);
    }

    #[test]
    fn snap_to_target_jumps_instantly() {
        let mut anim = CatchUpAnimation::new().with_step_size(1).with_interval(1);
        anim.initialize(0, 0);
        anim.set_target(100);
        anim.snap_to_target();
        assert_eq!(anim.current(), 100);
        assert!(!anim.is_animating());
    }

    #[test]
    fn reset_clears_position_and_target() {
        let mut anim = CatchUpAnimation::new().with_step_size(1);
        anim.initialize(50, 0);
        anim.set_target(60);
        anim.reset(0);
        assert_eq!(anim.current(), 0);
        assert_eq!(anim.target(), 0);
        assert!(!anim.is_animating());
    }

    #[test]
    fn distance_tracks_remaining() {
        let mut anim = CatchUpAnimation::new().with_step_size(2).with_interval(1);
        anim.initialize(0, 0);
        anim.set_target(10);
        assert_eq!(anim.distance(), 10);
        anim.tick(1); // -> 2
        assert_eq!(anim.distance(), 8);
        anim.tick(2); // -> 4
        assert_eq!(anim.distance(), 6);
    }

    #[test]
    fn first_tick_initializes_from_target() {
        let mut anim = CatchUpAnimation::new().with_step_size(1).with_interval(1);
        // No initialize() call: first tick should snap to target then start.
        anim.set_target(5);
        let pos = anim.tick(0);
        // After initialization, current == target == 5, no animation.
        assert_eq!(pos, 5);
        assert!(!anim.is_animating());

        // A new target should animate.
        anim.set_target(10);
        assert_eq!(anim.tick(1), 6);
        assert!(anim.is_animating());
    }

    #[test]
    fn with_step_size_clamps_to_minimum_one() {
        let anim = CatchUpAnimation::new().with_step_size(0);
        assert_eq!(anim.step_size(), 1);
    }

    #[test]
    fn with_interval_clamps_to_minimum_one() {
        let anim = CatchUpAnimation::new().with_interval(0);
        assert_eq!(anim.interval_ms(), 1);
    }
}
