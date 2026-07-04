//! Declarative transcript tool chrome (WS10.3 vocabulary growth).
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
