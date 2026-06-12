# phi Mux Mode — In-Process Session Multiplexer

## Context

phi currently runs one session per process (TUI mode or one-shot). The goal: `phi --mux` (or `mux.enabled = true` in Lua config) turns phi into a single-process session multiplexer with full feature parity to `/home/y0usaf/Dev/pi-harness` — sidebar of projects/sessions, status spinners, archive/usage/help overlays, chord keybinds, `:` command line, mouse selection/clipboard, persisted workspace state.

**Key architectural difference from pi-harness:** pi-harness spawns `pi` child processes in PTYs and learns status via a JS sidecar over a Unix socket. phi controls both sides, so every session runs **in-process** as a tokio task (single process for clean RAM accounting in btop; panic isolation via tokio's per-task unwind catching — `JoinError::is_panic()`; workspace has no `panic = "abort"`). pi-harness is Rust, so its UI layer ports nearly wholesale; only its backend is replaced.

**The embedding trick:** phi's `Terminal<W: Write>` (crates/phi-tui/src/terminal.rs:95) is already generic over its writer. Each embedded session's `Ui` writes its normal relative-cursor ANSI into an in-memory writer feeding a `vt100::Parser` (5000-line scrollback — same crate/approach pi-harness uses for PTY output). The mux composites the focused session's vt100 screen into the main pane. The session code barely changes; no PTYs, no sockets.

**Crate location decision:** mux lives in `crates/phi-app/src/mux/` (module tree), NOT a new crate — a `phi-mux` crate would need phi-app's internals (tui, session, config) while phi-app's binary needs phi-mux: circular dependency. tui.rs already lives in phi-app; mux belongs beside it.

## Shared vs per-session objects (verified)

- Shareable (already `Arc`): `AuthStorage` (fd-locked), `Registry` (read-only), `ProviderLm` root+sub, `PythonEnvFactory`. No global statics in `Rlm`/`RlmSession`; instances coexist freely.
- Per-session: `Config::load(project_cwd)` (per-dir `.phi/config.lua` already works), `RlmConfig` (`cwd` field → Python REPL chdir's itself; process cwd is never mutated), `SessionStore::new(cwd)`, `FileSearcher`.
- Session JSONL: `~/.local/share/phi/sessions/--flattened-cwd--/{uuid}.jsonl`, header line carries absolute `cwd` (used for archive restore).

## Phases

### Phase 0 — Dependencies
- `/home/y0usaf/Dev/phi/Cargo.toml` `[workspace.dependencies]`: add `vt100 = "0.16"`, `arboard = { version = "3", default-features = false, features = ["image-data", "wayland-data-control"] }` (versions match pi-harness). Use existing `jiff` for date bucketing (NOT chrono). Workspace `members = ["crates/*"]` — nothing to add there.
- `crates/phi-app/Cargo.toml`: add `vt100.workspace = true`, `arboard.workspace = true`.

### Phase 1 — phi-app refactors (existing code)

1.1 **Config** (`crates/phi-app/src/config.rs`): add `pub mux: Option<MuxConfig>` to `Config` (keep `deny_unknown_fields`; nested struct is compatible). New `MuxConfig { enabled: Option<bool>, keybinds: Option<HashMap<String,String>>, sidebar_width: Option<u16> }` with `#[serde(default, deny_unknown_fields)]`. Handle in `overlay()` macro (manual merge for the nested struct). Lua shape:
```lua
return { mux = { enabled = true, keybinds = { quit = "ctrl+q" } } }
```

1.2 **CLI** (`crates/phi-cli/src/lib.rs`): add `#[arg(long)] pub mux: bool` and `#[arg(long, value_name = "DIR")] pub project: Vec<String>` (extra project dirs; avoids clashing with the positional prompt arg).

1.3 **Extract `build_session()`** (`crates/phi-app/src/session_builder.rs`, new): pull per-session construction out of `main.rs run()` (line 61+): `Config::load(cwd)`, `RlmConfig` (cwd, `CODING_PRELUDE`, `coding_tools_section()`, prologue, `workspace_context(cwd)`), `SessionStore::new(cwd)`, `Rlm::new(...).with_events(tx)`. Define `SharedResources { storage, registry, root_lm, sub_lm, envs }`. `main.rs run()` becomes a caller.

1.4 **Terminal writer**: change `Terminal<W: Write>` usage in `Ui` (tui.rs:717, hardcoded `Terminal<std::io::Stdout>`) to `Terminal<Box<dyn Write + Send>>` (trait object — avoids threading a generic through the 2600-line `Ui` impl).

1.5 **Embeddable session loop** (`crates/phi-app/src/tui.rs`): extract from `run()` (lines 512–656) so the mux can drive it. New params struct:
```rust
pub struct SessionIo {
    pub event_rx: Option<UnboundedReceiver<crossterm::event::Event>>, // None => real EventStream
    pub dimensions: Option<(u16, u16)>,   // None => crossterm::terminal::size()
    pub embedded: bool,                   // skip RawModeGuard, OSC title, ctrl+z SIGTSTP
    pub suspend_tx: Option<UnboundedSender<SuspendRequest>>, // external $EDITOR protocol
    pub cwd: PathBuf,
}
pub struct SuspendRequest { pub text: String, pub response_tx: oneshot::Sender<Option<String>> }
```
- `Ui::new` takes `cwd: PathBuf` instead of `std::env::current_dir()` (tui.rs:860).
- Embedded mode: no `RawModeGuard::new()`, no OSC 0 title write (tui.rs:569–574), external-editor path (tui.rs:661–681) sends `SuspendRequest` and awaits response instead of `guard.with_suspended()`.
- Resize arrives as injected `Event::Resize` via `event_rx`.
- Existing single-session call site passes `SessionIo { embedded: false, .. }` — behavior unchanged.

1.6 **Usage in session JSONL** (`crates/phi-app/src/session.rs`): add `usage: Option<SessionUsage>` to `Entry` (`skip_serializing_if`), `SessionUsage { input, output, cache_read, cache_write, cost_total, model }`. Usage is available as `RlmCompletion.usage` (phi-rlm engine.rs:100), consumed in `handle_run_done()` (tui.rs:~2462) — write it on the assistant append there. Old sessions simply lack the field (backward compatible).

### Phase 2 — Mux backend (`crates/phi-app/src/mux/`)

2.1 **`mux/status.rs`**: `SessionStatus { Idle, Thinking, Outputting, Tool { name }, Crashed, Done }`.

2.2 **`mux/driver.rs`** — per-session glue:
```rust
pub struct SessionHandle {
    pub input_tx: UnboundedSender<Event>,
    pub parser: Arc<Mutex<vt100::Parser>>,     // 5000-line scrollback
    pub task: JoinHandle<()>,                  // Err(e) if e.is_panic() => Crashed
    pub suspend_rx-side: (held by mux)
}
```
- `ParserWriter(Arc<Mutex<vt100::Parser>>, UnboundedSender<()>)` implements `Write`: `process(buf)` + notify mux to recomposite. This is the `Box<dyn Write + Send>` handed to the session's `Terminal`.
- Forwarder task per session: tees `RlmEvent`s — forwards to the session Ui's channel AND maps depth-0 events to status sent into one shared mux channel `(SessionId, SessionStatus)`: `RunStart→Thinking`, `RootDelta/RootText→Outputting`, `ToolStart→Tool{name}`, `ToolEnd→Thinking`, `RunEnd→Idle`. `depth > 0` → ignore.

2.3 **`mux/workspace.rs`** — port of pi-harness `workspace.rs` semantics on phi data:
- `MuxProject { cwd, sessions: Vec<MuxSession> }`, `MuxSession { id, store_path, first_message, modified, status, draft, handle: Option<SessionHandle> }`.
- Discovery: `SessionStore::list(cwd)` (newest-first `SessionMeta`). Handles spawned lazily on first selection.
- Draft lifecycle (parity with pi-harness ephemeral drafts): `ctrl+n` creates draft (hidden? — pi-harness hides ephemeral drafts from sidebar until materialized; phi materialization = first `RunStart` status / file appears via rescan, since SessionStore creates files lazily). Reuse existing draft if present.
- `archive_session`: `fs::rename` JSONL → `~/.local/share/phi/sessions/ARCHIVE/{original-flattened-dir}--{uuid}.jsonl`; restore parses header `cwd` and moves back. `:cleanup` evicts archives >30 days.
- `remove_project`: abort session tasks, drop from workspace (files untouched).

2.4 **`mux/persisted.rs`** — port pi-harness `state/persisted.rs`: `{ projects, project_cache, selected_project, selected_session }` at `~/.local/share/phi/mux-state.json`; 250ms-debounced background writer thread, atomic tmp+rename, sync flush on quit.

### Phase 3 — Mux UI (ports from pi-harness, `crates/phi-app/src/mux/ui/`)

| New file | Ported from pi-harness | Changes |
|---|---|---|
| `ui/cell_surface.rs` | `src/app/cell_surface.rs` | local `Color{r,g,b}` type |
| `ui/ansi.rs` | `src/app/tui/ansi.rs` | wrap frames in `\x1b[?2026h/l` |
| `ui/layout.rs` | `src/app/layout.rs` | sidebar/terminal/statusline rects |
| `ui/sidebar.rs` | `src/app/sidebar.rs` + scene portion | status from `SessionStatus` not sidecar; keep braille spinner frames `⠋⠙⠹⠸⢸⢰⣰⣠⣤⣄⣆⡆⡇⠇⠏` (60ms), animated gradient, project rule headers, sticky anchor scroll, scrollbar |
| `ui/terminal_view.rs` | `src/app/terminal_view.rs` | iterate `vt100::Screen` cells → surface; hardware cursor from `screen.cursor_position()` unless `screen.hide_cursor()`; per-session wheel/shift+pgup scrollback via `set_scrollback(n)`, reset on switch |
| `ui/scene.rs` | `src/app/scene.rs` | statusline: mode label / project / session first-message / status glyphs `⋯ ⚙ ● × ◌ ▶ •` |
| `ui/overlays/archive.rs` | `overlays/archive.rs` | source = phi ARCHIVE scan; Enter restore, r reload, age column |
| `ui/overlays/usage.rs` | `overlays/usage.rs` + `pi/usage.rs` | scan `~/.local/share/phi/sessions/` (skip ARCHIVE) for assistant entries with `usage` field; day×model aggregation via jiff local dates, dedup `(timestamp, total)`, 7 columns, file cache by `(len, mtime)` |
| `ui/overlays/help.rs` | `overlays/help.rs` | phi action names + current bindings |
| `ui/command_line.rs` + `command.rs` | `tui/command_line.rs`, `tui/command.rs` | `:open :archive :cleanup :usage :refresh :reload :quit :help`; editing keys ctrl+a/e/u/w etc.; `::` literal colon to session |
| `keymap.rs` + `actions.rs` | `config/keymap.rs`, `config/actions.rs` | chord trie verbatim; `KeyStroke` wraps crossterm `KeyCode+KeyModifiers`; defaults: ctrl+left/right projects, ctrl+up/down sessions, ctrl+n new, ctrl+r refresh, ctrl+shift+r refresh-all, ctrl+delete archive, ctrl+shift+delete remove project, ctrl+shift+c copy, ctrl+v/shift+insert paste, ctrl+q quit, ctrl+g help; overrides from Lua `mux.keybinds` |
| `ui/mouse.rs` + `clipboard.rs` | `selection.rs` + backend clipboard code | drag-select over vt100 screen, auto-copy on release via arboard (Linux: Clipboard + Primary); paste → bracketed-paste into focused session `input_tx` |

### Phase 4 — Mux main loop (`crates/phi-app/src/mux/app.rs`)

`pub async fn run(initial_projects: Vec<PathBuf>, shared: SharedResources) -> Result<()>`:
- Owns the single `RawModeGuard`, crossterm `EventStream`, mouse capture enable, `CellSurface` + diff renderer to real stdout.
- `tokio::select!` over: input events; shared status channel; ParserWriter redraw notifications; `SuspendRequest` channel; 60ms animation tick.
- Input routing: overlay active → overlay; command line active → command line; else chord trie → `Triggered(action)` dispatch / `Pending` swallow / `NoMatch` → forward to focused session `input_tx` (Esc-interrupt and ctrl+c reach the session this way; ctrl+q quit and ctrl+g help are mux-level).
- Resize: resize vt100 parsers (focused pane dims), inject `Event::Resize` into sessions, rebuild surface.
- **Suspension protocol** ($EDITOR): pause rendering, `guard.with_suspended(|| external_editor(text))`, send result back on `response_tx`, full redraw.
- **Panic containment**: install custom panic hook writing to `~/.local/share/phi/mux-panic.log` (not stderr — keeps screen clean); on `JoinHandle` `Err(e) if e.is_panic()` mark session `Crashed` (`×` glyph), ctrl+r restarts it against the same JSONL.
- On quit: flush persisted state synchronously, abort session tasks (RlmSession shutdown for clean Python teardown), restore terminal.

### Phase 5 — Dispatch (`crates/phi-app/src/main.rs`)

In `run()` before the `tui_mode` check: `if args.mux || settings.mux` → require TTY, build `SharedResources`, projects = persisted-state projects ∪ cwd ∪ `--project` dirs, `mux::run(projects, shared).await`.

## Risks / notes

- **vt100 fidelity (top risk)**: phi's Terminal emits `\x1b[?2026h/l` (ignored by vt100 — harmless), `\x1b[?25l/h` (handled), relative moves `\x1b[NA/NB/NG`, `\x1b[0J`, `\x1b[2K`, `\r\n` scroll — all standard, vt100-supported. **Mitigate with a unit test first**: feed `Terminal<Vec<u8>>` render output into `vt100::Parser`, assert `screen.contents()`. Build this before the UI port.
- **Usage overlay** shows data only from sessions created after Phase 1.6 (old JSONL has no usage field). Acceptable.
- **Trait-object Terminal** adds one vtable call per write — irrelevant (writes are batched per frame).
- The single-session TUI path must remain behaviorally identical; it is the regression surface of Phase 1.

## Verification

1. `nix develop -c cargo build --workspace` and `nix develop -c cargo test --workspace` (plain cargo can't link — stale rustup ld wrapper).
2. New unit tests: vt100 round-trip of `Terminal` output (the de-risking test, written first); chord trie matching; persisted-state save/load; archive move/restore; usage scanner aggregation.
3. Regression: plain `phi` TUI mode unchanged after Phase 1 refactors.
4. Manual mux smoke: `phi --mux` (cwd project) → sidebar + embedded session renders and streams; ctrl+n new session; ctrl+up/down + ctrl+left/right nav; `phi --mux --project ~/a --project ~/b` multi-project; `:open <dir>`; ctrl+delete → `:archive` → restore; `:usage` after a completed turn; ctrl+g help; mouse select + ctrl+shift+c / ctrl+v; $EDITOR suspend/resume; deliberate dev-only panic in a session → `×` + ctrl+r restart, process survives; quit/restart restores state.
