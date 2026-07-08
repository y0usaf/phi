# phi — execution ladder

DESIGN.md is the spine (vision, doctrines, locked decisions); this file is
the ordered ladder of work. Each step has an acceptance check. Usability
first: a session you'd actually reach for before any nook-and-cranny parity.
Detail beyond this file lives in commits, not here — if this document grows
past ~150 lines, it's absorbing what commits should say.

**Parity calibration** (both vendored under `ref/`):

- **pi** sets the *behavioral* bar: a flat tool-call loop (~750 lines for
  the whole loop core), 4 default tools (read/bash/edit/write), steering at
  turn boundaries, errors as error tool-results (the loop never crashes),
  a ~30-line system prompt, print mode when stdin/stdout isn't a TTY.
- **forgecode** sets the *visual* bar: inline scrollback (no alt screen),
  one `•` glyph for every status line, dimmed secondary text, tool output
  suppressed unless it failed, braille spinner, tiny default-16-color
  palette, no boxes or borders anywhere.

## Done

- [x] **WS0 — Strip.** Workspace is the keep set; bare core boots
      (`--login`, `--list-models`, one-shot streamed completion).
- [x] **WS1 — Lua host.** `phi-host`: VM on a message thread, per-dispatch
      watchdog (5000 ms default), `register_command` / `register_tool`
      (native and Lua share one registry) / `on(event)`, discovery
      (`~/.config/phi/extensions`, `.phi/extensions`), trust store + prompt
      in phi-app. 13 integration tests cover the acceptance list.

## WS2 — The loop as a pack

- [x] **2.1 `phi-agent`: flat loop core.** Landed as
      `phi_agent::run_flat`: stream → sequential tool calls → repeat;
      steering drained at turn boundaries; provider errors/aborts land in
      the transcript, never panic; Rust event stream + subscriber-gated
      `host.emit` mirror; blockable `tool_call`. 8 scripted-stream tests;
      exercisers `examples/permission-gate.lua` + `examples/trace.lua`.
- [x] **2.2 Native tools: read, bash, edit, write.** Landed via the public
      `Host::register_tool` (`examples/tool.lua` uses the same door from
      Lua). Shared truncation (2000 lines / 50 KB, spill-to-temp for bash),
      bash in its own process group, exact-match edits, write creates
      parents. 28 unit tests incl. truncation and edit-mismatch.
- [x] **2.3 System prompt.** Landed as `phi_agent::prompt::build`:
      preamble + tool list + guidelines, `<project_context>` from
      AGENTS.md/CLAUDE.md (global + root→cwd walk, AGENTS.md wins per dir),
      date + cwd last; `--system` replaces the head, keeps the tail.
      Snapshot-style tests with fake project trees.
- [x] **2.4 One-shot drives the loop.** Landed as `phi-app/src/agent.rs`
      (TTY: streamed text + `•` tool lines on stderr, errors as
      `• ERROR:`; piped: final text only) + `settings.json` (`loop`,
      `model`, `thinking`; loop key selects the pack, only "flat" exists).
      First Ctrl+C cancels the run, second force-quits.
      *Pending live acceptance (edits a file + runs bash with a real
      model): blocked on provider credit — anthropic OAuth hit its monthly
      spend cap and the ChatGPT OAuth account rejects codex model ids.
      Error path verified live (clean `provider error` exit + usage line);
      loop mechanics covered by the scripted-stream tests.*

## WS3 — First frontend pack (inline TUI)

- [x] **3.1 Chat frame.** Landed as `phi-app/src/chat.rs` on `phi-tui`'s
      scrollback + live-region `Terminal`: `❯` editor (no borders),
      markdown committed to scrollback per block with a live streaming
      preview, braille spinner + dim elapsed, one dim status line
      (model · tokens · cost), `•` tool/error lines. Esc interrupts;
      Enter mid-run steers (leftover steering restarts a run); Ctrl+C
      clears/quits; Ctrl+D quits; resize blanks + replays the transcript.
      Frontend is a pack picked by `settings.json` `frontend` (only
      "inline" exists). 12 state-machine tests + pty smoke test (raw-mode
      enter/exit, editor + status render, clean quit + usage line).
      *Pending live acceptance (streamed turn with a real model): still
      blocked on provider credit, same as WS2.4.*
- [x] **3.2 Transcript vocabulary (forgecode-minimal).** Landed as
      `phi-app/src/transcript.rs`: every status line both frontends print
      is built there — `• name summary` (dim; edit shows `path +A -R`
      computed from the arguments, write `path +N`), `• ERROR:` first
      line red, `--verbose`/`-v` shows tool results dim-indented
      (head-capped at 10 lines), interrupted/retry lines. Two SGR
      attributes total (dim, red). 15 golden-string tests + a chat-state
      verbose test.
