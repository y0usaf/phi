//! Single-stroke key parsing and a generic keymap: key strokes parse
//! from strings like `"ctrl+n"` and convert from crossterm `KeyEvent`.
//!
//! Shared by the mux (`phi-mux::keys`) and the inline session TUI
//! (`phi-session-tui`) so both frontends share one binding-string grammar
//! and one `KeyEvent`-to-stroke conversion. Per-frontend action enums live
//! in their own crates; this module is the substrate.

use std::collections::HashMap;
use std::fmt;

use crossterm::event::{self, KeyCode, KeyEvent};

/// A parsed key stroke: modifiers plus a key token. Hashable so it can key a
/// `Keymap` binding table.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyStroke {
    pub modifiers: KeyModifiers,
    pub key: KeyToken,
}

impl KeyStroke {
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("empty key stroke".to_owned());
        }
        let mut parts: Vec<&str> = text.split('+').map(str::trim).collect();
        if parts.iter().any(|part| part.is_empty()) {
            return Err(format!("invalid key stroke: {text}"));
        }
        let key = parts.pop().expect("non-empty split");
        let mut modifiers = KeyModifiers::default();
        for modifier in parts {
            modifiers.apply_token(modifier)?;
        }
        Ok(Self {
            modifiers,
            key: KeyToken::parse(key)?,
        })
    }

    pub fn from_key_event(key: &KeyEvent) -> Option<Self> {
        let control = key.modifiers.contains(event::KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(event::KeyModifiers::ALT);
        let raw_shift = key.modifiers.contains(event::KeyModifiers::SHIFT);

        let token = match key.code {
            KeyCode::Char(' ') => KeyToken::Named(NamedKeyToken::Space),
            KeyCode::Char(c) => KeyToken::Character(c.to_ascii_lowercase().to_string()),
            KeyCode::Left => KeyToken::Named(NamedKeyToken::Left),
            KeyCode::Right => KeyToken::Named(NamedKeyToken::Right),
            KeyCode::Up => KeyToken::Named(NamedKeyToken::Up),
            KeyCode::Down => KeyToken::Named(NamedKeyToken::Down),
            KeyCode::Delete => KeyToken::Named(NamedKeyToken::Delete),
            KeyCode::Insert => KeyToken::Named(NamedKeyToken::Insert),
            KeyCode::Home => KeyToken::Named(NamedKeyToken::Home),
            KeyCode::End => KeyToken::Named(NamedKeyToken::End),
            KeyCode::PageUp => KeyToken::Named(NamedKeyToken::PageUp),
            KeyCode::PageDown => KeyToken::Named(NamedKeyToken::PageDown),
            KeyCode::Enter => KeyToken::Named(NamedKeyToken::Enter),
            KeyCode::Tab => KeyToken::Named(NamedKeyToken::Tab),
            KeyCode::Esc => KeyToken::Named(NamedKeyToken::Escape),
            KeyCode::Backspace => KeyToken::Named(NamedKeyToken::Backspace),
            _ => return None,
        };

        // For non-alphabetic characters, Shift is already encoded in the
        // character itself (e.g. Shift+/ = '?'), so don't duplicate it in
        // the modifiers. This lets a binding like "ctrl+?" match the real
        // Ctrl+Shift+/ keypress regardless of whether the terminal reports
        // the SHIFT flag for symbol keys.
        let shift = raw_shift && !matches!(key.code, KeyCode::Char(c) if !c.is_ascii_alphabetic());

        Some(Self {
            modifiers: KeyModifiers {
                control,
                shift,
                alt,
            },
            key: token,
        })
    }
}

