//! Theme registration: extensions can register named colour themes
//! so the TUI can use an extension-provided palette instead of the
//! built-in `generate_theme` derivation.
//!
//! The spec is **declarative-first**: an extension provides a map of
//! role-name → `Rgb` and the TUI resolves it into its own `Theme` struct.
//! A `ThemeResolver` closure is the advanced path for extensions that want
//! to derive colours programmatically from the terminal's reported colours.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::visual::Rgb;

/// A named colour theme registered by an extension.
///
/// The `palette` maps role names (e.g. `"foreground"`, `"accent"`,
/// `"surface"`) to `Rgb` values. The TUI knows which roles it needs and
/// looks them up by key. Unknown roles are simply ignored, so an extension
/// only needs to provide the colours it cares about.
///
/// For programmatic theme generation (e.g. deriving a gray ramp from the
/// terminal background), use [`ThemeSpec::with_resolver`].
#[derive(Clone)]
pub struct ThemeSpec {
    pub name: String,
    pub palette: BTreeMap<String, Rgb>,
    /// Optional resolver that receives the terminal's reported colours
    /// and returns a complete palette. When present, this takes priority
    /// over the static `palette` field.
    pub resolver:
        Option<Arc<dyn Fn(Option<&TerminalColorsProbe>) -> BTreeMap<String, Rgb> + Send + Sync>>,
}

/// A minimal description of what the terminal reported (OSC 10/11/4).
///
/// This is passed to a `ThemeResolver` so an extension can adapt its
/// palette to the user's terminal. It mirrors `phi_tui::TerminalColors`
/// but lives in `phi-ext` to avoid a cross-crate dependency.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalColorsProbe {
    pub background: Rgb,
    pub foreground: Rgb,
    pub palette: [Option<Rgb>; 16],
}

impl ThemeSpec {
    /// Create a declarative theme with a static palette.
    pub fn new(name: impl Into<String>, palette: BTreeMap<String, Rgb>) -> Self {
        Self {
            name: name.into(),
            palette,
            resolver: None,
        }
    }

    /// Create a theme with a programmatic resolver closure.
    pub fn with_resolver(
        name: impl Into<String>,
        resolver: Arc<dyn Fn(Option<&TerminalColorsProbe>) -> BTreeMap<String, Rgb> + Send + Sync>,
    ) -> Self {
        Self {
            name: name.into(),
            palette: BTreeMap::new(),
            resolver: Some(resolver),
        }
    }

    /// Resolve the palette, using the resolver if present, otherwise the
    /// static palette. `terminal_colors` is `Some` if the terminal answered
    /// OSC probes, `None` if using defaults.
    pub fn resolve(&self, terminal_colors: Option<&TerminalColorsProbe>) -> BTreeMap<String, Rgb> {
        if let Some(resolver) = &self.resolver {
            resolver(terminal_colors)
        } else {
            self.palette.clone()
        }
    }
}
