//! The mux main loop: owns the real terminal, multiplexes input between the
//! sidebar, overlays, the `:` command line, and the focused embedded
//! session, and composites every frame from the session's vt100 screen.
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use phi_tui::crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use phi_tui::{AnimationClock, RawModeGuard};
use tokio::sync::mpsc;

use super::command_line::{CommandLineEvent, CommandLineState, command_line_start_col};
use super::ui::overlays::archive::{ArchiveOutcome, ArchiveViewerState, render_archive_viewer};
use super::ui::overlays::help::{HelpOverlayState, render_help_overlay};
use super::ui::overlays::model::{ModelOverlayOutcome, ModelOverlayState, render_model_overlay};
use super::ui::overlays::usage::{UsageOverlayState, render_usage_overlay};
use super::ui::palette::ScenePalette;
use super::ui::scene::{Chrome, MuxMode, StatusNoteKind, render_scene, statusline_status};
use super::ui::screen_snapshot::ScreenSnapshot;
use super::ui::sidebar::{
    SidebarRowKind, build_sidebar_rows, ensure_sidebar_selection_visible, project_sidebar_title,
    scroll_sidebar_by_rows_value, selected_sidebar_selection_span, session_sidebar_title,
    sidebar_viewport_items, sticky_sidebar_anchor_row,
};
use crate::clipboard::MuxClipboard;
use crate::driver::{self as driver, MuxChannels, SessionKey};
use crate::grid::ansi::{AnsiRenderer, HardwareCursor};
use crate::grid::cell_surface::CellSurface;
use crate::grid::layout::{CellLayout, SIDEBAR_WIDTH_DEFAULT, compute_cell_layout};
use crate::grid::selection::{SelectionPoint, TerminalSelection, selected_text};
use crate::grid::theme;
use crate::keys::{AppAction, KeyStroke, Keymap, KeymapMatch, MuxCommand, parse_command};
use crate::persisted::{self as persisted, PersistedState};
use crate::status::SessionStatus;
use crate::watcher::SessionWatcher;
use crate::workspace::{self as workspace, Workspace};
use phi_ext::{AppRuntime, OverlaySpec};
use phi_session::config::MuxConfig;
use phi_session_tui::session_builder::SharedResources;
use phi_session_tui::tui::{self, HostModalKind, HostModalRequest};

const NOTE_TTL: Duration = Duration::from_secs(4);
const WHEEL_LINES: i32 = 3;
const ARCHIVE_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

mod commands;
mod input;
mod rendering;
mod session;