impl fmt::Display for KeyStroke {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::with_capacity(4);
        if self.modifiers.control {
            parts.push("ctrl".to_owned());
        }
        if self.modifiers.shift {
            parts.push("shift".to_owned());
        }
        if self.modifiers.alt {
            parts.push("alt".to_owned());
        }
        parts.push(self.key.to_string());
        write!(f, "{}", parts.join("+"))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyModifiers {
    pub control: bool,
    pub shift: bool,
    pub alt: bool,
}

impl KeyModifiers {
    fn apply_token(&mut self, token: &str) -> Result<(), String> {
        let normalized = token.trim().to_ascii_lowercase();
        let enabled = match normalized.as_str() {
            "ctrl" | "control" => &mut self.control,
            "shift" => &mut self.shift,
            "alt" | "option" => &mut self.alt,
            other => return Err(format!("unknown modifier: {other}")),
        };
        if *enabled {
            return Err(format!("duplicate modifier: {normalized}"));
        }
        *enabled = true;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeyToken {
    Character(String),
    Named(NamedKeyToken),
}

impl KeyToken {
    fn parse(token: &str) -> Result<Self, String> {
        let token = token.trim().to_ascii_lowercase();
        if token.is_empty() {
            return Err("empty key".to_owned());
        }
        let named = match token.as_str() {
            "left" | "arrowleft" => Some(NamedKeyToken::Left),
            "right" | "arrowright" => Some(NamedKeyToken::Right),
            "up" | "arrowup" => Some(NamedKeyToken::Up),
            "down" | "arrowdown" => Some(NamedKeyToken::Down),
            "delete" | "del" => Some(NamedKeyToken::Delete),
            "insert" | "ins" => Some(NamedKeyToken::Insert),
            "home" => Some(NamedKeyToken::Home),
            "end" => Some(NamedKeyToken::End),
            "pageup" | "page-up" | "page_up" => Some(NamedKeyToken::PageUp),
            "pagedown" | "page-down" | "page_down" => Some(NamedKeyToken::PageDown),
            "enter" | "return" => Some(NamedKeyToken::Enter),
            "tab" => Some(NamedKeyToken::Tab),
            "esc" | "escape" => Some(NamedKeyToken::Escape),
            "space" => Some(NamedKeyToken::Space),
            "backspace" | "back" | "bs" => Some(NamedKeyToken::Backspace),
            _ => None,
        };
        if let Some(named) = named {
            return Ok(Self::Named(named));
        }
        let character = match token.as_str() {
            "plus" => "+",
            "minus" => "-",
            "equal" | "equals" => "=",
            other => other,
        };
        if character.chars().count() == 1 {
            return Ok(Self::Character(character.to_owned()));
        }
        Err(format!("unknown key: {token}"))
    }
}

impl fmt::Display for KeyToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Character(text) => match text.as_str() {
                "+" => write!(f, "plus"),
                "-" => write!(f, "minus"),
                "=" => write!(f, "equal"),
                _ => write!(f, "{text}"),
            },
            Self::Named(named) => write!(f, "{named}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NamedKeyToken {
    Left,
    Right,
    Up,
    Down,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Enter,
    Tab,
    Escape,
    Space,
    Backspace,
}

impl fmt::Display for NamedKeyToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Up => "up",
            Self::Down => "down",
            Self::Delete => "delete",
            Self::Insert => "insert",
            Self::Home => "home",
            Self::End => "end",
            Self::PageUp => "pageup",
            Self::PageDown => "pagedown",
            Self::Enter => "enter",
            Self::Tab => "tab",
            Self::Escape => "escape",
            Self::Space => "space",
            Self::Backspace => "backspace",
        };
        write!(f, "{text}")
    }
}

/// A binding lookup table from parsed strokes to a frontend's action enum.
/// Generic over the action so the mux (`AppAction`) and the session TUI
/// (`SessionAction`) share one substrate.
#[derive(Clone, Debug)]
pub struct Keymap<A> {
    bindings: HashMap<KeyStroke, A>,
}

