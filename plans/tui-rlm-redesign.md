# Plan: RLM activity rendering — anchor-once + depth-gutter cards

## Goal
Replace the per-tool four-wall box idiom with a lighter **single-header card +
non-stealing depth gutter**, and render each call's canonical identity (path /
command / pattern / `model ← "prompt"`) exactly **once**. Addresses the two
named complaints: "box design doesn't represent the RLM" and "a lot of path
duplication".

## Why this scope (and what's explicitly out)
A workflow of 4 map agents + 4 adversarially-judged designs landed on these
hard constraints:

1. **Scrollback is immutable** (`transcript/render.rs:160-167`,
   `terminal.rs:208-211`, vt100 buffer irreversible). Any "expand a committed
   row inline" is impossible — `replace_block_rows` (`tui.rs:3361`) only
   mutates the live `self.block`. So the **timeline** (`Vec<TimelineStep>` +
   `step.expanded` on committed rows) and **two-pane** (always-live detail)
   paradigms regress history visibility. Rejected.
2. **`strip_tool_echoes` is data-level**, not layout-level — `print(grep(...))`
   literally puts result lines in stdout. Keep it (re-scoped in docs), never
   delete via layout.
3. **`tool_title` (`tui.rs:530`) and `tool_activity_lines` (`tui.rs:672`)** are
   two ~60-line match arms over tool name producing near-identical output —
   the maintenance duplication. Collapse to one source of truth.
4. **Depth today steals text width**: `nested_text_width = repl_body − 2·depth − 4`
   (`tui.rs:3194`) — a depth-3 sub-RLM run loses 6 content columns. A fixed
   gutter prefix removes the extra `−4` box padding and stops the cascade.
5. **`engine/{rlm,config,helpers}.rs` is dead code** (`lib.rs` declares only
   `pub mod engine;`). All work targets `engine.rs`.

**In scope:** the **indent-tree** paradigm (judge scores rlm-rep 4, scan 4),
realized *through the existing block/live/commit model* (not a state-machine
rewrite): single-header cards in place of four-wall sectioned boxes, a uniform
depth-gutter prefix, and anchor-once dedup. Applied to **both** renderers
(unifies them on one idiom — a win on its own).

**Out of scope (deferred, documented):** full `Vec<TimelineStep>` rewrite;
two-pane; the optional engine change to add `call_id`/parent to
`SubcallStart`/`SubcallEnd` (`engine.rs:184/189`) so concurrent subcalls
(`max_concurrent_subcalls > 1`) place deterministically. Today correlation is
depth+order — correct for serial subcalls only; the gutter exposes this no
worse than boxes did.

## Target shape

```
─ repl · turn 1 ─────────────────────────────────────
  │ ─ ◷ read  src/parser.rs
  │ ─ ⠋ grep  "parse" in src/
  │     src/parser.rs:42  fn parse(input) -> Ast
  │     … 38 more
  │ ─ ◉ rlm ← "extract the parse loop"
  │   │ ─ [turn 1/30]
  │   │ ─ ◷ edit  src/parser.rs
  │   │ ─ ✓ bash  $ cargo check
  │   │     error[E0425]: cannot find value `p`
  │ ─ ✓ bash  $ cargo test
  │     running 12 tests … (9 more)
─ answer · 4 turns · 4.2s ───────────────────────────
```

Per call: one header rule (status glyph + anchor), indented output body folded
under it (tail-preview, `preview_tail_lines` / `preview_rows_with_lang`).
Depth = `│ ` guide columns (fixed, non-stealing beyond the gutter itself). No
`┌─┐│└─┘`, no `Activity`/`Output` separators at the tool level. The outer
`repl` panel and Assistant/You panels stay boxed (they own the live/commit
split and the prose containers).

## Phase 1 — anchor-once dedup (renderer-only, safe)

### Interactive — `crates/phi-session/src/tui.rs`
- Add `tool_identity(name, args) -> Identity { anchor: String, body: Vec<String> }`
  where `anchor` is the one-line form (`tool_title` today) and `body` is the
  multi-line content (`tool_activity_lines` today: full highlighted bash command,
  repl code; **empty for read/write/edit/grep/find/ls** — their identity IS the
  anchor). This is the single source of truth.
- `tool_title` + `tool_activity_lines` become thin wrappers over
  `tool_identity` (kept only if other callers need the one-line form); delete
  the duplicated match logic.
