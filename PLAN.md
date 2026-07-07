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
  one `●` glyph for every status line, dimmed secondary text, tool output
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
      (TTY: streamed text + `●` tool lines on stderr, errors as
      `● ERROR:`; piped: final text only) + `settings.json` (`loop`,
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
      (model · tokens · cost), `●` tool/error lines. Esc interrupts;
      Enter mid-run steers (leftover steering restarts a run); Ctrl+C
      clears/quits; Ctrl+D quits; resize blanks + replays the transcript.
      Frontend is a pack picked by `settings.json` `frontend` (only
      "inline" exists). 12 state-machine tests + pty smoke test (raw-mode
      enter/exit, editor + status render, clean quit + usage line).
      *Pending live acceptance (streamed turn with a real model): still
      blocked on provider credit, same as WS2.4.*
- [x] **3.2 Transcript vocabulary (forgecode-minimal).** Landed as
      `phi-app/src/transcript.rs`: every status line both frontends print
      is built there — `● name summary` (dim; edit shows `path +A -R`
      computed from the arguments, write `path +N`), `● ERROR:` first
      line red, `--verbose`/`-v` shows tool results dim-indented
      (head-capped at 10 lines), interrupted/retry lines. Two SGR
      attributes total (dim, red). 15 golden-string tests + a chat-state
      verbose test.
- [ ] **3.3 Slash commands from the registry.** `/name args` dispatches
      via `host.invoke_command`; result `{ message }` prints as a `●`
      line; frontend dispatches on returned `action` string keys (`quit`,
      `clear`), never a closed enum. Built-ins (`/quit`, `/clear`,
      `/model`?) ship as embedded Lua through the public API.
      *Accept: `/hello` from hello.lua works in-session; `/quit` quits.*

## WS4 — Sessions (usability, then parity)

- [ ] **4.1 JSONL session log.** Append-only, pi-compatible spirit
      (header line + entries), stored under the phi data dir keyed by cwd;
      every run writes one. `--continue` resumes the most recent for cwd.
      *Accept: kill a session, `phi --continue`, the model remembers.*
- [ ] **4.2 `/resume` picker + `--resume`.** Selector over session files.
- [ ] **4.3 Compaction.** Only when overflow actually bites; pi's
      reserve/keep numbers as defaults; overridable via a
      `session_before_compact` handler.

## WS5+ — per DESIGN.md

Examples-as-conformance (≥15 ports), RLM pack, mux pack — unchanged.

## Deliberately not now

- Parallel tool execution (pi's default; sequential is correct, just
  slower — revisit when it hurts).
- Images in `read` (needs content-block support in `phi-ai-types` first).
- grep/find/ls native tools, MCP, skills, themes, RPC mode, model cycling,
  retry knobs beyond phi-ai's built-ins, project-local settings.
- Anything the transcript vocabulary above doesn't need to render.
