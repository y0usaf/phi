//! The mux main loop: owns the real terminal, multiplexes input between the
//! sidebar, overlays, the `:` command line, and the focused embedded
//! session, and composites every frame from the session's vt100 screen.
use std::collections::BTreeMap;
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
use crate::grid::ansi::HardwareCursor;
use crate::grid::layout::{CellLayout, SIDEBAR_WIDTH_DEFAULT, compute_cell_layout};
use crate::grid::selection::{SelectionPoint, selected_text};
use crate::grid::theme;
use crate::keys::{AppAction, KeyStroke, Keymap, KeymapMatch};
use crate::persisted::{self as persisted, PersistedState};
use crate::remote::protocol::ServerMsg;
use crate::remote::server::{AttachInfo, ClientRegistry, RemoteEvent};
use crate::status::SessionStatus;
use crate::watcher::SessionWatcher;
use crate::workspace::{self as workspace, Focus, Workspace};
use phi_ext::{AppRuntime, OverlaySpec};
use phi_session::config::MuxConfig;
use phi_session_tui::session_builder::SharedResources;
use phi_session_tui::tui::{self, HostModalRequest};

const NOTE_TTL: Duration = Duration::from_secs(4);
const WHEEL_LINES: i32 = 3;
const ARCHIVE_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

mod commands;
mod input;
mod rendering;
mod session;
mod view;

use view::MuxView;

