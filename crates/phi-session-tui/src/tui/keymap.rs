//! Inline session TUI keymap (WS6.3): the single-stroke shortcuts that are
//! safe to remappable config bindings. Shares the `phi_tui::keymap` parser
//! with the mux so both frontends use one binding-string grammar.
//!
//! Stateful shortcuts (double-Esc tree, double-Ctrl+C quit, Tab interrupt,
//! Enter submit/login) stay hardcoded in `events.rs` — they depend on
//! multi-keystroke timing or editor state, not a single stroke. Everything
//! routed here is a named, remappable binding read from config.lua's
//! `keybinds` table.

use std::collections::HashMap;

use phi_tui::keymap::{BindingSpec, KeyStroke, Keymap, KeymapMatch};

/// Remappable single-stroke actions for the inline session TUI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionAction {
    /// Restore a queued (un-sent) message back to the editor (Alt+Up).
    RestoreQueued,
    /// Cycle the root/sub model (Ctrl+P; Shift cycles direction).
    CycleModel,
    /// Open the model picker overlay (Ctrl+L).
    ModelPicker,
    /// Toggle thinking on/off (Ctrl+T).
    ToggleThinking,
    /// Hand the editor text to `$EDITOR` (Ctrl+G).
    ExternalEdit,
    /// Cycle the thinking level (BackTab / Shift+Tab).
    CycleThinkingLevel,
    /// Toggle the tool-result panel expand/collapse (Ctrl+O).
    ToggleTools,
}

pub type SessionBindingSpec = BindingSpec<SessionAction>;

/// Defaults mirror the hardcoded keys WS6.3 replaces. Override any of them
/// from config.lua's `[keybinds]` table by action name.
pub const SESSION_BINDINGS: &[SessionBindingSpec] = &[
    SessionBindingSpec {
        action: SessionAction::RestoreQueued,
        name: "restore_queued",
        defaults: &["alt+up"],
        desc: "restore the queued message to the editor",
    },
    SessionBindingSpec {
        action: SessionAction::CycleModel,
        name: "cycle_model",
        defaults: &["ctrl+p", "shift+ctrl+p"],
        desc: "cycle the model (shift to reverse)",
    },
    SessionBindingSpec {
        action: SessionAction::ModelPicker,
        name: "model_picker",
        defaults: &["ctrl+l"],
        desc: "open the model picker",
    },
    SessionBindingSpec {
        action: SessionAction::ToggleThinking,
        name: "toggle_thinking",
        defaults: &["ctrl+t"],
        desc: "toggle thinking on/off",
    },
    SessionBindingSpec {
        action: SessionAction::ExternalEdit,
        name: "external_edit",
        defaults: &["ctrl+g"],
        desc: "edit the input in $EDITOR",
    },
    SessionBindingSpec {
        action: SessionAction::CycleThinkingLevel,
        name: "cycle_thinking_level",
        defaults: &["shift+tab"],
        desc: "cycle the thinking level",
    },
    SessionBindingSpec {
        action: SessionAction::ToggleTools,
        name: "toggle_tools",
        defaults: &["ctrl+o"],
        desc: "expand/collapse the tool result panel",
    },
];

/// Build the session keymap from config overrides. Unknown/invalid
/// overrides fall back to each action's defaults (mirrors the mux).
pub fn session_keymap(overrides: &HashMap<String, String>) -> Keymap<SessionAction> {
    Keymap::from_overrides(overrides, SESSION_BINDINGS)
}

/// Try to interpret a crossterm key as a remappable session action. Returns
/// `None` when the key is not bound (caller falls back to its own handling).
pub fn lookup_session_action(
    keymap: &Keymap<SessionAction>,
    key: &phi_tui::crossterm::event::KeyEvent,
) -> Option<SessionAction> {
    let stroke = KeyStroke::from_key_event(key)?;
    match keymap.lookup(&stroke) {
        KeymapMatch::Triggered(action) => Some(action),
        KeymapMatch::NoMatch => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn defaults_bind_expected_strokes() {
        let km = session_keymap(&HashMap::new());
        let stroke = |s: &str| KeyStroke::parse(s).unwrap();
        assert_eq!(
            km.lookup(&stroke("ctrl+o")),
            KeymapMatch::Triggered(SessionAction::ToggleTools)
        );
        assert_eq!(
            km.lookup(&stroke("shift+tab")),
            KeymapMatch::Triggered(SessionAction::CycleThinkingLevel)
        );
        assert_eq!(km.lookup(&stroke("ctrl+x")), KeymapMatch::NoMatch);
    }

    #[test]
    fn override_replaces_default() {
        let overrides = HashMap::from([("toggle_tools".to_owned(), "ctrl+b".to_owned())]);
        let km = session_keymap(&overrides);
        let stroke = |s: &str| KeyStroke::parse(s).unwrap();
        assert_eq!(
            km.lookup(&stroke("ctrl+b")),
            KeymapMatch::Triggered(SessionAction::ToggleTools)
        );
        assert_eq!(km.lookup(&stroke("ctrl+o")), KeymapMatch::NoMatch);
    }
}
