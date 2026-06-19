# Flatten Phi's TUI: forgecode `●` headers + claude-code `⎿` nesting

## Context

Phi's TUI currently renders tool calls, RLM subcalls, status rows, previews, and
results as **boxed cards** (`─ header ───` rule line + `│ ` gutter-guided depth
indentation), and assistant/user/REPL content as **four-wall panels**
(`┌─ … ┐`). The user finds `ref/forgecode`'s UI appealing for its minimalism and
wants Phi to look similarly spare — flat `● [time] Title` header lines (forgecode's
`TitleFormat`, `ref/forgecode/crates/forge_main/src/title_display.rs:23`) — while
still expressing RLM nesting/recursion, which forgecode lacks. The chosen hybrid:
flattened `●` headers + claude-code's `⎿` indentation for nesting
(`~/Dev/claude-code/components/MessageResponse.tsx:22`, the dim `  ⎿  ` gutter, one
`⎿` per depth level). The user explicitly chose **"replace cards entirely"** — no
compatibility mode.

Confirmed compatible up front: Phi and forgecode are the same renderer class
(inline scrollback + live bottom region, **no alt screen**;
`crates/phi-tui/src/terminal.rs:1-3`), so this is a render-vocabulary change, not
an architecture change.

Outcome: tool/subcall/status/preview/result rows become `● [HH:MM:SS] title` +
`⎿`-indented body; the REPL block, assistant prose, and user echo flatten to the
same vocabulary; the `│ ` gutter and `─ ──`/`┌┐└┘` box primitives are removed. The
anchor-once tool-identity invariant and the live-region diff contract are
preserved.

## Decisions

- **Nesting model:** `nest_prefix(depth)` = dim `⎿ ` * depth (2 cols/level). This
  keeps `nest_width == gutter_width == depth*2`, so `card_body_width`
  (`tui.rs:3115`) and every `wrap_ansi(.., card_body_width(depth))` site stay
  *arithmetically identical* — zero re-wrapping risk, and the width-clamp test
  (`tui.rs:4156`) keeps holding. (Rejected: claude-code's literal `  ⎿  ` 5-col
  marker, which would shift wrap boundaries per depth and risk live-region
  flicker.)
- **Status in the `●` color, not a separate ✓/✗:** success `{GREEN}●`, failure
  `{RED}●`, tool `{TOOL_ACCENT}●`, RLM subcall `{SUBCALL_ACCENT}●`, info/thinking
  `{CYAN}`/`{DIM}●`. Drop the `✓`/`✗` glyph from `tool_result_rows` (`tui.rs:3479`)
  and `subcall_result_rows` (`tui.rs:3509`). In-flight rows keep the **animated
  spinner** (they redraw each 100ms tick and stay volatile; `●` is for *committed*
  headers).
- **Flatten REPL + prose + user too:** REPL block → `● [time] repl · turn N`
  header (turn only when live, per `tui.rs:3386`) + `⎿` code; assistant prose →
  `● [time] Assistant` header + plain `ASSISTANT_FG` markdown (no `⎿`); user echo
  → `● [time] You` + plain `USER_FG` body.
- **Keep the chrome editor box out of scope:** the live input editor at
  `chrome_lines` (`tui.rs:1777`) and its `┌─ φ ─┐`/`┌─ Input ─┐` are *input chrome*,
  not transcript cards, and `chrome_lines_editor_mode_has_single_scaled_input_box`
  (`tui.rs:4601`) asserts it. Leave it. Keep `fitted_panel_width` (`tui.rs:242`,
  used by the editor at 1833/1835).
- **Timestamps are live-only, omitted on replay.** `forge_title` takes
  `timestamp: Option<&str>`. Live commits pass `Some(now_hms())`; replayed/restored
  rows pass `None` (so re-rendered transcripts don't stamp false replay-time
  clocks — the live-region diff keys on exact visible-line equality,
  `transcript/diff.rs:23`, and replay renders via `replay_branch` `tui.rs:2543`).
  Faithful per-event replay timestamps would require adding a timestamp field to
  `session::ToolCall`/`ReplRecord` — noted as future work, out of scope here.