- `tool_result_rows` (`tui.rs:3564`): header = anchor; body = `body` lines
  (if any) followed by output preview. The path/pattern appears once, in the
  anchor. (Interactive already largely does this; the win is code dedup +
  guaranteeing in-flight label == committed anchor.)

### Headless — `crates/phi-app/src/{render,tool_view,main}.rs`
- Add `call_anchor(name, args) -> String` (plain) to `tool_view.rs` built on
  `tool_display_name` + `tool_title_suffix`, extended to cover **bash**
  (`$ <cmd first line>`). Single source for the start line + panel title.
- `print_events` `ToolStart` (`main.rs:357`): print `call_anchor` instead of
  `tool_args_preview` (kills the `key=value` dump that duplicates the panel).
- `render_tool_panel` (`render.rs:69`): add `residual_args(name, args)` that
  removes the canonical key(s) `call_anchor` consumed (`path` for read/write/
  edit/ls; `path`+`pattern` for grep/find; `cmd` for bash) before any args
  section is emitted. **Preserve the full multi-line bash command** — bash's
  body is the meaningful content, shown once in its section, not truncated to
  one line. Drop `tool_args_preview` from the panel.
- Keep `cli_strip_tool_echoes` (`main.rs:242`); re-document as data-level
  echo coalescing.

## Phase 2 — boxes → single-header cards + depth gutter (interactive)

`crates/phi-session/src/tui.rs`:
- Add `render_card(header, header_plain, max_width, color, depth, body) ->
  Vec<String>` next to `render_nested_panel` (`tui.rs:428`): one titled top
  rule (`nested_panel_border` reused) + body lines as
  `gutter_prefix(depth+1) + content`, **no side walls, no bottom border, no
  separators**. (Phase 3 may collapse the top rule to a bare `─ ` rule.)
- `gutter_prefix(depth) -> String` = `│ ` repeated `depth` times (replaces
  `nested_indent` `tui.rs:288` for cards; `nested_indent` stays for the outer
  boxed panels).
- Card content width = `repl_body_width − gutter_width(depth)` (no extra
  `−4`/`−2·depth` beyond the gutter). Update `nested_text_width` callers in
  the card path; the boxed-panel path keeps the old math.
- Convert call sites (replace `render_nested_sectioned_panel`/`render_nested_panel`
  with `render_card`):
  - `tool_result_rows` (`tui.rs:3564`) — header = `{glyph} {anchor}`, body = identity body + output.
  - `status_rows`/`push_status` (`tui.rs:3316/3327`) — in-flight card = single
    header row (`{spinner} {anchor}`); `OpenToolRow.len` becomes ~1 for the
    header (output appended on `ToolEnd`).
  - `repl_code_lines` (`tui.rs:3263`) — `{label}` header (● from forge_title) + folded code body.
  - `push_bar` (`tui.rs:3495`) — header rule + one body line.
  - `push_preview_with_lang` (`tui.rs:3546`) — fold body under the current
    card's gutter (no separate Output box).
  - `subcall_result_rows` (`tui.rs:3590`) — header = `subcall_title` (computed
    **once** at `SubcallStart`, stored on `OpenSubcallRow`, reused at End —
    drops the `subcall_title_plain` border-label recompute at `tui.rs:3928`).
- `commit_block` (`tui.rs:3472`): keep the outer `render_repl_panel` wrap;
  enrich its label to `repl · turn {N}` from the depth-0 `IterationStart`
  handler (`tui.rs:3665`).
