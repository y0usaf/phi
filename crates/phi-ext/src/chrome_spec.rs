//! Declarative transcript + status + input chrome (WS10.3 vocabulary
//! growth).
//!
//! A [`ChromeSpec`] describes the presentation *around* tool calls that
//! [`crate::ToolRenderSpec`] deliberately doesn't: the anchor's verb/args
//! composition (`read src/main.rs` vs claude-code's `Read(src/main.rs)`),
//! the status gutter glyphs (`✓`/`✗` vs a state-coloured `⏺`), the prefix
//! carried by the first tool-result row (claude-code's `  ⎿  `), the
//! hidden-lines marker, and whether committed repl/tool blocks get a
//! bg-tinted card surface or lie flat in the transcript. These were the
//! top of the WS10.2 gap inventory — the at-a-glance differences a style
//! pack could not express.
//!
//! The second growth round covers the rest of that inventory: how a user
//! turn is marked (gutter bar vs prefix glyph + background wash), the
//! assistant prose prefix (claude-code's `⏺`), the live status line's
//! indicator/effect/meta suffix, and the input area's prompt glyph and
//! two-rule box. Chrome remains the styling surface for *singular* UI
//! surfaces — there is one transcript, one status line, one input box —
//! so all of these are members here, not new registration kinds.
//!
//! Like `ToolRenderSpec`, this is **pure data** (WS1.3a rationale): the
//! anchor feeds the in-flight status line, which re-renders every
//! animation tick — chrome must never dispatch into an extension VM.
//! Frontends keep their presentation machinery and read these strings;
//! all fields carry Phi's stock look as serde defaults, so a pack sets
//! only what it changes. Chrome strings are plain text (glyphs, spacing,
//! template literals) — colour comes from the frontend's palette roles,
//! never from embedded escapes.
//!
//! Chrome is a [`crate::StyleSpec`] member, not a standalone registration:
//! it is a singular surface (there is exactly one transcript), so "which
//! chrome is in effect" is exactly "which style is active". A pack that
//! only wants chrome is a style bundle with a single member.

use serde::{Deserialize, Serialize};

/// Container surface for committed repl/tool blocks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChromeContainer {
    /// Phi's stock look: block body wrapped in a bg-tinted padded card.
    #[default]
    Card,
    /// claude-code/codex look: block body lies flat in the transcript,
    /// the gutter glyphs are the only structure.
    Flat,
}

/// Indicator drawn at the head of the live status line.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusIndicator {
    /// Phi's stock look: the 5-bar sine equalizer.
    #[default]
    Equalizer,
    /// The style's spinner frames (claude-code's asterisk morph), coloured
    /// with the theme's accent role.
    Spinner,
}

/// Text effect applied to the live status line's thinking expression.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusEffect {
    /// Phi's stock look: accent→accent_2 gradient with a glint sweep.
    #[default]
    Gradient,
    /// claude-code's shimmer: accent-coloured text with a single-character
    /// highlight sweeping across it (uniform pulse while a tool runs).
    Sweep,
}

/// Presentation of an in-flight tool/subcall row.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingIndicator {
    /// Phi's stock look: spinner glyph + shimmer sweep over the label.
    #[default]
    SpinnerShimmer,
    /// claude-code's ToolUseLoader: the status glyph blinks (glyph ↔ blank
    /// on a 600ms interval), dim, over a plain (un-shimmered) label.
    BlinkGlyph,
}