- [x] **3.3 Slash commands from the registry.** Landed as chat.rs slash
      dispatch (off the UI loop) + `phi-app/src/builtins/commands.lua` —
      `/help /quit /clear /model /login /logout` as embedded Lua through
      the public `register_command` door. `{ message }` prints as a dim
      `•` block; `action` string keys dispatch open-vocabulary (`quit`,
      `clear`, `set_model`, `login`, `logout`; unknown → error line).
      `/model <id>` swaps model+auth together once the provider's auth
      resolves; `/login` runs the OAuth flow in-session with progress as
      `•` notices (Esc cancels). New `phi.commands()` snapshot
      (exerciser: `examples/list-commands.lua`) powers `/help`. Accepted
      live in a pty: `/hello` from hello.lua (trusted project extension),
      `/model` switch reflected in the status line, `/quit` quits.
      *Notes: `/model` is session-only (no settings.json persistence);
      the in-session `/login` browser round-trip reuses `--login`'s flow
      but hasn't been driven end-to-end (needs a browser).*

## WS4 — Sessions (usability, then parity)

- [x] **4.1 JSONL session log.** Landed as `phi-session` (new crate):
      append-only JSONL — one `session` header line + one `message` entry per
      transcript `Message`, tagged `Line` so the format grows (compaction,
      branch nav) without breaking older readers. Files under
      `{data dir}/sessions/{--cwd--}/{ts}_{uuidv7}.jsonl`, mirroring pi's
      per-cwd layout. Tolerant reader skips malformed/torn lines; newer-than-
      understood versions are rejected. `phi-app/src/session.rs` wires it:
      every run (one-shot + chat) writes one; the loop owns the transcript,
      the store is an append-only sink flushed with the run's message delta;
      `--continue` resumes the most recent non-empty session for cwd,
      seeding `Context.messages` and reusing its file. 12 phi-session store
      tests + 3 phi-app session-wiring tests (continue round-trip, fresh
      fallback, empty-session skip). *Accept: kill a session, `phi --continue`,
      the model remembers.* *Pending live acceptance (a real two-turn session
      with a model): still blocked on provider credit, same as WS2.4/WS3.1.
      The headless round-trip (write → continue → see both turns) is covered
      by `session::tests::continue_seeds_and_appends_across_runs`.*
- [ ] **4.2 `/resume` picker + `--resume`.** Selector over session files.
- [ ] **4.3 Compaction.** Only when overflow actually bites; pi's
      reserve/keep numbers as defaults; overridable via a
      `session_before_compact` handler.

## WS5+ — per DESIGN.md

Examples-as-conformance (≥15 ports), RLM pack, mux pack — unchanged.

### `register_provider` — all providers arrive via the extension surface

- [x] **Provider registration API.** Landed `phi.register_provider(name,
      config)` in `phi-host` (Lua globals + `Host::providers()` query) and
      `extensions::graft_providers` in `phi-app`. Extensions register
      model providers; the host converts each `ProviderInfo` to
      `Vec<Model>` and grafts them onto the `Registry` via
      `Registry::extended`. An inline `api_key` becomes a runtime-only
      auth override (`AuthStorage::set_runtime_api_key`, never persisted).
      The grafted registry is used in every mode: `--list-models`,
      `--model`, `/model`, agent, and chat. Exerciser:
      `examples/custom-provider.lua` (OpenRouter pattern + Vercel AI
      Gateway pattern + proxy-override pattern).
- [x] **First-party catalog through the same door.** There is no embedded
      models.json: `Registry::builtin()` is protocol implementations only,
      and the whole first-party catalog (anthropic, openai, cursor,
      neuralwatt — cost, modalities, thinking_levels, adaptive_thinking,
      reasoning_control) is `phi-app/src/builtins/providers.lua`,
      registered via `phi.register_provider` like any user extension
      (The Rule). Provider re-registration replaces, so user extensions
      override first-party providers by name (exercised in tests).

## Deliberately not now

- Parallel tool execution (pi's default; sequential is correct, just
  slower — revisit when it hurts).
- Images in `read` (needs content-block support in `phi-ai-types` first).
- grep/find/ls native tools, MCP, skills, themes, RPC mode, model cycling,
  retry knobs beyond phi-ai's built-ins, project-local settings.
- Anything the transcript vocabulary above doesn't need to render.
