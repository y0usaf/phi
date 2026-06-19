//! Loop detection: sliding-window tool-call signature hashing.
//!
//! Ported from Crush's `loop_detection.go`. Each tool call's signature is
//! `SHA-256(concat(tool_name, "\x00", args, "\x00", result))`, stored as a
//! hex digest in a sliding window. If any single signature repeats more than
//! `max_repeats` times within the window, a loop is detected and the run
//! terminates with [`StopCause::LoopDetected`].

use sha2::{Digest, Sha256};

use crate::StopCause;

const LOOP_WINDOW_SIZE: usize = 10;
const LOOP_MAX_REPEATS: usize = 5;

/// Sliding-window loop detector for tool-call signatures.
pub struct LoopDetector {
    window: Vec<String>,
    window_size: usize,
    max_repeats: usize,
}

impl LoopDetector {
    /// Construct with default `window_size = 10`, `max_repeats = 5`.
    pub fn new() -> Self {
        Self::with_params(LOOP_WINDOW_SIZE, LOOP_MAX_REPEATS)
    }

    /// Construct with explicit window/repeat thresholds.
    pub fn with_params(window_size: usize, max_repeats: usize) -> Self {
        Self {
            window: Vec::with_capacity(window_size),
            window_size,
            max_repeats,
        }
    }

    /// Record a tool call + result. Returns `Some(StopCause::LoopDetected)`
    /// if the signature exceeded the repeat threshold within the window.
    ///
    /// Mirrors Crush's `hasRepeatedToolCalls`: the signature (SHA-256 of
    /// name + args + result) is pushed onto a sliding window; if its count
    /// in the window exceeds `max_repeats` (exclusive, matching Crush's
    /// `> maxRepeats`), a loop is detected.
    pub fn observe(
        &mut self,
        name: &str,
        args: &serde_json::Value,
        result: &str,
    ) -> Option<StopCause> {
        let sig = Self::signature(name, args, result);
        self.window.push(sig.clone());

        // Trim to window size (sliding window).
        if self.window.len() > self.window_size {
            self.window.remove(0);
        }

        // Count occurrences of the latest signature in the window.
        let count = self.window.iter().filter(|s| **s == sig).count();

        if count > self.max_repeats {
            return Some(StopCause::LoopDetected);
        }

        None
    }

    /// Clear the window (e.g. on run start).
    pub fn reset(&mut self) {
        self.window.clear();
    }

    fn signature(name: &str, args: &serde_json::Value, result: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(name.as_bytes());
        hasher.update(b"\x00");
        hasher.update(args.to_string().as_bytes());
        hasher.update(b"\x00");
        hasher.update(result.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

impl Default for LoopDetector {
    fn default() -> Self {
        Self::new()
    }
}