- Keep the depth-0 live-discard+rebuild at `CodeExecEnd` (`tui.rs:3770`) — now
  cheap (re-deriving header+body cards through `push_repl_replay`'s same path).
- Keep `strip_tool_echoes` (`tui.rs:199`); re-document.

### Headless — keep the box, dedup only
Headless (`phi "prompt"`) is non-default and its tests assert box structure
(`┌───`, `├─── Command`, `│`). The box→card conversion is interactive-only to
bound risk. Headless gets the Phase-1 dedup only: `call_anchor` start line +
`residual_args` stripping the canonical key from the Args section (so
edit/write stop re-showing `path` that's already in the title suffix). The
box chrome and the bash `Command` section (full multi-line cmd, preserved)
stay as-is.

## Constraints honored
- Scrollback immutable → output stays inline + tail-folded while live and
  commits visibly (today's behaviour preserved). No expand-on-commit.
- `strip_tool_echoes` kept (data-level).
- Multi-line bash bodies preserved.
- Real structural cue for depth (gutter guides), not 2-space indent alone.

## Tests
- Rewrite `tool_result_merges_activity_and_output_sections` (`tui.rs:4265`),
  `repl_panel_splits_input_and_output_sections` (`tui.rs:4291`),
  `render_panel_clamps_to_available_width` (`tui.rs:4244`) to the card form.
- Keep `panel_row_*`/`panel_border_*`/`render_panel_*` tests (boxed panels
  still used by Assistant/You/repl outer).
- Keep `strip_tool_echoes_*` (`tui.rs:4320+`).
- Add `tool_identity_anchor_shown_once`, `residual_args_strips_canonical_key`,
  `bash_body_preserved_multiline`.
- **Headless tests stay box-form** (see "Phase 2 — Headless: keep the box,
  dedup only" — the governing instruction). Do NOT rewrite
  `render_tool_panel` assertions to the card form; only the Phase-1 dedup
  (`call_anchor` start line, `residual_args`, multi-line bash preserved)
  applies headless. The earlier "update `render_tool_panel` assertions … to
  the card form" note is superseded and removed.
- Build/test: `nix develop -c cargo test -p phi-session -p phi-app`.

## Rollout
Phase 1 → build+test → Phase 2 → build+test. Renderer-only; no `engine.rs`
change; no session/log format change (the ReplRecord/ToolCall session structs
are untouched — only how they render). Reversible per-phase.

## Resolution (implementation decisions, post-audit)
1. **Gutter guides implemented.** `gutter_prefix(depth) = "│ " * depth` (dim),
   replacing the two-space `nested_indent` for cards (Constraint: real guide,
   not 2-space indent alone). `gutter_width(depth) = depth*2` keeps card width
   math identical to the old two-space budget. `render_card` header + body are
   inset by the gutter. `nested_indent` was removed (it had no remaining
   callers once the boxed nested-panel cluster was deleted).
2. **Repl output stays a separate flat `output` card (not folded under the
   `Input` card).** Rationale: `push_preview` has no parent-card handle, the
   live/commit split (a hard-protected scrollback model) rejects an inline
   expand, and the `output`/`exec` cards are *flat* (no `┌─ Output ─┐` box) —
   so the hardened "no separate Output box" constraint is already met. The
   separate `output` card also mirrors the established depth>0 sub-RLM
   `CodeExecEnd` preview (`push_preview(depth, ...)`). Folding output under
   the input card is left as future work (would need an `OpenToolRow`-style
   card handle for the repl input).
3. **`repl · turn N` label** threaded via `render_repl_panel(.., turn:
   Option<usize>)`, `repl_panel_label(turn)`, from the depth-0
   `IterationStart` handler's `self.turn`. Applied to the **live** repl block
   (live view + `commit_block`) only while a run is active
   (`commit_block` gates the turn on `self.running.is_some()`); persisted
   assistant messages re-rendered as prose (restore, post-run) stay at a
   plain `repl` label — never the misleading `turn 0`.
4. **Dead code removed:** `nested_panel_border/_row/_bottom/_separator`,
   `fitted_sectioned_panel_width`, `render_nested_sectioned_panel`,
   `render_nested_panel`, `subcall_title_plain`, `nested_text_width`,
   `nested_indent`. Subcall title computed once at `SubcallStart` (stored on
   `OpenSubcallRow`), reused at `End` (already done; kept).
5. **`strip_tool_echoes` re-documented** as data-level (both renderers).
6. **Anchor-once hardened (adversarial audit found single-line dup).**
   `tool_identity` now keeps an empty body when the canonical content already
   fits the one-line anchor, so the path/command/value is shown once total:
   - **bash**: empty body for a single-line command (anchor carries it); full
     command body only when multi-line (anchor is the truncated `first…`).
   - **repl**: empty body for single-line code; highlighted body only when the
     code spans more than one line.
   - **unknown tools** (MCP/extension names): `fallback_body` is empty when
     every `key=value` fits the anchor; a multi-line/long/nested value earns a
     full pretty-print body row (anchor keeps the truncated one); a bare
     non-object value (never in the anchor) always lives in the body.
   `tool_identity_anchor_shown_once` and `bash_body_preserved_multiline` pin
   these (single-line bash count==1, multi-line bash body len==2, small vs.
   long unknown-tool args, bare-string value shown). In-flight and committed
   anchors share `tool_identity().anchor` so the spinner and committed card
   header never drift (no flicker).

