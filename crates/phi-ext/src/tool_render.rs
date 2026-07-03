//! Declarative per-tool display metadata (WS1.3a).
//!
//! A [`ToolRenderSpec`] describes how a frontend should title, categorize,
//! and body a tool call: the anchor verb, which arguments are canonical (and
//! how they're styled), whether the tool's payload is code, and how to pick a
//! syntax-highlight hint for the result. It is deliberately **pure data** —
//! no render closures — because the same facts feed three very different
//! projections (the inline TUI's ANSI cards, the one-shot CLI's plain-text
//! panels, and the consolidated summary tally), and because the in-flight
//! status line re-renders every animation tick: dispatching into an
//! extension VM per frame would put extension code on the hot render path.
//! Frontends keep their presentation machinery and project these facts;
//! tools without a spec fall back to the generic `name key=value` rendering.
//!
//! Specs are registered via `register_tool_renderer` and are keyed by tool
//! name, decoupled from `ToolSpec`: the coding helpers (`bash`, `read_file`,
//! …) are implemented by the Python prelude and only appear as `ToolSpec`s
//! when natively exposed, yet their display metadata always applies.
//! Registration is last-write-wins so a user extension can restyle a
//! builtin (the provider-registry precedent).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Broad category a tool belongs to, used for consolidated summary cards.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRenderCategory {
    Reads,
    Edits,
    Commands,
    Searches,
    #[default]
    Other,
}

impl ToolRenderCategory {
    /// Display order for summary cards.
    pub fn order() -> &'static [ToolRenderCategory] {
        &[
            Self::Reads,
            Self::Edits,
            Self::Commands,
            Self::Searches,
            Self::Other,
        ]
    }

    pub fn singular(self) -> &'static str {
        match self {
            Self::Reads => "read",
            Self::Edits => "edit",
            Self::Commands => "command",
            Self::Searches => "search",
            Self::Other => "other",
        }
    }

    pub fn plural(self) -> &'static str {
        match self {
            Self::Reads => "reads",
            Self::Edits => "edits",
            Self::Commands => "commands",
            Self::Searches => "searches",
            Self::Other => "others",
        }
    }
}

/// Per-category `(ok, failed)` tallies, in [`ToolRenderCategory::order`]
/// order.
pub fn tally(
    items: impl IntoIterator<Item = (ToolRenderCategory, bool)>,
) -> [(ToolRenderCategory, usize, usize); 5] {
    let mut out = [
        (ToolRenderCategory::Reads, 0, 0),
        (ToolRenderCategory::Edits, 0, 0),
        (ToolRenderCategory::Commands, 0, 0),
        (ToolRenderCategory::Searches, 0, 0),
        (ToolRenderCategory::Other, 0, 0),
    ];
    for (cat, failed) in items {
        let idx = ToolRenderCategory::order()
            .iter()
            .position(|c| *c == cat)
            .unwrap();
        if failed {
            out[idx].2 += 1;
        } else {
            out[idx].1 += 1;
        }
    }
    out
}

/// Style role for an anchor argument. The frontend maps roles onto its own
/// palette (the inline TUI: `path_fg`/`string_fg`; the plain CLI: nothing).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorStyle {
    /// Unstyled, first-line-truncated.
    #[default]
    Plain,
    /// A filesystem path: path colour, never truncated.
    Path,
    /// A search pattern / string literal: string colour, first-line-truncated.
    Pattern,
}

/// One canonical argument shown in the tool's one-line anchor.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnchorArg {
    /// The argument key in the tool-call args object.
    pub key: String,
    #[serde(default)]
    pub style: AnchorStyle,
    /// Connector word rendered before the value (e.g. `"in"` for grep's
    /// path → `grep pattern in src/`).
    #[serde(default)]
    pub prefix: Option<String>,
    /// Values rendered as absent (e.g. `"."` for a cwd path). Empty values
    /// are always skipped.
    #[serde(default)]
    pub skip_values: Vec<String>,
}