/// Declarative transcript chrome for tool presentation. See the module
/// docs for why this is data, not hooks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChromeSpec {
    /// Anchor composition template over `{verb}` and `{args}` placeholders
    /// (`"{verb} {args}"` → `read src/main.rs`; `"{verb}({args})"` →
    /// `Read(src/main.rs)`). When a call has no rendered args the anchor
    /// collapses to the bare verb — template literals never dangle
    /// (claude-code suppresses empty `()` the same way).
    pub anchor_format: String,
    /// Status glyph for a succeeded tool call (frontend colours it with
    /// its success role).
    pub glyph_ok: String,
    /// Status glyph for a failed tool call (error role).
    pub glyph_failed: String,
    /// Gutter prefix on the first tool-result row (claude-code: `"  ⎿  "`).
    /// Continuation rows get a blank indent of the same width.
    pub result_prefix: String,
    /// Hidden-lines marker template over `{n}`
    /// (claude-code: `"… +{n} lines (ctrl+o to expand)"`).
    pub more_lines: String,
    pub container: ChromeContainer,
    /// Live status-line indicator (equalizer bars vs the style's spinner).
    pub status_indicator: StatusIndicator,
    /// Live status-line text effect on the thinking expression.
    pub status_effect: StatusEffect,
    /// Append a dim `(elapsed · ↓ tokens)` suffix to the status line
    /// (claude-code's semantics: elapsed shown past 30s, tokens once > 0).
    pub status_meta: bool,
    /// Editor prompt glyph override (claude-code: `"❯"`, crush: `":::"`).
    /// `None` keeps the frontend's stock glyph. Any display width works —
    /// the cursor math tracks the glyph's rendered width (glyph + space).
    pub input_prompt: Option<String>,
    /// Rule the input area top *and* bottom (claude-code's two-line input
    /// box); the chrome header moves below the box, reading as a footer.
    pub input_ruled: bool,
    /// User-turn marker override: `Some(prefix)` replaces the accent
    /// gutter bar with a ghost-coloured prefix glyph on the first row
    /// (claude-code: `"❯ "`); continuation rows hang at the same width.
    /// `None` keeps the stock bar.
    pub user_prefix: Option<String>,
    /// Wash user-turn rows full-width with the theme's `user_message_bg`
    /// role (claude-code's gray band).
    pub user_background: bool,
    /// Assistant prose prefix (claude-code: `"⏺ "`); continuation rows
    /// hang at the prefix width. `None` keeps stock bare prose.
    pub assistant_prefix: Option<String>,
    /// In-flight tool/subcall row presentation.
    pub pending_indicator: PendingIndicator,
    /// Result-gutter note under an in-flight call (claude-code: dim
    /// `"Running…"` on the `⎿` row). `None` shows no note.
    pub pending_note: Option<String>,
    /// Render the repl execution container like a tool card: status glyph
    /// + verb anchor instead of the bare `repl` title, code hanging under
    /// the anchor, stdout/stderr behind the result gutter — claude-code
    /// has no repl concept, so the container borrows the tool shape.
    pub repl_as_tool: bool,
}

impl Default for ChromeSpec {
    fn default() -> Self {
        Self {
            anchor_format: "{verb} {args}".to_owned(),
            glyph_ok: "✓".to_owned(),
            glyph_failed: "✗".to_owned(),
            result_prefix: "  ".to_owned(),
            more_lines: "… ({n} more lines)".to_owned(),
            container: ChromeContainer::Card,
            status_indicator: StatusIndicator::default(),
            status_effect: StatusEffect::default(),
            status_meta: false,
            input_prompt: None,
            input_ruled: false,
            user_prefix: None,
            user_background: false,
            assistant_prefix: None,
            pending_indicator: PendingIndicator::default(),
            pending_note: None,
            repl_as_tool: false,
        }
    }
}

impl ChromeSpec {
    /// Compose the anchor line from pre-styled verb and args segments.
    /// `args = None`/empty collapses to the bare verb. Substitution order
    /// is verb-then-args so args content (user data) is never re-scanned
    /// for placeholders it happens to contain.
    pub fn format_anchor(&self, verb: &str, args: Option<&str>) -> String {
        match args {
            Some(args) if !args.is_empty() => self
                .anchor_format
                .replace("{verb}", verb)
                .replace("{args}", args),
            _ => verb.to_owned(),
        }
    }

    /// Render the hidden-lines marker for `n` elided rows.
    pub fn format_more_lines(&self, n: usize) -> String {
        self.more_lines.replace("{n}", &n.to_string())
    }

