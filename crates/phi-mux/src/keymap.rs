//! Mux actions and single-stroke keymap: key strokes parse from strings like
//! "ctrl+n", defaults per action can be overridden from Lua `mux.keybinds`.

use std::collections::HashMap;
use std::fmt;

use phi_tui::crossterm::event::{self, KeyCode, KeyEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AppAction {
    PreviousProject,
    NextProject,
    PreviousSession,
    NextSession,
    NewSession,
    RefreshSession,
    RefreshAllSessions,
    ArchiveSession,
    RemoveProject,
    CopySelection,
    PasteClipboard,
    Help,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionSpec {
    pub action: AppAction,
    pub name: &'static str,
    pub defaults: &'static [&'static str],
    pub desc: &'static str,
}

pub const ACTION_SPECS: &[ActionSpec] = &[
    ActionSpec {
        action: AppAction::PreviousProject,
        name: "project_prev",
        defaults: &["ctrl+left"],
        desc: "focus the previous project",
    },
    ActionSpec {
        action: AppAction::NextProject,
        name: "project_next",
        defaults: &["ctrl+right"],
        desc: "focus the next project",
    },
    ActionSpec {
        action: AppAction::PreviousSession,
        name: "session_prev",
        defaults: &["ctrl+up"],
        desc: "focus the previous session",
    },
    ActionSpec {
        action: AppAction::NextSession,
        name: "session_next",
        defaults: &["ctrl+down"],
        desc: "focus the next session",
    },
    ActionSpec {
        action: AppAction::NewSession,
        name: "new_session",
        defaults: &["ctrl+n"],
        desc: "new session in the focused project",
    },
    ActionSpec {
        action: AppAction::RefreshSession,
        name: "refresh_session",
        defaults: &["ctrl+r"],
        desc: "restart/rescan the focused session",
    },
    ActionSpec {
        action: AppAction::RefreshAllSessions,
        name: "refresh_all_sessions",
        defaults: &["ctrl+shift+r"],
        desc: "rescan every project",
    },
    ActionSpec {
        action: AppAction::ArchiveSession,
        name: "archive_session",
        defaults: &["ctrl+delete"],
        desc: "archive the focused session",
    },
    ActionSpec {
        action: AppAction::RemoveProject,
        name: "remove_project",
        defaults: &["ctrl+shift+delete", "ctrl+shift+d"],
        desc: "unavailable when sidebar mirrors live session files",
    },
    ActionSpec {
        action: AppAction::CopySelection,
        name: "copy_selection",
        defaults: &["ctrl+shift+c"],
        desc: "copy the mouse selection",
    },
    ActionSpec {
        action: AppAction::PasteClipboard,
        name: "paste_clipboard",
        defaults: &["ctrl+v", "shift+insert"],
        desc: "paste into the focused session",
    },
    ActionSpec {
        action: AppAction::Help,
        name: "help",
        defaults: &["ctrl+g"],
        desc: "toggle the help overlay",
    },
    ActionSpec {
        action: AppAction::Quit,
        name: "quit",
        defaults: &["ctrl+q"],
        desc: "quit the mux",
    },
];

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
        let modifiers = KeyModifiers {
            control: key.modifiers.contains(event::KeyModifiers::CONTROL),
            shift: key.modifiers.contains(event::KeyModifiers::SHIFT),
            alt: key.modifiers.contains(event::KeyModifiers::ALT),
        };
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
        Some(Self {
            modifiers,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeymapMatch {
    NoMatch,
    Triggered(AppAction),
}

#[derive(Clone, Debug, Default)]
pub struct Keymap {
    bindings: HashMap<KeyStroke, AppAction>,
}

impl Keymap {
    /// Defaults overridden per action by Lua `mux.keybinds`; an invalid
    /// override string falls back to that action's defaults. Multi-stroke
    /// bindings are ignored.
    pub fn from_overrides(overrides: &HashMap<String, String>) -> Self {
        let mut keymap = Self::default();
        for spec in ACTION_SPECS {
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
    pub fn bindings_for(&self, action: AppAction) -> Vec<String> {
        let mut found: Vec<String> = self
            .bindings
            .iter()
            .filter(|(_, bound)| **bound == action)
            .map(|(stroke, _)| stroke.to_string())
            .collect();
        found.sort();
        found
    }

    pub fn lookup(&self, stroke: &KeyStroke) -> KeymapMatch {
        self.bindings
            .get(stroke)
            .copied()
            .map(KeymapMatch::Triggered)
            .unwrap_or(KeymapMatch::NoMatch)
    }

    fn insert(&mut self, action: AppAction, stroke: KeyStroke) {
        self.bindings.entry(stroke).or_insert(action);
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

    #[test]
    fn single_stroke_bindings_trigger() {
        let overrides = HashMap::from([("new_session".to_owned(), "ctrl+n".to_owned())]);
        let keymap = Keymap::from_overrides(&overrides);
        assert_eq!(
            keymap.lookup(&stroke("ctrl+n")),
            KeymapMatch::Triggered(AppAction::NewSession)
        );
    }

    #[test]
    fn invalid_override_falls_back_to_defaults() {
        let overrides = HashMap::from([(
            "archive_session".to_owned(),
            "definitely-not-a-key".to_owned(),
        )]);
        let keymap = Keymap::from_overrides(&overrides);
        assert_eq!(
            keymap.lookup(&stroke("ctrl+delete")),
            KeymapMatch::Triggered(AppAction::ArchiveSession)
        );
    }

    #[test]
    fn multi_stroke_override_falls_back_to_defaults() {
        let overrides = HashMap::from([("new_session".to_owned(), "ctrl+k n".to_owned())]);
        let keymap = Keymap::from_overrides(&overrides);
        assert_eq!(
            keymap.lookup(&stroke("ctrl+n")),
            KeymapMatch::Triggered(AppAction::NewSession)
        );
        assert_eq!(keymap.lookup(&stroke("ctrl+k")), KeymapMatch::NoMatch);
    }

    #[test]
    fn override_replaces_all_defaults_for_that_action() {
        let overrides = HashMap::from([("paste_clipboard".to_owned(), "shift+insert".to_owned())]);
        let keymap = Keymap::from_overrides(&overrides);
        assert_eq!(
            keymap.lookup(&stroke("shift+insert")),
            KeymapMatch::Triggered(AppAction::PasteClipboard)
        );
        assert_eq!(keymap.lookup(&stroke("ctrl+v")), KeymapMatch::NoMatch);
    }

    #[test]
    fn key_event_conversion_lowercases_shifted_chars() {
        use phi_tui::crossterm::event::{KeyEvent, KeyModifiers as CtMods};
        let key = KeyEvent::new(KeyCode::Char('N'), CtMods::CONTROL | CtMods::SHIFT);
        let converted = KeyStroke::from_key_event(&key).unwrap();
        assert_eq!(converted, stroke("ctrl+shift+n"));
    }

    #[test]
    fn bindings_for_lists_default_bindings() {
        let keymap = Keymap::from_overrides(&HashMap::new());
        assert_eq!(keymap.bindings_for(AppAction::Quit), vec!["ctrl+q"]);
        let paste = keymap.bindings_for(AppAction::PasteClipboard);
        assert_eq!(paste, vec!["ctrl+v", "shift+insert"]);
    }
}