impl AnchorArg {
    pub fn new(key: impl Into<String>, style: AnchorStyle) -> Self {
        Self {
            key: key.into(),
            style,
            prefix: None,
            skip_values: Vec::new(),
        }
    }

    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = Some(prefix.into());
        self
    }

    pub fn skip(mut self, value: impl Into<String>) -> Self {
        self.skip_values.push(value.into());
        self
    }

    /// Whether `value` should be rendered as absent.
    pub fn skips(&self, value: &str) -> bool {
        value.is_empty() || self.skip_values.iter().any(|s| s == value)
    }
}

/// What the tool card's multi-line body carries.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolBody {
    /// Pretty-printed args when they carry detail the one-line anchor
    /// can't — the generic fallback behaviour.
    #[default]
    Args,
    /// No body: the anchor is the whole identity.
    None,
    /// The value of `key` is code: the anchor carries its first line, the
    /// body carries the full text (multi-line only), syntax-highlighted
    /// with `lang`, each line prefixed with `prompt` when given (`"$"` for
    /// shell commands).
    Code {
        key: String,
        #[serde(default)]
        lang: Option<String>,
        #[serde(default)]
        prompt: Option<String>,
    },
}

/// How to pick the syntax-highlight hint for the tool's result preview.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResultLang {
    #[default]
    None,
    /// Always highlight as `lang`.
    Fixed { lang: String },
    /// Derive from the file extension of the path argument `key`
    /// (e.g. `read_file` results highlighted by the file's language).
    PathArg { key: String },
}

/// Declarative display metadata for one tool. See the module docs for why
/// this is data, not hooks.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolRenderSpec {
    /// Tool name this spec applies to.
    pub tool: String,
    /// Anchor verb (e.g. `"read"`, `"$"`). `None` → the generic
    /// `name key=value` anchor form.
    #[serde(default)]
    pub verb: Option<String>,
    /// Human display name for panel titles (e.g. `"Shell"`). `None` →
    /// the title-cased tool name.
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub category: ToolRenderCategory,
    /// Ordered canonical args shown in the anchor. These are stripped from
    /// the residual-args view so the canonical value appears once.
    #[serde(default)]
    pub anchor: Vec<AnchorArg>,
    #[serde(default)]
    pub body: ToolBody,
    #[serde(default)]
    pub result_lang: ResultLang,
    /// Parse `[exit code N]` / timed-out trailer lines out of the result
    /// text into a status footer (the prelude's `bash` output convention).
    #[serde(default)]
    pub exit_code_trailer: bool,
}

impl ToolRenderSpec {
    pub fn new(tool: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            verb: None,
            display_name: None,
            category: ToolRenderCategory::Other,
            anchor: Vec::new(),
            body: ToolBody::Args,
            result_lang: ResultLang::None,
            exit_code_trailer: false,
        }
    }

    pub fn with_verb(mut self, verb: impl Into<String>) -> Self {
        self.verb = Some(verb.into());
        self
    }

    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }

    pub fn with_category(mut self, category: ToolRenderCategory) -> Self {
        self.category = category;
        self
    }

    pub fn with_anchor(mut self, arg: AnchorArg) -> Self {
        self.anchor.push(arg);
        self
    }

    pub fn with_body(mut self, body: ToolBody) -> Self {
        self.body = body;
        self
    }

    pub fn with_result_lang(mut self, result_lang: ResultLang) -> Self {
        self.result_lang = result_lang;
        self
    }

    pub fn with_exit_code_trailer(mut self) -> Self {
        self.exit_code_trailer = true;
        self
    }

    /// The display name for panel titles: explicit `display_name`, else the
    /// tool name title-cased on `_` boundaries (`read_file` → `Read File`).
    pub fn resolved_display_name(&self) -> String {
        match &self.display_name {
            Some(name) => name.clone(),
            None => title_case_tool_name(&self.tool),
        }
    }

    /// Argument keys already carried by the anchor (for residual-args
    /// stripping so the canonical value is never re-stated).
    pub fn anchor_keys(&self) -> impl Iterator<Item = &str> {
        self.anchor.iter().map(|a| a.key.as_str())
    }
}