impl<A> Default for Keymap<A> {
    fn default() -> Self {
        Self {
            bindings: HashMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeymapMatch<A> {
    NoMatch,
    Triggered(A),
}

/// Declares one mappable action: a stable name (the config key), default
/// binding strings, and a description for help output.
pub struct BindingSpec<A: Copy> {
    pub action: A,
    pub name: &'static str,
    pub defaults: &'static [&'static str],
    pub desc: &'static str,
}

impl<A: Copy + Eq> Keymap<A> {
    /// Build a keymap from per-action override strings (action name →
    /// binding). An invalid or multi-stroke override falls back to that
    /// action's defaults.
    pub fn from_overrides(overrides: &HashMap<String, String>, specs: &[BindingSpec<A>]) -> Self {
        let mut keymap = Self::default();
        for spec in specs {
            let configured = overrides
                .get(spec.name)
                .and_then(|s| KeyStroke::parse(s).ok());
            match configured {
                Some(stroke) => keymap.insert(spec.action, stroke),
                None => {
                    for default in spec.defaults {
                        if let Ok(stroke) = KeyStroke::parse(default) {
                            keymap.insert(spec.action, stroke);
                        }
                    }
                }
            }
        }
        keymap
    }

    /// The binding string(s) currently bound to `action`, for the help overlay.
    pub fn bindings_for(&self, action: A) -> Vec<String>
    where
        A: Ord,
    {
        let mut found: Vec<String> = self
            .bindings
            .iter()
            .filter(|(_, bound)| **bound == action)
            .map(|(stroke, _)| stroke.to_string())
            .collect();
        found.sort();
        found
    }

    pub fn lookup(&self, stroke: &KeyStroke) -> KeymapMatch<A> {
        self.bindings
            .get(stroke)
            .copied()
            .map(KeymapMatch::Triggered)
            .unwrap_or(KeymapMatch::NoMatch)
    }

    fn insert(&mut self, action: A, stroke: KeyStroke) {
        self.bindings.entry(stroke).or_insert(action);
    }
}

/// Trait bridging `BindingSpec` to `Keymap::from_overrides`; kept for
/// future per-frontend spec tables that delegate name lookups. Currently
/// `from_overrides` reads `BindingSpec::name` directly.
pub trait BindingEntry<A: Copy> {
    fn name(&self) -> &str;
}

impl<A: Copy> BindingEntry<A> for BindingSpec<A> {
    fn name(&self) -> &str {
        self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(text: &str) -> KeyStroke {
        KeyStroke::parse(text).unwrap()
    }

    #[test]
    fn parse_normalizes_aliases_and_display() {
        let parsed = KeyStroke::parse("  Control + Option + Page_Down ").unwrap();
        assert_eq!(parsed.to_string(), "ctrl+alt+pagedown");
        assert_eq!(KeyStroke::parse("esc").unwrap().to_string(), "escape");
        assert_eq!(
            KeyStroke::parse("ctrl+plus").unwrap().to_string(),
            "ctrl+plus"
        );
        assert!(KeyStroke::parse("hyper+x").is_err());
        assert!(KeyStroke::parse("ctrl+ctrl+x").is_err());
        assert!(KeyStroke::parse("ctrl+f13").is_err());
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum TestAction {
        Quit,
        New,
    }

    #[test]
    fn keymap_overrides_and_lookup() {
        let specs = [BindingSpec {
            action: TestAction::Quit,
            name: "quit",
            defaults: &["ctrl+q"],
            desc: "",
        }];
        let overrides = HashMap::from([("quit".to_owned(), "ctrl+x".to_owned())]);
        let km = Keymap::from_overrides(&overrides, &specs);
        assert_eq!(
            km.lookup(&stroke("ctrl+x")),
            KeymapMatch::Triggered(TestAction::Quit)
        );
        assert_eq!(km.lookup(&stroke("ctrl+q")), KeymapMatch::NoMatch);
    }

    #[test]
    fn invalid_override_falls_back() {
        let specs = [BindingSpec {
            action: TestAction::Quit,
            name: "quit",
            defaults: &["ctrl+q"],
            desc: "",
        }];
        let overrides = HashMap::from([("quit".to_owned(), "nope".to_owned())]);
        let km = Keymap::from_overrides(&overrides, &specs);
        assert_eq!(
            km.lookup(&stroke("ctrl+q")),
            KeymapMatch::Triggered(TestAction::Quit)
        );
    }
}