pub async fn run(shared: SharedResources, mux_config: MuxConfig, cwd: PathBuf) -> Result<()> {
    install_panic_log_hook();

    let projects = shared.projects.clone();
    let ws = build_workspace(&cwd, &projects);

    let guard = RawModeGuard::new().context("mux requires a tty")?;
    tui::restore_terminal_on_signals();
    let mut stdout = std::io::stdout();
    let _ = phi_tui::crossterm::execute!(stdout, EnableMouseCapture);
    // The mux owns the whole screen (unlike the inline single-session TUI).
    let _ = write!(stdout, "\x1b[2J\x1b[H");

    // Probe the host terminal's colors (OSC 10/11/4) once, inside raw mode.
    let terminal_colors = phi_tui::detect_terminal_colors(Duration::from_millis(150))
        .unwrap_or_else(theme::default_terminal_colors);
    let size = phi_tui::crossterm::terminal::size().unwrap_or((80, 24));

    // Local mode is "core + one view": the single view is the synthetic
    // client 0 that InputSource::Local tags its events with.
    let (mut app, receivers) = build_app(
        shared,
        &mux_config,
        &cwd,
        ws,
        terminal_colors,
        0,
        size,
        RenderSink::Stdout,
    );

    let result = app
        .main_loop(
            HostIo::Local(&guard),
            InputSource::Local(EventStream::new()),
            receivers,
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

/// Run the mux headless for the daemon: no tty, no raw mode — input arrives
/// as [`RemoteEvent`]s from client connections, frames go out through the
/// [`ClientRegistry`]. Terminal colors are seeded by the first client's
/// handshake (`seed_id` is that client's registry id — its `Attached` event
/// was consumed by the caller, so its view is created here).
pub async fn run_server(
    shared: SharedResources,
    mux_config: MuxConfig,
    cwd: PathBuf,
    seed_id: u64,
    seed: AttachInfo,
    registry: Arc<ClientRegistry>,
    events_rx: mpsc::UnboundedReceiver<RemoteEvent>,
) -> Result<()> {
    install_panic_log_hook();

    let mut projects = shared.projects.clone();
    projects.push(seed.cwd.display().to_string());
    let ws = build_workspace(&cwd, &projects);

    let terminal_colors = seed
        .terminal_colors
        .clone()
        .unwrap_or_else(theme::default_terminal_colors);

    let (mut app, receivers) = build_app(
        shared,
        &mux_config,
        &cwd,
        ws,
        terminal_colors,
        seed_id,
        (seed.cols, seed.rows),
        RenderSink::Registry(registry),
    );

    let result = app
        .main_loop(HostIo::Remote, InputSource::Remote(events_rx), receivers)
        .await;

    app.persist_now();
    PersistedState::flush();
    drop(app);
    result
}

/// Workspace discovery + filesystem watchers, shared by the local and
/// daemon entry points.
struct WorkspaceSetup {
    workspace: Workspace,
    /// The seed view's selection (from persisted state). Later-attaching
    /// clients start from the last active view's focus instead.
    focus: Focus,
    session_watcher: SessionWatcher,
    watcher_rx: mpsc::UnboundedReceiver<usize>,
}

fn build_workspace(cwd: &std::path::Path, extra_projects: &[String]) -> WorkspaceSetup {
    let mut persisted = PersistedState::load_default().unwrap_or_default();
    let mut project_dirs: Vec<PathBuf> = persisted.projects.iter().map(PathBuf::from).collect();
    let push_project = |dirs: &mut Vec<PathBuf>, dir: PathBuf| {
        let dir = PathBuf::from(persisted::normalize_path(&dir));
        if dir.is_dir() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };
    push_project(&mut project_dirs, cwd.to_path_buf());
    for dir in extra_projects {
        push_project(&mut project_dirs, PathBuf::from(dir));
    }
    project_dirs.retain(|dir| dir.is_dir());

    persisted.normalize();
    let mut workspace = Workspace::discover(project_dirs);

    let mut focus = Focus::default();
    if let Some(selected) = &persisted.selected_project {
        let selected = PathBuf::from(selected);
        if let Some(i) = workspace.projects.iter().position(|p| p.cwd == selected) {
            focus.project = i;
        }
    }

    workspace.ensure_project_session(focus.project);

    // Start filesystem watchers on each project's session directory so the
    // sidebar auto-refreshes when session files change (the filesystem is
    // the source of truth; the sidebar is a view over it).
    let (mut session_watcher, watcher_rx) = SessionWatcher::new();
    for (i, project) in workspace.projects.iter().enumerate() {
        session_watcher.add_watch(i, &project.cwd);
    }

    WorkspaceSetup {
        workspace,
        focus,
        session_watcher,
        watcher_rx,
    }
}

/// The receiver half of every channel the main loop selects over.
pub(crate) struct MuxReceivers {
    pub(crate) status_rx: mpsc::UnboundedReceiver<(SessionKey, SessionStatus)>,
    pub(crate) redraw_rx: mpsc::UnboundedReceiver<()>,
    pub(crate) suspend_rx: mpsc::UnboundedReceiver<(SessionKey, tui::SuspendRequest)>,
    pub(crate) host_modal_rx: mpsc::UnboundedReceiver<(SessionKey, HostModalRequest)>,
    pub(crate) watcher_rx: mpsc::UnboundedReceiver<usize>,
}

#[allow(clippy::too_many_arguments)]
fn build_app(
    shared: SharedResources,
    mux_config: &MuxConfig,
    cwd: &std::path::Path,
    ws: WorkspaceSetup,
    terminal_colors: phi_tui::TerminalColors,
    seed_client: u64,
    size: (u16, u16),
    sink: RenderSink,
) -> (MuxApp, MuxReceivers) {
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
    let visual_runtime = build_visual_runtime(cwd);

    // Derive the mux palette: if an extension registered a theme, use it
    // (overlaid on the built-in derivation so missing roles fall back),
    // via the active style pack when one is set. Otherwise, use the
    // built-in `generate_theme` path — the same gray ramp / ANSI mapping
    // as before.
    let palette = if let Some(rt) = &visual_runtime {
        if let Some(theme_spec) = rt.active_theme() {
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

    let app = MuxApp {
        shared,
        workspace: ws.workspace,
        views: BTreeMap::from([(seed_client, MuxView::new(ws.focus, size))]),
        keymap: Keymap::from_overrides(mux_config.keybinds.as_ref().unwrap_or(&Default::default())),
        sidebar_width: mux_config.sidebar_width.unwrap_or(SIDEBAR_WIDTH_DEFAULT),
        extension_overlays: visual_runtime
            .as_ref()
            .map(|rt| rt.all_overlays().cloned().collect())
            .unwrap_or_default(),
        clipboard: MuxClipboard::default(),
        palette,
        terminal_colors,
        visual_runtime,
        clock: AnimationClock::with_baseline_keep_alive(),
        channels: MuxChannels {
            status_tx,
            redraw_tx,
            suspend_tx,
            host_modal_tx,
        },
        session_watcher: ws.session_watcher,
        sink,
        last_active_client: seed_client,
        pending_edits: Vec::new(),
        pending_size_recompute: None,
        quit: false,
    };
    let receivers = MuxReceivers {
        status_rx,
        redraw_rx,
        suspend_rx,
        host_modal_rx,
        watcher_rx: ws.watcher_rx,
    };
    (app, receivers)
}

/// Where rendered frames go: the local tty, or the attached-client fanout.
pub(crate) enum RenderSink {
    Stdout,
    Registry(Arc<ClientRegistry>),
}

/// Local-mode resources the main loop needs a handle to (the raw-mode
/// guard for external-editor suspension). The daemon has none.
pub(crate) enum HostIo<'a> {
    Local(&'a RawModeGuard),
    Remote,
}

/// Where input events come from: the local crossterm stream, or the fan-in
/// channel fed by client connections. Local events are wrapped as
/// [`RemoteEvent::Input`] from a synthetic client 0 so the main loop has a
/// single event shape.
pub(crate) enum InputSource {
    Local(EventStream),
    Remote(mpsc::UnboundedReceiver<RemoteEvent>),
}

impl InputSource {
    /// `None` means the source is gone (stdin closed / channel dropped) and
    /// the main loop should exit.
    pub(crate) async fn next(&mut self) -> Option<RemoteEvent> {
        match self {
            InputSource::Local(events) => {
                use futures::StreamExt as _;
                match events.next().await {
                    Some(Ok(event)) => Some(RemoteEvent::Input { id: 0, event }),
                    Some(Err(_)) | None => None,
                }
            }
            InputSource::Remote(rx) => rx.recv().await,
        }
    }
}

/// The mux core plus one view per attached client.
///
/// Core state — the workspace, session handles, watchers, channels,
/// persistence, clock — is shared by every attached client. Per-client
/// presentation state lives in [`MuxView`]: each client navigates, sizes,
/// and scrolls independently while session content stays canonical.
struct MuxApp {
    shared: SharedResources,
    workspace: Workspace,
    /// Per-client view state, keyed by client id (0 = the synthetic local
    /// client). Views are created on attach and removed on detach.
    views: BTreeMap<u64, MuxView>,
    keymap: Keymap,
    sidebar_width: u16,
    /// Extension-registered overlay specs, cloned from the visual runtime.
    extension_overlays: Vec<OverlaySpec>,
    clipboard: MuxClipboard,
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
    /// Universal animation clock — single source of truth for all
    /// animation timing (sidebar shimmer, statusbar shimmer, spinner
    /// glyphs, gradient text). Replaces the old `ANIMATION_TICK` constant
    /// and ad-hoc `SystemTime::now()` calls.
    clock: AnimationClock,
    channels: MuxChannels,
    session_watcher: SessionWatcher,
    /// Frame destination: local stdout, or the daemon's client fanout.
    sink: RenderSink,
    /// The client whose input most recently drove the mux — the target for
    /// client-side round-trips (external editor, clipboard read). Always 0
    /// in local mode.
    last_active_client: u64,
    /// External-editor round-trips awaiting an `EditResult` from a client,
    /// keyed by client id. Failed with an error if that client detaches.
    pending_edits: Vec<(
        u64,
        tokio::sync::oneshot::Sender<std::result::Result<Option<String>, String>>,
    )>,
    /// Debounce deadline for [`MuxApp::recompute_session_sizes`]: focus
    /// changes, attaches/detaches, and client resizes all schedule a
    /// recompute here instead of resizing ptys inline, so flipping through
    /// the sidebar doesn't storm sessions with resizes.
    pending_size_recompute: Option<tokio::time::Instant>,
    quit: bool,
}

impl MuxApp {
    /// The client registry when running as a daemon, `None` in local mode.
    pub(crate) fn registry(&self) -> Option<&Arc<ClientRegistry>> {
        match &self.sink {
            RenderSink::Registry(registry) => Some(registry),
            RenderSink::Stdout => None,
        }
    }
}

/// An extension-registered overlay that is currently open (active).
///
/// Holds a reference to the overlay spec (for render + handle_key closures)
/// and the type-erased state created by the spec's `init_state` factory.
pub(crate) struct ActiveExtensionOverlay {
    pub(crate) name: String,
    pub(crate) spec: OverlaySpec,
    pub(crate) state: phi_ext::OverlayState,
}

/// Panic hook: restore the terminal to cooked mode first (the unwind may
/// never reach the guard's `Drop`, and raw mode garbles any output), then
/// log with a backtrace and print to the now-readable stderr.
fn install_panic_log_hook() {
    std::panic::set_hook(Box::new(|info| {
        phi_tui::emergency_restore();
        let log_path = directories::ProjectDirs::from("", "", "phi")
            .map(|dirs| dirs.data_dir().join("mux-panic.log"));
        if let Some(path) = &log_path
            && let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
        {
            let _ = writeln!(file, "[{:?}] panic: {info}", std::time::SystemTime::now());
            let _ = writeln!(file, "{}", std::backtrace::Backtrace::force_capture());
        }
        eprintln!("phi mux panicked: {info}");
        if let Some(path) = log_path {
            eprintln!("(panic log: {})", path.display());
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
    // Project-local Lua only loads from an explicitly trusted project; the
    // mux never prompts, so an undecided project stays gated until the user
    // runs /trust inside a session.
    let trusted = !phi_session::trust::has_project_content(cwd)
        || phi_session::trust::TrustStore::load_default().decision(cwd) == Some(true);
    let ext_paths = phi_lua::discover_extensions(cwd, trusted);
    let lua_exts = phi_lua::load_extensions(&ext_paths);

    let config_ext_paths = phi_lua::config_extension_paths(cwd, trusted);
    let config_exts = phi_lua::load_extensions(&config_ext_paths);

    let mut rt_builder = phi_ext::RuntimeBuilder::new().with_cwd(cwd);
    // Register the built-in default visual extension first so the
    // default spinner, thinking expressions, and theme are always
    // available. Lua extensions loaded afterward can register
    // additional themes/spinners with different names.
    // Without the `builtins` feature the bare mux shell boots with no
    // builtin extensions; overlays and visuals degrade gracefully (WS1.5).
    #[cfg(feature = "builtins")]
    {
        // Rust extensions that need native resources (phi_tui theme
        // derivation + text effects, phi_session usage/model/archive data).
        // The mux colon commands, visual data (spinner + thinking
        // expressions), and the about/help overlays ship as Lua defaults —
        // the same `.lua` a user would write.
        rt_builder = rt_builder.register_extension(phi_builtins::DefaultVisualExtension);
        rt_builder = rt_builder.register_extension(phi_builtins::DefaultOverlayExtension);
        // Visual data: spinner + thinking expressions (Lua default).
        let visual_data = phi_lua::LuaExtension::from_source(
            "phi-visual-data",
            phi_builtins::defaults::VISUAL_DATA_LUA.to_owned(),
        )
        .ok()?;
        rt_builder = rt_builder.register_extension(visual_data);
        // Mux colon commands (Lua default).
        let mux_cmds = phi_lua::LuaExtension::from_source(
            "phi-mux-commands",
            phi_builtins::defaults::MUX_COMMANDS_LUA.to_owned(),
        )
        .ok()?;
        rt_builder = rt_builder.register_extension(mux_cmds);
        // About and help overlays (Lua defaults — the tutorial overlays).
        let about_ext = phi_lua::LuaExtension::from_source(
            "phi-about-overlay",
            phi_builtins::defaults::ABOUT_OVERLAY_LUA.to_owned(),
        )
        .ok()?;
        rt_builder = rt_builder.register_extension(about_ext);
        let help_ext = phi_lua::LuaExtension::from_source(
            "phi-help-overlay",
            phi_builtins::defaults::HELP_OVERLAY_LUA.to_owned(),
        )
        .ok()?;
        rt_builder = rt_builder.register_extension(help_ext);
    }
    for ext in config_exts {
        rt_builder = rt_builder.register_boxed_extension(ext);
    }
    for ext in lua_exts {
        rt_builder = rt_builder.register_boxed_extension(ext);
    }

    match rt_builder.build() {
        Ok(rt) => {
            // Initial style pack (WS10): the persisted `/style` choice wins
            // over config.lua's `style`; a stale name degrades to base
            // visuals.
            let config_style = phi_session::config::Config::load(cwd)
                .ok()
                .and_then(|c| c.style);
            if let Some(style) = phi_session::style::initial_style(config_style) {
                if let Err(e) = rt.set_active_style(Some(&style)) {
                    eprintln!("Warning: {e:#}");
                }
            }
            Some(Arc::new(rt))
        }
        Err(e) => {
            eprintln!("Warning: visual extension discovery failed: {e}");
            None
        }
    }
}
