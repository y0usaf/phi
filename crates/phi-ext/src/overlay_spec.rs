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
//! is opened.

use std::any::Any;
use std::sync::Arc;

use crate::draw_context::{DrawContext, OverlayKey};

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
/// Extensions register overlays with a name, a toggle key (which the
/// TUI binds to open the overlay), and render + key-handler closures.
///
/// The `state` field is a type-erased factory that creates the overlay's
/// initial state when it's opened. The TUI stores the resulting `Box<dyn Any>`
/// and passes it to the render and key-handler closures.
#[derive(Clone)]
pub struct OverlaySpec {
    pub name: String,
    pub description: String,
    /// The key that toggles this overlay, expressed as a KeyStroke string
    /// (e.g. "ctrl+?", "a", "ctrl+shift+n"). The TUI parses and matches
    /// this against incoming key events with full modifier support.
    pub toggle_key: String,
    /// Factory closure: creates the initial state when the overlay opens.
    pub init_state: Arc<dyn Fn() -> Box<dyn Any + Send + Sync> + Send + Sync>,
    /// Render the overlay into the given draw context.
    /// Receives the mutable state (type-erased) and the surface dimensions.
    pub render: Arc<dyn Fn(&mut dyn DrawContext, &mut Box<dyn Any + Send + Sync>) + Send + Sync>,
    /// Handle a key event.
    /// Receives the mutable state and the terminal dimensions.
    pub handle_key: Arc<
        dyn Fn(&mut Box<dyn Any + Send + Sync>, &OverlayKey, u16, u16) -> OverlayOutcome
            + Send
            + Sync,
    >,
}

impl OverlaySpec {
    /// Create a new overlay spec.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        toggle_key: impl Into<String>,
        init_state: Arc<dyn Fn() -> Box<dyn Any + Send + Sync> + Send + Sync>,
        render: Arc<dyn Fn(&mut dyn DrawContext, &mut Box<dyn Any + Send + Sync>) + Send + Sync>,
        handle_key: Arc<
            dyn Fn(&mut Box<dyn Any + Send + Sync>, &OverlayKey, u16, u16) -> OverlayOutcome
                + Send
                + Sync,
        >,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            toggle_key: toggle_key.into(),
            init_state,
            render,
            handle_key,
        }
    }
}