/// `read_file` → `Read File`; used when a spec has no explicit display name
/// and for tools with no spec at all.
pub fn title_case_tool_name(name: &str) -> String {
    name.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The core `repl` tool's render spec. Seeded by [`ToolRenderIndex`] so a
/// builtins-less build still renders the engine's own tool richly (bare
/// core must boot); a registered spec for `repl` overrides the seed.
pub fn repl_render_spec() -> ToolRenderSpec {
    ToolRenderSpec::new("repl").with_body(ToolBody::Code {
        key: "code".to_owned(),
        lang: Some("repl".to_owned()),
        prompt: None,
    })
}

/// Resolved name→spec lookup snapshot for frontends. Seeded with the core
/// `repl` spec; registry entries override the seed.
#[derive(Clone, Debug)]
pub struct ToolRenderIndex {
    specs: BTreeMap<String, ToolRenderSpec>,
}

impl Default for ToolRenderIndex {
    fn default() -> Self {
        Self::new(std::iter::empty())
    }
}

impl ToolRenderIndex {
    pub fn new(registered: impl IntoIterator<Item = ToolRenderSpec>) -> Self {
        let mut specs = BTreeMap::new();
        let repl = repl_render_spec();
        specs.insert(repl.tool.clone(), repl);
        for spec in registered {
            specs.insert(spec.tool.clone(), spec);
        }
        Self { specs }
    }

    pub fn get(&self, name: &str) -> Option<&ToolRenderSpec> {
        self.specs.get(name)
    }

    pub fn category(&self, name: &str) -> ToolRenderCategory {
        self.specs.get(name).map(|s| s.category).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_deserializes_from_declarative_json() {
        let spec: ToolRenderSpec = serde_json::from_value(serde_json::json!({
            "tool": "bash",
            "verb": "$",
            "display_name": "Shell",
            "category": "commands",
            "body": { "kind": "code", "key": "cmd", "lang": "bash", "prompt": "$" },
            "exit_code_trailer": true,
        }))
        .unwrap();
        assert_eq!(spec.tool, "bash");
        assert_eq!(spec.category, ToolRenderCategory::Commands);
        assert!(matches!(&spec.body, ToolBody::Code { key, .. } if key == "cmd"));
        assert!(spec.exit_code_trailer);
        // Omitted fields default.
        assert!(spec.anchor.is_empty());
        assert!(matches!(spec.result_lang, ResultLang::None));
    }

    #[test]
    fn index_seeds_repl_and_lets_registry_override() {
        let idx = ToolRenderIndex::default();
        assert!(matches!(
            &idx.get("repl").unwrap().body,
            ToolBody::Code { key, .. } if key == "code"
        ));

        let custom = ToolRenderSpec::new("repl").with_display_name("Sandbox");
        let idx = ToolRenderIndex::new([custom]);
        assert_eq!(idx.get("repl").unwrap().resolved_display_name(), "Sandbox");
    }

    #[test]
    fn tally_counts_by_category() {
        let out = tally([
            (ToolRenderCategory::Reads, false),
            (ToolRenderCategory::Reads, true),
            (ToolRenderCategory::Commands, false),
        ]);
        assert_eq!(out[0], (ToolRenderCategory::Reads, 1, 1));
        assert_eq!(out[2], (ToolRenderCategory::Commands, 1, 0));
    }

    #[test]
    fn display_name_title_cases_by_default() {
        assert_eq!(
            ToolRenderSpec::new("read_file").resolved_display_name(),
            "Read File"
        );
        assert_eq!(
            ToolRenderSpec::new("bash")
                .with_display_name("Shell")
                .resolved_display_name(),
            "Shell"
        );
    }
}
