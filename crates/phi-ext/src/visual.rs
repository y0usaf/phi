//! Minimal visual primitive types shared between the extension system
//! and the TUI.
//!
//! These types live in `phi-ext` so the extension system has no dependency
//! on `phi-tui`. The TUI converts between `visual::Rgb` and its own
//! `phi_tui::palette::Rgb` at the adapter boundary.

use serde::{Deserialize, Serialize};

/// A packed sRGB triple (0..255 per channel).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// A styled text span: a string fragment with optional foreground and
/// background colours and text attributes (bold, italic, etc.).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StyledSpan {
    pub text: String,
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
}

/// A sequence of [`StyledSpan`]s — the return type for rich-text
/// rendering by extensions (e.g. message renderers that want to emit
/// styled output rather than raw ANSI strings).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StyledText {
    pub spans: Vec<StyledSpan>,
}

impl StyledText {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            spans: vec![StyledSpan {
                text: text.into(),
                ..Default::default()
            }],
        }
    }

    pub fn push_span(&mut self, span: StyledSpan) {
        self.spans.push(span);
    }

    /// Convert to a raw ANSI-annotated string.
    pub fn to_ansi(&self) -> String {
        let mut out = String::new();
        for span in &self.spans {
            let mut prefixes: Vec<String> = Vec::new();
            if span.bold {
                prefixes.push("1".to_string());
            }
            if span.italic {
                prefixes.push("3".to_string());
            }
            if span.underline {
                prefixes.push("4".to_string());
            }
            if span.dim {
                prefixes.push("2".to_string());
            }
            if let Some(fg) = span.fg {
                prefixes.push(format!("38;2;{};{};{}", fg.0, fg.1, fg.2));
            }
            if let Some(bg) = span.bg {
                prefixes.push(format!("48;2;{};{};{}", bg.0, bg.1, bg.2));
            }
            if !prefixes.is_empty() {
                out.push_str(&format!("[{}m", prefixes.join(";")));
            }
            out.push_str(&span.text);
            if !prefixes.is_empty() {
                out.push_str("[0m");
            }
        }
        out
    }
}

impl StyledSpan {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }

    pub fn with_fg(mut self, fg: Rgb) -> Self {
        self.fg = Some(fg);
        self
    }

    pub fn with_bg(mut self, bg: Rgb) -> Self {
        self.bg = Some(bg);
        self
    }

    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    pub fn underline(mut self) -> Self {
        self.underline = true;
        self
    }

    pub fn dim(mut self) -> Self {
        self.dim = true;
        self
    }
}