- **Time source:** use `jiff` (already a workspace dep, `session.rs:189`) for
  `now_hms()` — no new dependency. ⚠️ `chrono` is NOT a phi-session dep; do not
  reach for it.

## New primitives (in `crates/phi-session/src/tui.rs`, near the existing `gutter_*`/`render_card` block ~306-344)

```rust
// dim `⎿ ` repeated `depth` times (replaces gutter_prefix)
fn nest_prefix(depth: usize) -> String;          // {DIM}{"⎿ ".repeat(depth)}{RESET}
fn nest_width(depth: usize) -> usize;             // depth*2  (== old gutter_width)

// forgecode header: {nest_prefix(depth)}{colored ●}{RESET} [{HH:MM:SS}] {title}
// timestamp None omits the [time] segment. No rule/fill — kills the box-card look.
fn forge_title(depth: usize, dot_color: &str, timestamp: Option<&str>, title: &str) -> String;

// flat header line + ⎿-indented body. Single replacement for render_card.
// body must already be wrapped to card_body_width(depth) (math unchanged).
fn render_flat(dot_color:&str, timestamp:Option<&str>, title:&str,
               depth:usize, body:&[String]) -> Vec<String>;

fn now_hms() -> String;   // jiff::Zoned::now().strftime("%H:%M:%S")
```

`card_body_width` (`tui.rs:3115`) and `repl_body_width` (`tui.rs:3107`) keep their
arithmetic — only swap `gutter_width` → `nest_width`. Generalize the existing
`push_dot` (`tui.rs:3398`, already a `●`-style primitive used for `thinking` at
2516/3221) to `push_dot(depth, color, ts, title)`.

## Steps

1. **Primitives.** Add `nest_prefix`/`nest_width` (drop-in renames of
   `gutter_prefix`/`gutter_width`, `│ `→`⎿ `), `forge_title`, `render_flat`,
   `now_hms`. Update `card_body_width` to call `nest_width`. Delete
   `render_card`/`gutter_prefix`/`gutter_width` once callers migrate.
2. **Migrate `render_card` call sites → `render_flat`:**
   - `tool_lines` 3152 — header `forge_title(depth, TOOL_ACCENT, ts, title)` + dim output body.
   - `repl_code_lines` 3175 — header `forge_title(depth, SUBCALL_ACCENT, ts, \"Input\"/\"repl\")` (the `●` from forge_title is the recognizer) + `⎿` code.
   - `status_rows` 3226 — single flat line `{nest_prefix(depth)}{text}` (text is the spinner label; no body).
   - `push_bar` 3403 — `forge_title(depth, DIM, ts, &text)`.
   - `push_preview_with_lang` 3452 — `forge_title(depth, DIM, ts, \"output\")` + `⎿` body.
   - `tool_result_rows` 3468 — drop glyph; `forge_title(depth, if failed {RED} else {GREEN}, ts, &identity.anchor)` + body. Identity/anchor-once untouched (`tool_identity` 422).
   - `subcall_result_rows` 3501 — drop glyph; `forge_title(depth, if failed {RED} else {SUBCALL_ACCENT}, ts, label)` + content. (depth>0 → `⎿ ⎿ …` = recursion.)
3. **Flatten panels (D3):**
   - Replace `render_repl_panel` (296) with `render_repl_block(0, turn, body)` → `forge_title(0, PANEL_REPL, ts, &repl_label)` + body. Update sites 1706, 1737, 3388.
   - `assistant_card_lines` (3121) → `● [time] Assistant` header + plain `ASSISTANT_FG` markdown (no `⎿`).
   - `user_card_lines` (3136) → `● [time] You` + plain `USER_FG` body.
   - Delete `render_panel`/`panel_border`/`panel_row`/`panel_bottom`/`render_repl_panel`/`repl_panel_label`. **Keep `fitted_panel_width`** (editor 1833/1835).
