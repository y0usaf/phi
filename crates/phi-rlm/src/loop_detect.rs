//! Loop detection: per-step tool-interaction signature hashing.
//!
//! Ported from Crush's `loop_detection.go`. Each *step* (one assistant turn)
//! yields a single signature: the SHA-256 of every tool call + result it
//! contains, concatenated. Steps with no tool calls (text-only) are skipped
//! entirely — they never enter the window and never count toward repeats.
//! If, within the last `window_size` recorded steps, any single signature
//! appears more than `max_repeats` times, a loop is detected and the run
//! terminates with [`StopCause::LoopDetected`].
//!
//! Matching Crush exactly:
//! - the full step history is retained and the last `window_size` entries are
//!   re-scanned on every step;
//! - detection is gated on `history.len() >= window_size` (fewer steps than
//!   the window never triggers);
//! - the threshold is exclusive (`count > max_repeats`).

use sha2::{Digest, Sha256};

use crate::StopCause;

const LOOP_WINDOW_SIZE: usize = 10;
const LOOP_MAX_REPEATS: usize = 5;

/// One tool interaction recorded during the current step: `(name, args, result)`.
type ToolInteraction = (String, String, String);

/// Per-step loop detector for tool-interaction signatures.
///
/// Calls arriving during a step are buffered in [`Self::pending`]. When the
/// step completes, [`Self::finalize_step`] aggregates them into a single
/// signature and evaluates the repeat window.
pub struct LoopDetector {
    /// Signatures of all past steps (oldest first).
    history: Vec<String>,
    /// Tool calls observed so far in the in-progress step.
    pending: Vec<ToolInteraction>,
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
            history: Vec::new(),
            pending: Vec::new(),
            window_size,
            max_repeats,
        }
    }

    /// Buffer a tool call + result for the in-progress step. Does not
    /// evaluate; call [`Self::finalize_step`] at the step boundary.
    pub fn observe_call(
        &mut self,
        name: &str,
        args: &serde_json::Value,
        result: &str,
    ) {
        self.pending.push((name.to_owned(), args.to_string(), result.to_owned()));
    }

    /// Finalize the in-progress step: compute its aggregate signature and
    /// check the repeat window. Returns `Some(StopCause::LoopDetected)` if
    /// the signature exceeded the repeat threshold within the window.
    ///
    /// A step with no buffered tool calls produces no signature and is
    /// skipped entirely (matching Crush's `sig == ""` continue), so it does
    /// not pollute the window or count toward repeats. Detection is gated on
    /// `history.len() >= window_size`: until enough steps have accumulated
    /// the detector never fires, matching Crush's `len(steps) < windowSize`
    /// early return.
    pub fn finalize_step(&mut self) -> Option<StopCause> {
        let pending = std::mem::take(&mut self.pending);
        let sig = Self::step_signature(&pending);
        let Some(sig) = sig else {
            return None;
        };

        self.history.push(sig);

        if self.history.len() < self.window_size {
            return None;
        }

        // Match Crush: count *every* signature in the window (not just the
        // latest), returning true if any exceeds `max_repeats`.
        let window = &self.history[self.history.len() - self.window_size..];
        let mut counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for s in window {
            let c = counts.entry(s.as_str()).or_insert(0);
            *c += 1;
            if *c > self.max_repeats {
                return Some(StopCause::LoopDetected);
            }
        }
        None
    }

    /// Clear all history and pending calls (e.g. on run start).
    pub fn reset(&mut self) {
        self.history.clear();
        self.pending.clear();
    }

    /// Aggregate signature for a step's tool interactions.
    ///
    /// Mirrors Crush's `getToolInteractionSignature`: the SHA-256 of
    /// `name \x00 args \x00 result \x00` for every call, concatenated in
    /// order. Returns `None` when there are no calls (text-only step).
    fn step_signature(calls: &[ToolInteraction]) -> Option<String> {
        if calls.is_empty() {
            return None;
        }

        let mut hasher = Sha256::new();
        for (name, args, result) in calls {
            hasher.update(name.as_bytes());
            hasher.update(b"\x00");
            hasher.update(args.as_bytes());
            hasher.update(b"\x00");
            hasher.update(result.as_bytes());
            hasher.update(b"\x00");
        }
        Some(format!("{:x}", hasher.finalize()))
    }
}