    /// Blank indent matching [`Self::result_prefix`]'s width, for result
    /// continuation rows. Chrome strings are plain text (no ANSI), so
    /// char count is the display width for the glyphs in play.
    pub fn result_continuation(&self) -> String {
        " ".repeat(self.result_prefix.chars().count())
    }

    pub fn is_flat(&self) -> bool {
        self.container == ChromeContainer::Flat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_phi_stock_look() {
        let c = ChromeSpec::default();
        assert_eq!(
            c.format_anchor("read", Some("src/main.rs")),
            "read src/main.rs"
        );
        assert_eq!(c.format_more_lines(4), "… (4 more lines)");
        assert_eq!(c.result_continuation(), "  ");
        assert!(!c.is_flat());
        assert_eq!(c.status_indicator, StatusIndicator::Equalizer);
        assert_eq!(c.status_effect, StatusEffect::Gradient);
        assert!(!c.status_meta);
        assert!(c.input_prompt.is_none());
        assert!(!c.input_ruled);
        assert!(c.user_prefix.is_none());
        assert!(!c.user_background);
        assert!(c.assistant_prefix.is_none());
        assert_eq!(c.pending_indicator, PendingIndicator::SpinnerShimmer);
        assert!(c.pending_note.is_none());
        assert!(!c.repl_as_tool);
    }

    #[test]
    fn partial_table_deserializes_over_defaults() {
        let c: ChromeSpec = serde_json::from_value(serde_json::json!({
            "anchor_format": "{verb}({args})",
            "glyph_ok": "⏺",
            "glyph_failed": "⏺",
            "result_prefix": "  ⎿  ",
            "container": "flat",
        }))
        .unwrap();
        assert_eq!(
            c.format_anchor("Read", Some("src/main.rs")),
            "Read(src/main.rs)"
        );
        // Omitted member keeps the stock marker.
        assert_eq!(c.format_more_lines(2), "… (2 more lines)");
        assert_eq!(c.result_continuation(), "     ");
        assert!(c.is_flat());
    }

    #[test]
    fn claude_shaped_table_deserializes_new_members() {
        let c: ChromeSpec = serde_json::from_value(serde_json::json!({
            "status_indicator": "spinner",
            "status_effect": "sweep",
            "status_meta": true,
            "input_prompt": "❯",
            "input_ruled": true,
            "user_prefix": "❯ ",
            "user_background": true,
            "assistant_prefix": "● ",
            "pending_indicator": "blink_glyph",
            "pending_note": "Running…",
            "repl_as_tool": true,
        }))
        .unwrap();
        assert_eq!(c.status_indicator, StatusIndicator::Spinner);
        assert_eq!(c.status_effect, StatusEffect::Sweep);
        assert!(c.status_meta);
        assert_eq!(c.input_prompt.as_deref(), Some("❯"));
        assert!(c.input_ruled);
        assert_eq!(c.user_prefix.as_deref(), Some("❯ "));
        assert!(c.user_background);
        assert_eq!(c.assistant_prefix.as_deref(), Some("● "));
        assert_eq!(c.pending_indicator, PendingIndicator::BlinkGlyph);
        assert_eq!(c.pending_note.as_deref(), Some("Running…"));
        assert!(c.repl_as_tool);
        // Omitted members keep the stock look.
        assert_eq!(c.container, ChromeContainer::Card);
        assert_eq!(c.format_anchor("read", Some("a")), "read a");
    }

    #[test]
    fn empty_args_collapse_to_bare_verb() {
        let c: ChromeSpec = serde_json::from_value(serde_json::json!({
            "anchor_format": "{verb}({args})",
        }))
        .unwrap();
        assert_eq!(c.format_anchor("List", None), "List");
        assert_eq!(c.format_anchor("List", Some("")), "List");
    }

    #[test]
    fn args_content_is_not_rescanned_for_placeholders() {
        let c = ChromeSpec::default();
        assert_eq!(
            c.format_anchor("$", Some("echo '{verb}'")),
            "$ echo '{verb}'"
        );
    }
}
