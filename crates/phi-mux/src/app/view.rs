//! Per-client view state (WS8.2).
//!
//! Everything one attached client needs to look at the shared workspace:
//! which session it is focused on, its canvas size, its compositing
//! surface and renderer diff state, sidebar scroll, `:` command line,
//! overlays, terminal selection, and the transient status note. Session
//! content itself stays canonical in the core (`Workspace` + handles).
//!
//! [`MuxApp`](super::MuxApp) holds one `MuxView` per attached client, keyed
//! by client id (a single view with id 0 in local mode). Input is routed to
//! the sending client's view; every view renders its own frame at its own
//! size through its own diff renderer.

use std::time::Instant;

use super::ActiveExtensionOverlay;
use crate::command_line::CommandLineState;
use crate::grid::ansi::AnsiRenderer;
use crate::grid::cell_surface::CellSurface;
use crate::grid::selection::TerminalSelection;
use crate::ui::scene::StatusNoteKind;
use crate::workspace::Focus;

pub(crate) struct MuxView {
    /// This view's selection (focused project + per-project session).
    pub(crate) focus: Focus,
    /// This view's canvas size (cols, rows).
    pub(crate) size: (u16, u16),
    /// Persistent compositing surface, reused across frames so the
    /// `StringInterner` isn't rebuilt and text re-interned every frame, and
    /// so dirty tracking is relative to the previous frame's content.
    pub(crate) surface: CellSurface,
    /// ANSI diff renderer; its row-hash state is relative to this view's
    /// previous frame.
    pub(crate) renderer: AnsiRenderer,
    pub(crate) sidebar_scroll: usize,
    pub(crate) command_line: Option<CommandLineState>,
    /// The currently open extension overlay (if any).
    pub(crate) active_extension_overlay: Option<ActiveExtensionOverlay>,
    pub(crate) selection: TerminalSelection,
    pub(crate) note: Option<(String, StatusNoteKind, Instant)>,
    /// Whether the focused session's screen was in the alternate screen last
    /// frame; a transition drops scroll history and forces a full repaint.
    pub(crate) alt_screen_active: bool,
    /// Cached total scrollback lines for the focused session. Recomputed only
    /// when a scrollback-affecting event occurs (session switch, new content),
    /// not every frame — avoiding the set_scrollback(MAX)/restore dance that
    /// mutates shared vt100 state on every render.
    pub(crate) cached_scrollback_total: usize,
    pub(crate) render_failures: u32,
    pub(crate) renderer_invalidated: bool,
}

impl MuxView {
    pub(crate) fn overlay_active(&self) -> bool {
        self.active_extension_overlay.is_some()
    }

    pub(crate) fn new(focus: Focus, size: (u16, u16)) -> Self {
        Self {
            focus,
            size,
            surface: CellSurface::default(),
            renderer: AnsiRenderer::default(),
            sidebar_scroll: 0,
            command_line: None,
            active_extension_overlay: None,
            selection: TerminalSelection::default(),
            note: None,
            alt_screen_active: false,
            cached_scrollback_total: 0,
            render_failures: 0,
            renderer_invalidated: false,
        }
    }
}