4. **`push_dot` generalization** (3398) → `push_dot(depth, color, ts, title)`; update the two `thinking` call sites (2516, 3221).
5. **Thread `ts`:** live/commit callers pass `Some(&now_hms())`; replay/restored callers (`replay_branch` 2543, toggle paths) pass `None`.

## Tests to update (`crates/phi-session/src/tui.rs:4118+` and `transcript/tests.rs`)

Rewrite shape assertions to the new vocabulary (anchor/content asserts stay):
- `tool_result_merges_activity_and_output_sections` (4182): `─ ✓ read`→`●`, `│ `→`⎿ `.
- `repl_panel_splits_input_and_output_sections` (4226): keep `repl · turn 1`/`Input`, add `●`, drop `├`; call `render_repl_block`.
- `render_panel_clamps_to_available_width` (4156): call `render_flat`+`render_repl_block`, assert `visible_width(line) <= 10`.
- `repl_panel_label_carries_turn_only_for_live_blocks` (4403): `┌─ repl `→`● repl` (no-turn invariant stays).
- `render_panel_fits_box_to_rendered_content` (4142), `panel_row_does_not_paint_box_body_background` (4126), `panel_border_reopens_color_after_label_reset` (4133): deleted primitives — delete or convert to `render_flat`/`forge_title` tests.
- `transcript/tests.rs:67`: `"┌ Edit: foo.ts"` → `● … Edit` committed-header shape.
- Unchanged (content-only, no box asserts): `tool_identity_anchor_shown_once` (4258), `bash_body_preserved_multiline` (4348), `streamed_paragraphs_commit_as_one_assistant_message_card` (4695), `chrome_lines_editor_mode_has_single_scaled_input_box` (4601), `replay_branch_*` (4792/4865), `toggle_tools_replays...` (4898).
- **Add** a unit test asserting `display_cell_width("⎿ ") == 2` (lock `⎿`=width-1 against a unicode-width upgrade; `phi-tui/src/cells.rs`), plus `nest_width == old gutter_width` and `card_body_width` unchanged.

## Critical files

- `crates/phi-session/src/tui.rs` — primitives + all call sites + tests (the bulk).
- `crates/phi-session/src/transcript/{render,diff,tests}.rs` — confirm line-key stability; update tests.rs:67.
- `crates/phi-session/src/session.rs:189` — reuse `jiff` pattern for `now_hms`.
- `crates/phi-tui/src/cells.rs` — `display_cell_width` `⎿`-width test.
- `crates/phi-session/Cargo.toml` — no change (jiff already present).

## Risks

1. **Replay timestamp falsity** — handled by `timestamp: Option`; omit on replay. (Faithful replay stamps = future schema work.)
2. **`⎿` width** — U+23BF is ambiguous; `unicode-width` returns 1 and `display_cell_width` `.max(1)`. Safe; locked by the new unit test.
3. **`⎿` font fallback** in rare terminals (missing-glyph box). Cosmetic-only; the user explicitly chose claude-code's `⎿`. Document a min-terminal note.
4. **Anchor-once invariant** — only rendering of `identity.anchor`/`body` changes; `tool_identity` untouched; invariant tests pass as-is.
5. **Diff contract** — flattening changes line *content*, not `BlockSpec.key`/`commit_stable` flags; `common_prefix_length`/`rows_visibly_equal` (`transcript/diff.rs:23`) still work. In-flight blocks stay volatile.

## Verification

1. `nix develop -c cargo test -p phi-session` (plain cargo can't link — stale rustup ld wrapper per project memory). Expect ~8 assertion rewrites; rest green.
2. `nix develop -c cargo test -p phi-session transcript` — transcript shape tests.
3. Manual via the `/run` skill, drive a session that exercises: (a) a tool call (`read_file`/`bash`) → `● [time] read <path>` + `⎿` output; (b) a nested RLM subcall (depth-1) → `⎿ ⎿` recursion; (c) a ```repl block → `● [time] repl · turn N` + `⎿` code; (d) streamed assistant prose → flat `● … Assistant` + plain text, no box; (e) a user message → `● … You`. Confirm no `│`, no `─ …─` rule, no `┌/└` in transcript content (only the chrome editor box remains).