pub async fn run(shared: SharedResources, mux_config: MuxConfig, cwd: PathBuf) -> Result<()> {
    install_panic_log_hook();

    let mut persisted = PersistedState::load_default().unwrap_or_default();
    let mut project_dirs: Vec<PathBuf> = persisted.projects.iter().map(PathBuf::from).collect();
    let push_project = |dirs: &mut Vec<PathBuf>, dir: PathBuf| {
        let dir = PathBuf::from(persisted::normalize_path(&dir));
        if dir.is_dir() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };
    push_project(&mut project_dirs, cwd.clone());
    for dir in &shared.projects {
        push_project(&mut project_dirs, PathBuf::from(dir));
    }
    project_dirs.retain(|dir| dir.is_dir());

    persisted.normalize();
    let mut workspace = Workspace::discover(project_dirs);

    if let Some(selected) = &persisted.selected_project {
        let selected = PathBuf::from(selected);
        if let Some(i) = workspace.projects.iter().position(|p| p.cwd == selected) {
            workspace.selected_project = i;
        }
    }

    let _ = workspace.ensure_project_session(workspace.selected_project);

    // Start filesystem watchers on each project's session directory so the
    // sidebar auto-refreshes when session files change (the filesystem is
    // the source of truth; the sidebar is a view over it).
    let (mut session_watcher, watcher_rx) = SessionWatcher::new();
    for (i, project) in workspace.projects.iter().enumerate() {
        session_watcher.add_watch(i, &project.cwd);
    }

    let guard = RawModeGuard::new().context("mux requires a tty")?;
    let mut stdout = std::io::stdout();
    let _ = phi_tui::crossterm::execute!(stdout, EnableMouseCapture);
    // The mux owns the whole screen (unlike the inline single-session TUI).
    let _ = write!(stdout, "\x1b[2J\x1b[H");

    // Probe the host terminal's colors (OSC 10/11/4) once, inside raw mode.
    let terminal_colors = phi_tui::detect_terminal_colors(Duration::from_millis(150))
        .unwrap_or_else(theme::default_terminal_colors);

    // ── Visual extension discovery ──────────────────────────────────────
    // Build a lightweight runtime that discovers and registers extensions
    // from the project's `.phi/` directory and global config. This lets
    // extensions register themes, spinners, thinking expressions, and text
    // effects that the mux TUI consumes for its look-and-feel.
    //
    // The runtime is built separately from the per-session runtime (which
    // is created later inside `build_session`), but visual specs are static
    // data registered at `register()` time, so the same extensions will
    // register the same visual specs in both runtimes.
    let visual_runtime = build_visual_runtime(&cwd);

    // Derive the mux palette: if an extension registered a theme, use it
    // (overlaid on the built-in derivation so missing roles fall back).
    // Otherwise, use the built-in `generate_theme` path — the same gray
    // ramp / ANSI mapping as before.
    let palette = if let Some(rt) = &visual_runtime {
        if let Some(theme_spec) = rt.first_theme() {
            ScenePalette::from_extension_theme(theme_spec, &terminal_colors)
        } else {
            ScenePalette::from_terminal_colors(&terminal_colors)
        }
    } else {
        ScenePalette::from_terminal_colors(&terminal_colors)
    };

    let (status_tx, status_rx) = mpsc::unbounded_channel();
    let (redraw_tx, redraw_rx) = mpsc::unbounded_channel();
    let (suspend_tx, suspend_rx) = mpsc::unbounded_channel();
    let (host_modal_tx, host_modal_rx) = mpsc::unbounded_channel();

    let mut app = MuxApp {
        shared,
        workspace,
        keymap: Keymap::from_overrides(mux_config.keybinds.as_ref().unwrap_or(&Default::default())),
        sidebar_width: mux_config.sidebar_width.unwrap_or(SIDEBAR_WIDTH_DEFAULT),
        command_line: None,
        help_overlay: None,
        usage_overlay: None,
        model_overlay: None,
        archive_viewer: None,
        extension_overlays: visual_runtime
            .as_ref()
            .map(|rt| rt.all_overlays().cloned().collect())
            .unwrap_or_default(),
        active_extension_overlay: None,
        selection: TerminalSelection::default(),
        clipboard: MuxClipboard::default(),
        renderer: AnsiRenderer::default(),
        size: phi_tui::crossterm::terminal::size().unwrap_or((80, 24)),
        surface: CellSurface::default(),
        palette,
        terminal_colors,
        visual_runtime,
        alt_screen_active: false,
        cached_scrollback_total: 0,
        sidebar_scroll: 0,
        note: None,
        clock: AnimationClock::with_baseline_keep_alive(),
        channels: MuxChannels {
            status_tx,
            redraw_tx,
            suspend_tx,
            host_modal_tx,
        },
        session_watcher,
        quit: false,
        render_failures: 0,
        renderer_invalidated: false,
    };

    let result = app
        .main_loop(
            &guard,
            status_rx,
            redraw_rx,
            suspend_rx,
            host_modal_rx,
            watcher_rx,
        )
        .await;

    app.persist_now();
    PersistedState::flush();
    // Drop sessions before restoring the terminal so their final writes land
    // in the in-memory parsers, not the real screen.
    drop(app);
    let _ = phi_tui::crossterm::execute!(stdout, DisableMouseCapture);
    let _ = write!(stdout, "\x1b[2J\x1b[H\x1b[?25h");
    let _ = stdout.flush();
    drop(guard);
    result
}

