//! Overlay registration: extensions can register named overlay modals
//! that the TUI renders on top of the normal chrome.
//!
//! An overlay is a pair of closures:
//! - `render`: draws into a `&mut dyn DrawContext`
//! - `handle_key`: processes a key event and returns an `OverlayOutcome`
//!
//! The overlay's state is owned by the extension via a `Box<dyn Any + Send + Sync>`
//! that the TUI stores while the overlay is active. The extension provides
//! an `init_state` closure that creates the initial state when the overlay
//! is opened. The host may pass an [`OverlayPayload`] when it opens the
//! overlay programmatically (e.g. a model picker request carrying the model
//! list and a response channel); toggle-key opens pass `None`.

use std::any::Any;
use std::sync::Arc;

use crate::draw_context::{DrawContext, OverlayKey};

/// Type-erased overlay state, created by `init_state` and threaded through
/// `render` / `handle_key` / `handle_scroll` while the overlay is open.
pub type OverlayState = Box<dyn Any + Send + Sync>;

/// Type-erased payload the host passes when opening an overlay
/// programmatically. The overlay downcasts it to the concrete payload type
/// it was registered to consume (see [`ModelPickerPayload`],
/// [`HelpPayload`]).
pub type OverlayPayload = Box<dyn Any + Send + Sync>;

/// Factory closure: creates the initial state when the overlay opens.
/// Receives the host-supplied payload for programmatic opens, `None` for
/// toggle-key opens.
pub type OverlayInitFn = Arc<dyn Fn(Option<OverlayPayload>) -> OverlayState + Send + Sync>;

/// Render the overlay into the given draw context.
pub type OverlayRenderFn = Arc<dyn Fn(&mut dyn DrawContext, &mut OverlayState) + Send + Sync>;

/// Handle a key event. Receives the mutable state and terminal dimensions.
pub type OverlayKeyFn =
    Arc<dyn Fn(&mut OverlayState, &OverlayKey, u16, u16) -> OverlayOutcome + Send + Sync>;

/// Handle a mouse-wheel scroll. Receives the mutable state, the wheel delta
/// in lines (positive = down), and terminal dimensions.
pub type OverlayScrollFn = Arc<dyn Fn(&mut OverlayState, i32, u16, u16) + Send + Sync>;

/// Well-known overlay names the core opens by name. Built-ins register
/// under these names; an extension can replace one by registering the same
/// name (last registration wins in the runtime's overlay map).
pub const OVERLAY_USAGE: &str = "phi:usage";
pub const OVERLAY_MODEL: &str = "phi:model";
pub const OVERLAY_HELP: &str = "phi:help";

/// Outcome of processing a key event in an overlay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverlayOutcome {
    /// The key was consumed but no state change occurred.
    None,
    /// Close the overlay.
    Close,
    /// Close the overlay and submit a result string.
    Submit(String),
}

/// A registered overlay spec.
///
/// Overlays are modal UI panels drawn on top of the normal TUI chrome.
/// Extensions register overlays with a name, an optional toggle key (which
/// the TUI binds to open/close the overlay), and render + key-handler
/// closures. Overlays without a toggle key are opened programmatically by
/// the host (commands, host modal requests) via their name.
#[derive(Clone)]
pub struct OverlaySpec {
    pub name: String,
    pub description: String,
    /// The key that toggles this overlay, expressed as a KeyStroke string
    /// (e.g. "ctrl+?", "a", "ctrl+shift+n"). `None` means the overlay can
    /// only be opened programmatically by name.
    pub toggle_key: Option<String>,
    pub init_state: OverlayInitFn,
    pub render: OverlayRenderFn,
    pub handle_key: OverlayKeyFn,
    /// Optional mouse-wheel scroll handler.
    pub handle_scroll: Option<OverlayScrollFn>,
}

impl OverlaySpec {
    /// Create a new overlay spec with no toggle key (programmatic open only).
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        init_state: OverlayInitFn,
        render: OverlayRenderFn,
        handle_key: OverlayKeyFn,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            toggle_key: None,
            init_state,
            render,
            handle_key,
            handle_scroll: None,
        }
    }

    /// Bind a toggle key that opens (and closes) the overlay.
    pub fn with_toggle_key(mut self, toggle_key: impl Into<String>) -> Self {
        let key = toggle_key.into();
        self.toggle_key = (!key.is_empty()).then_some(key);
        self
    }

    /// Attach a mouse-wheel scroll handler.
    pub fn with_scroll(mut self, handle_scroll: OverlayScrollFn) -> Self {
        self.handle_scroll = Some(handle_scroll);
        self
    }
}

/// The result an [`OverlayResponder`] carries back to the requester:
/// `Ok(Some(value))` for a selection, `Ok(None)` for a dismissal.
pub type OverlayResponse = Result<Option<String>, String>;

type OverlayRespondFn = Box<dyn FnOnce(OverlayResponse) + Send + Sync>;

/// One-shot response channel handed to an overlay through its payload.
///
/// Wraps a host callback (e.g. a `oneshot::Sender` back to the requesting
/// session). If the overlay never responds — closed by a toggle key,
/// replaced by another overlay, or dropped on quit — the `Drop` impl fires
/// the callback with `Ok(None)` ("dismissed without a selection") so the
/// requester is never left hanging.
pub struct OverlayResponder(Option<OverlayRespondFn>);

impl OverlayResponder {
    pub fn new(respond: impl FnOnce(OverlayResponse) + Send + Sync + 'static) -> Self {
        Self(Some(Box::new(respond)))
    }

    /// Fire the response. Subsequent calls (and the drop guard) are no-ops.
    pub fn respond(&mut self, result: OverlayResponse) {
        if let Some(respond) = self.0.take() {
            respond(result);
        }
    }
}

impl Drop for OverlayResponder {
    fn drop(&mut self) {
        self.respond(Ok(None));
    }
}

impl std::fmt::Debug for OverlayResponder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("OverlayResponder")
            .field(&self.0.as_ref().map(|_| "pending").unwrap_or("fired"))
            .finish()
    }
}

/// Payload for the [`OVERLAY_MODEL`] picker: the candidate list, the current
/// selection, and the response channel the picked value is sent through.
pub struct ModelPickerPayload {
    /// `true` when picking the root model, `false` for the sub model.
    pub root: bool,
    /// The currently active `provider/id` value.
    pub current: String,
    /// Candidate `provider/id` values.
    pub models: Vec<String>,
    pub responder: OverlayResponder,
}

/// One line of the [`OVERLAY_HELP`] overlay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelpEntry {
    /// A section heading.
    Heading(String),
    /// A key/description row.
    Item { keys: String, description: String },
    /// A blank spacer line.
    Blank,
}

/// Payload for the [`OVERLAY_HELP`] overlay: the host's keybinding and
/// command reference, built fresh at open time so live keymap overrides
/// are reflected.
pub struct HelpPayload {
    pub entries: Vec<HelpEntry>,
}
