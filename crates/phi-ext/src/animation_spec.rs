//! Animation registration: extensions can register named spinner frames,
//! thinking expressions, and text effects so the TUI can use
//! extension-provided animations instead of the built-in defaults.

use std::sync::Arc;

use crate::visual::Rgb;

// ── Spinner ─────────────────────────────────────────────────────────────

/// A named spinner registered by an extension.
///
/// The TUI's live status line cycles through `frames` every
/// `frame_interval_ms` milliseconds. When no spinner is registered, the
/// built-in braille spinner is used.
#[derive(Clone, Debug)]
pub struct SpinnerSpec {
    pub name: String,
    pub frames: Vec<String>,
    pub frame_interval_ms: u64,
}

impl SpinnerSpec {
    pub fn new(name: impl Into<String>, frames: Vec<String>, frame_interval_ms: u64) -> Self {
        Self {
            name: name.into(),
            frames,
            frame_interval_ms,
        }
    }

    /// Compute the frame index for a given millisecond timestamp.
    pub fn frame_index(&self, ms: u64) -> usize {
        if self.frames.is_empty() || self.frame_interval_ms == 0 {
            return 0;
        }
        ((ms / self.frame_interval_ms) as usize) % self.frames.len()
    }

    /// Return the frame string for a given millisecond timestamp.
    pub fn frame_at(&self, ms: u64) -> &str {
        let idx = self.frame_index(ms);
        self.frames.get(idx).map(|s| s.as_str()).unwrap_or("")
    }
}

// ── Thinking expressions ───────────────────────────────────────────────

/// Thinking expressions cycled while the agent is working.
///
/// Each phrase is shown for `expression_interval_ms` milliseconds before
/// cycling to the next. When `tool_active` is true, `tool_expression`
/// is shown instead.
#[derive(Clone, Debug)]
pub struct ThinkingExpressionSpec {
    pub name: String,
    pub expressions: Vec<String>,
    pub tool_expression: String,
    pub expression_interval_ms: u64,
}

impl ThinkingExpressionSpec {
    pub fn new(
        name: impl Into<String>,
        expressions: Vec<String>,
        tool_expression: impl Into<String>,
        expression_interval_ms: u64,
    ) -> Self {
        Self {
            name: name.into(),
            expressions,
            tool_expression: tool_expression.into(),
            expression_interval_ms,
        }
    }

    /// Return the thinking expression for the given elapsed time.
    pub fn expression(&self, elapsed_ms: u64, tool_active: bool) -> &str {
        if tool_active {
            return &self.tool_expression;
        }
        if self.expressions.is_empty() || self.expression_interval_ms == 0 {
            return &self.tool_expression;
        }
        let idx = (elapsed_ms / self.expression_interval_ms) as usize % self.expressions.len();
        self.expressions
            .get(idx)
            .map(|s| s.as_str())
            .unwrap_or(&self.tool_expression)
    }
}

// ── Text effects ───────────────────────────────────────────────────────

/// A named text effect registered by an extension.
///
/// The `handler` closure takes a text string, optional colour arguments,
/// and an elapsed-ms timestamp, and returns a styled (ANSI-annotated)
/// string. This covers gradient text, shimmer sweeps, and any other
/// per-character visual transform.
///
/// `kind` is a tag the TUI uses to decide where to apply the effect
/// (e.g. `"gradient"`, `"shimmer"`, `"equalizer"`).
#[derive(Clone)]
pub struct TextEffectSpec {
    pub name: String,
    pub kind: String,
    pub handler: Arc<dyn Fn(&str, [Rgb; 2], Rgb, u64) -> String + Send + Sync>,
}

impl TextEffectSpec {
    pub fn new(
        name: impl Into<String>,
        kind: impl Into<String>,
        handler: Arc<dyn Fn(&str, [Rgb; 2], Rgb, u64) -> String + Send + Sync>,
    ) -> Self {
        Self {
            name: name.into(),
            kind: kind.into(),
            handler,
        }
    }

    /// Apply the effect. `text` is the input, `from`/`to` are the gradient
    /// endpoints, `shimmer` is the accent colour, `elapsed_ms` is the
    /// wall-clock elapsed time.
    pub fn apply(&self, text: &str, from: Rgb, to: Rgb, shimmer: Rgb, elapsed_ms: u64) -> String {
        (self.handler)(text, [from, to], shimmer, elapsed_ms)
    }
}