struct MuxApp {
    shared: SharedResources,
    workspace: Workspace,
    keymap: Keymap,
    sidebar_width: u16,
    command_line: Option<CommandLineState>,
    help_overlay: Option<HelpOverlayState>,
    usage_overlay: Option<UsageOverlayState>,
    model_overlay: Option<ModelOverlayState>,
    archive_viewer: Option<ArchiveViewerState>,
    /// Extension-registered overlay specs, cloned from the visual runtime.
    extension_overlays: Vec<OverlaySpec>,
    /// The currently open extension overlay (if any).
    active_extension_overlay: Option<ActiveExtensionOverlay>,
    selection: TerminalSelection,
    clipboard: MuxClipboard,
    renderer: AnsiRenderer,
    size: (u16, u16),
    /// Persistent compositing surface, reused across frames so the
    /// `StringInterner` isn't rebuilt and text re-interned every frame, and
    /// so dirty tracking is relative to the previous frame's content.
    surface: CellSurface,
    /// Resolved mux palette. Derived from the host terminal's OSC colors at
    /// startup; falls back to the built-in Charmtone palette.
    palette: ScenePalette,
    /// The host terminal's OSC-reported colors (or the default fallback).
    /// Stored so `driver::spawn` can pass them to each embedded session,
    /// so the session TUI derives its palette from the same terminal colors
    /// as the mux chrome.
    terminal_colors: phi_tui::TerminalColors,
    /// Visual extension runtime: holds extension-registered visual specs
    /// (themes, spinners, thinking expressions, text effects) so the TUI
    /// rendering code can query them during the draw loop.
    visual_runtime: Option<Arc<AppRuntime>>,
    /// Whether the focused session's screen was in the alternate screen last
    /// frame; a transition drops scroll history and forces a full repaint.
    alt_screen_active: bool,
    /// Cached total scrollback lines for the focused session. Recomputed only
    /// when a scrollback-affecting event occurs (session switch, new content),
    /// not every frame — avoiding the set_scrollback(MAX)/restore dance that
    /// mutates shared vt100 state on every render.
    cached_scrollback_total: usize,
    sidebar_scroll: usize,
    note: Option<(String, StatusNoteKind, Instant)>,
    /// Universal animation clock — single source of truth for all
    /// animation timing (sidebar shimmer, statusbar shimmer, spinner
    /// glyphs, gradient text). Replaces the old `ANIMATION_TICK` constant
    /// and ad-hoc `SystemTime::now()` calls.
    clock: AnimationClock,
    channels: MuxChannels,
    session_watcher: SessionWatcher,
    quit: bool,
    render_failures: u32,
    renderer_invalidated: bool,
}

/// An extension-registered overlay that is currently open (active).
///
/// Holds a reference to the overlay spec (for render + handle_key closures)
/// and the type-erased state created by the spec's `init_state` factory.
#[allow(dead_code)]
pub(crate) struct ActiveExtensionOverlay {
    pub(crate) name: String,
    pub(crate) spec: OverlaySpec,
    pub(crate) state: std::boxed::Box<dyn std::any::Any + Send + Sync>,
}

fn install_panic_log_hook() {
    std::panic::set_hook(Box::new(|info| {
        let Some(dirs) = directories::ProjectDirs::from("", "", "phi") else {
            return;
        };
        let path = dirs.data_dir().join("mux-panic.log");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(file, "[{:?}] panic: {info}", std::time::SystemTime::now());
            let _ = writeln!(file, "{}", std::backtrace::Backtrace::force_capture());
        }
    }));
}

/// Build a lightweight extension runtime for visual discovery.
///
/// Discovers and loads Lua extensions from the project's `.phi/` directory
/// and global config, then registers them. The resulting runtime holds
/// any extension-registered visual specs (themes, spinners, thinking
/// expressions, text effects) that the mux TUI can query.
///
/// This is a best-effort function: if extension discovery or registration
/// fails, it returns `None` and the mux falls back to its built-in visual
/// defaults.
fn build_visual_runtime(cwd: &std::path::Path) -> Option<Arc<AppRuntime>> {
    let ext_paths = phi_lua::discover_extensions(cwd);
    let lua_exts = phi_lua::load_extensions(&ext_paths);

    let config_ext_paths = phi_lua::config_extension_paths(cwd);
    let config_exts = phi_lua::load_extensions(&config_ext_paths);

    let mut rt_builder = phi_ext::RuntimeBuilder::new().with_cwd(cwd);
    // Register the built-in default visual extension first so the
    // default spinner, thinking expressions, and theme are always
    // available. Lua extensions loaded afterward can register
    // additional themes/spinners with different names.
    rt_builder = rt_builder.register_extension(phi_builtins::DefaultVisualExtension);
    rt_builder = rt_builder.register_extension(phi_builtins::DefaultOverlayExtension);
    for ext in config_exts {
        rt_builder = rt_builder.register_boxed_extension(ext);
    }
    for ext in lua_exts {
        rt_builder = rt_builder.register_boxed_extension(ext);
    }

    match rt_builder.build() {
        Ok(rt) => Some(Arc::new(rt)),
        Err(e) => {
            eprintln!("Warning: visual extension discovery failed: {e}");
            None
        }
    }
}
