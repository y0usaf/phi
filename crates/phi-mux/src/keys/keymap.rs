//! Mux actions and single-stroke keymap: key strokes parse from strings like
//! "ctrl+n", defaults per action can be overridden from Lua `mux.keybinds`.
//!
//! The key-stroke parser (`KeyStroke`/`KeyToken`/`KeyModifiers`) lives in
//! `phi_tui::keymap` and is shared with the inline session TUI (WS6.3) so
//! both frontends share one binding-string grammar.

use phi_tui::crossterm::event::KeyEvent;
use phi_tui::keymap::BindingSpec;

pub use phi_tui::keymap::{KeyModifiers, KeyStroke, KeyToken, Keymap, KeymapMatch, NamedKeyToken};

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

/// Per-action binding declaration. `BindingSpec` is the shared shape; the
/// mux specializes it with `AppAction`.
pub type ActionSpec = BindingSpec<AppAction>;

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

/// `KeyStroke::from_key_event` re-exported as a free fn for the mux's call
/// sites that still import it from here.
pub fn stroke_from_key_event(key: &KeyEvent) -> Option<KeyStroke> {
    KeyStroke::from_key_event(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phi_tui::crossterm::event::KeyCode;
    use std::collections::HashMap;

    fn stroke(text: &str) -> KeyStroke {
        KeyStroke::parse(text).unwrap()
    }

    fn keymap(overrides: &HashMap<String, String>) -> Keymap<AppAction> {
        Keymap::from_overrides(overrides, ACTION_SPECS)
    }

    #[test]
    fn single_stroke_bindings_trigger() {
        let overrides = HashMap::from([("new_session".to_owned(), "ctrl+n".to_owned())]);
        let km = keymap(&overrides);
        assert_eq!(
            km.lookup(&stroke("ctrl+n")),
            KeymapMatch::Triggered(AppAction::NewSession)
        );
    }

    #[test]
    fn invalid_override_falls_back_to_defaults() {
        let overrides = HashMap::from([(
            "archive_session".to_owned(),
            "definitely-not-a-key".to_owned(),
        )]);
        let km = keymap(&overrides);
        assert_eq!(
            km.lookup(&stroke("ctrl+delete")),
            KeymapMatch::Triggered(AppAction::ArchiveSession)
        );
    }

    #[test]
    fn multi_stroke_override_falls_back_to_defaults() {
        let overrides = HashMap::from([("new_session".to_owned(), "ctrl+k n".to_owned())]);
        let km = keymap(&overrides);
        assert_eq!(
            km.lookup(&stroke("ctrl+n")),
            KeymapMatch::Triggered(AppAction::NewSession)
        );
        assert_eq!(km.lookup(&stroke("ctrl+k")), KeymapMatch::NoMatch);
    }

    #[test]
    fn override_replaces_all_defaults_for_that_action() {
        let overrides = HashMap::from([("paste_clipboard".to_owned(), "shift+insert".to_owned())]);
        let km = keymap(&overrides);
        assert_eq!(
            km.lookup(&stroke("shift+insert")),
            KeymapMatch::Triggered(AppAction::PasteClipboard)
        );
        assert_eq!(km.lookup(&stroke("ctrl+v")), KeymapMatch::NoMatch);
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
        let km = keymap(&HashMap::new());
        assert_eq!(km.bindings_for(AppAction::Quit), vec!["ctrl+q"]);
        let paste = km.bindings_for(AppAction::PasteClipboard);
        assert_eq!(paste, vec!["ctrl+v", "shift+insert"]);
    }
}