impl Default for LoopDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str, args: &serde_json::Value, result: &str) -> ToolInteraction {
        (name.to_owned(), args.to_string(), result.to_owned())
    }

    fn finalize_with(det: &mut LoopDetector, calls: &[ToolInteraction]) -> Option<StopCause> {
        for c in calls {
            det.observe_call(&c.0, &serde_json::from_str(&c.1).unwrap_or(json!(null)), &c.2);
        }
        det.finalize_step()
    }

    #[test]
    fn no_steps() {
        let mut det = LoopDetector::new();
        assert_eq!(det.finalize_step(), None);
    }

    #[test]
    fn text_only_step_is_skipped() {
        // A step with no tool calls should not enter the window and never
        // trigger detection, even if repeated.
        let mut det = LoopDetector::with_params(10, 5);
        for _ in 0..20 {
            assert_eq!(det.finalize_step(), None);
        }
    }

    #[test]
    fn fewer_steps_than_window_never_triggers() {
        // 6 identical tool steps but window is 10 and only 6 exist.
        let mut det = LoopDetector::with_params(10, 5);
        let c = call("read", &json!({"file":"a.go"}), "content");
        for _ in 0..6 {
            assert_eq!(finalize_with(&mut det, std::slice::from_ref(&c)), None);
        }
    }

    #[test]
    fn all_different_signatures() {
        let mut det = LoopDetector::with_params(10, 5);
        for i in 0..10 {
            let c = call("tool", &json!({"i": i}), &format!("result-{i}"));
            assert_eq!(finalize_with(&mut det, std::slice::from_ref(&c)), None);
        }
    }

    #[test]
    fn exact_repeat_at_threshold_not_detected() {
        // max_repeats=5 means > 5 is needed, so exactly 5 should NOT fire.
        let mut det = LoopDetector::with_params(10, 5);
        let c = call("read", &json!({"file":"a.go"}), "content");
        for _ in 0..5 {
            assert_eq!(finalize_with(&mut det, std::slice::from_ref(&c)), None);
        }
        for i in 5..10 {
            let c = call("tool", &json!({"i": i}), &format!("result-{i}"));
            assert_eq!(finalize_with(&mut det, std::slice::from_ref(&c)), None);
        }
    }

    #[test]
    fn loop_detected() {
        // 6 identical steps + 4 distinct = 10 total; window fills on the
        // 10th step, at which point the 6 identical exceed max_repeats (5).
        let mut det = LoopDetector::with_params(10, 5);
        let c = call("read", &json!({"file":"a.go"}), "content");
        // Push 6 identical steps (window not full yet, so not detected).
        let mut detected = false;
        for _ in 0..6 {
            if finalize_with(&mut det, std::slice::from_ref(&c)).is_some() {
                detected = true;
            }
        }
        assert!(!detected, "window not yet full: 6 steps should not fire");
        // Fill the window with 4 distinct steps. The 10th lands and triggers.
        for i in 0..4 {
            let o = call("tool", &json!({"i": i}), &format!("result-{i}"));
            if finalize_with(&mut det, std::slice::from_ref(&o)).is_some() {
                detected = true;
            }
        }
        assert!(detected, "expected loop detection once window has 6 identical + 4 distinct");
    }

    #[test]
    fn loop_detected_on_sixth_identical_step() {
        // Sanity: with 10 prior identical steps already in history, the 11th
        // should *not* be needed — detection fires as soon as window has 6.
        let mut det = LoopDetector::with_params(10, 5);
        let c = call("read", &json!({"file":"a.go"}), "content");
        // 5 identical + 4 distinct = 9 steps, no fire yet (window not full).
        for _ in 0..5 {
            assert_eq!(finalize_with(&mut det, std::slice::from_ref(&c)), None);
        }
        for i in 0..4 {
            let o = call("write", &json!({"i": i}), &format!("r{i}"));
            assert_eq!(finalize_with(&mut det, std::slice::from_ref(&o)), None);
        }
        // 10th step is the 6th occurrence of `c` -> window now full and count=6.
        assert_eq!(
            finalize_with(&mut det, std::slice::from_ref(&c)),
            Some(StopCause::LoopDetected)
        );
    }

    #[test]
    fn empty_steps_skipped_among_tool_steps() {
        // 4 repeated tool steps + 4 empty steps + 2 distinct steps -> only 4 in
        // window, never above 5.
        let mut det = LoopDetector::with_params(10, 5);
        let c = call("read", &json!({"file":"a.go"}), "content");
        for _ in 0..4 {
            assert_eq!(finalize_with(&mut det, std::slice::from_ref(&c)), None);
        }
        for _ in 0..4 {
            assert_eq!(det.finalize_step(), None); // text-only, skipped
        }
        for i in 0..2 {
            let c = call("write", &json!({"i": i}), &format!("r{i}"));
            assert_eq!(finalize_with(&mut det, std::slice::from_ref(&c)), None);
        }
    }

    #[test]
    fn alternating_patterns_each_five_times() {
        let mut det = LoopDetector::with_params(10, 5);
        let a = call("read", &json!({"file":"a.go"}), "content-a");
        let b = call("write", &json!({"file":"b.go"}), "content-b");
        for i in 0..10 {
            let c = if i % 2 == 0 { &a } else { &b };
            assert_eq!(finalize_with(&mut det, std::slice::from_ref(c)), None);
        }
    }

    #[test]
    fn signature_empty_for_no_calls() {
        assert_eq!(LoopDetector::step_signature(&[]), None);
    }

    #[test]
    fn signature_present_for_one_call() {
        let c = [call("read", &json!({"file":"a.go"}), "content")];
        assert!(LoopDetector::step_signature(&c).is_some());
    }

    #[test]
    fn same_interactions_same_signature() {
        let c1 = [call("read", &json!({"file":"a.go"}), "content")];
        let c2 = [call("read", &json!({"file":"a.go"}), "content")];
        assert_eq!(
            LoopDetector::step_signature(&c1),
            LoopDetector::step_signature(&c2)
        );
    }

    #[test]
    fn different_inputs_different_signatures() {
        let c1 = [call("read", &json!({"file":"a.go"}), "content")];
        let c2 = [call("read", &json!({"file":"b.go"}), "content")];
        assert_ne!(
            LoopDetector::step_signature(&c1),
            LoopDetector::step_signature(&c2)
        );
    }

    #[test]
    fn multiple_calls_one_step_aggregate() {
        // Two identical calls in one step produce one signature; that step
        // counts once toward the window (per-step, not per-call).
        let calls = vec![
            call("read", &json!({"file":"a.go"}), "content"),
            call("read", &json!({"file":"a.go"}), "content"),
        ];
        let mut det = LoopDetector::with_params(10, 5);
        // Push 10 identical two-call steps.
        for _ in 0..9 {
            assert_eq!(finalize_with(&mut det, &calls), None);
        }
        // 10th step: all entries are identical -> count=10 > 5 -> detected.
        assert_eq!(
            finalize_with(&mut det, &calls),
            Some(StopCause::LoopDetected)
        );
    }
}
