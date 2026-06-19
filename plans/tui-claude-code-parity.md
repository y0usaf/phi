# Plan: Pixel-Exact TUI + Renderer Rewrite to claude-code Parity

**Status:** Revised 2026-06-19 after an adversarial 5-axis review (codebase accuracy / completeness / Stage-A feasibility / boundary-freeze / consistency, findings re-verified against real code). Blocker + 10 majors + 18 nits applied. Pending re-review sign-off.
Grounded by two verified investigation workflows (phi render call-graph + Ink internals + boundary mutability).
**Scope:** Interactive renderer layer ONLY — new `crates/phi-layout` flexbox engine, migration of `crates/phi-session/src/tui.rs` onto it, themes, shared headless crate. **RLM, harness, and mux (phi-mux) internals are untouched.**
**Target:** `~/Dev/claude-code` — readable TypeScript/React+Ink source (NOT a bundle).
**Build:** `nix develop -c cargo build` / `nix develop -c cargo test` (plain cargo can't link — stale rustup ld wrapper).
**Baseline:** 357 tests green as of 2026-06-19.

> This document **replaces** the earlier "polish" version of this plan. The earlier stance — *"parity as behavior, NOT pixels; exact byte-equality impossible and not the goal"* — is **overturned** by locked decision #1 (pixel-exact rewrite).

---

## Goal

**Pixel-exact** rewrite of phi's interactive TUI renderer to claude-code parity:
- Build a **custom flexbox layout engine** over raw crossterm (mirroring Ink's `stringWidth` → Yoga measure/constraint/place → Screen → colorize/border/table pipeline — **NOT ratatui**).
- Migrate the `phi-session/tui.rs` render path onto it (ending with the manual `Vec<String>` line primitive and `Terminal::<W>`'s double-wrap gone).
- Plumb **all 6 claude-code themes + ThemePicker** without touching frozen boundary fields.
- Extract a shared headless `tool_identity` crate (`phi-toolview`).
- Apply parity polish: claude-orange spinner/verbs, **`●`/`⏺` (BLACK_CIRCLE)** tool-use headers + `✓`/`✗` result cards, borderless markdown cards with `formatToken` parity (incl. **proportional tables** + **OSC-8 links**), `-` bullets, `❯` input trough, dim footer (no permission slot), `ctrl-l`/`ctrl-t`, `j/k + ctrl-n/p` selectors.
  > **Glyph note:** claude-code renders the standard tool-use dot as `BLACK_CIRCLE` (`●`/`⏺`, platform-gated; `constants/figures.ts:4`, `ToolUseLoader.tsx:20`). `DIAMOND_OPEN '◇'`/`DIAMOND_FILLED '◆'` (`figures.ts:26`) belong only to the background-task / remote-session / ultrareview surfaces — **not** tool cards. This plan uses `●`/`⏺` for tool cards; `◇`/`◆` are out of scope for v1.
- End with a side-by-side byte-level parity audit.

**Inline default only (v1); alt-screen deferred** (matches Ink's non-fullscreen inline path: content-tall, no scrollbox, no DECSTBM).

---

## The 13 Locked Decisions

1. **Pixel-exact REWRITE** (not polish) — overturns the prior "behavior not pixels" stance.
2. **Custom flexbox engine over raw crossterm** (not ratatui) — proportional tables + char-level colorize; keep inline philosophy.
3. **Inline default only (v1)**; alt-screen deferred — matches Ink's non-fullscreen path (content-tall, incremental cell-diff + CR/LF append, no DECSTBM/scrollbox).
4. **All 6 themes** (dark, light, light-daltonized, dark-daltonized, light-ansi, dark-ansi) + **ThemePicker**.
5. **`ctrl-l` = model picker** (keep phi's binding). *(Intentional divergence from claude-code, where `ctrl-l` = app:redraw + `meta+p` = model — documented here so the keybinding asymmetry is explicit.)*
6. **`ctrl-t` = thinking toggle** (keep). *(Intentional divergence from claude-code, where `ctrl-t` = toggleTodos — phi has no todos concept.)*
7. **Tool verbs adopt claude orange** (drop phi gold `#FFD278`).
8. **Bullets use `-`** (not `•`).
9. **No statusLine hook** (chrome is static; no runtime injection point).
10. **No fast-mode slot.**
11. **Extract a shared headless crate** for `tool_identity` (single anchor-once source of truth for phi-session + phi-app).
12. **No mux tests** (mux untouched, no test additions against it).
13. **No permission footer slot.**

---

## Architectural correction — what is actually replaced vs. preserved

A key finding from the grounding investigation: the originally-proposed "replaceable" set is **too aggressive**. The transcript commit engine is the content-agnostic scrollback-stability backing the embedded mux; replacing it would break a frozen invariant.

**PRESERVED (contract unchanged):**
- The **transcript engine**: `BlockSpec` / `Frame` / `Transcript::render` / `derive_live_commit_state`, `transcript/render.rs` blank-line + `last_activity` + `durable_end` logic, and the constants `VOLATILE_REARM_FRAMES=30`, `STABLE_PREFIX_COMMIT_FRAMES=30`, `TAIL_VOLATILITY_ROWS=4`. Only the **source** of `BlockSpec.lines` changes (manual `format!` → flexbox rows); the type/contract stays.
- The **frozen boundary types** (full list below).
- `phi-tui`'s public re-export list (the mux links against it).

**REPLACED:**
- `Terminal::<W>`'s double-wrap render path (re-wrap to `self.width` + cursor re-locate) → an `InlineCommit` layer (Ink's inline log-update: incremental cell-diff + CR/LF append).
- The manual `Vec<String>` line primitive — `panel_*`, `render_card`, `gutter_*`, manual width-budgeting → flex layout.

This achieves pixel-exact **layout** without touching the commit engine.

---

## Frozen boundary (verified; every stage must preserve this)

| Type | Location | Fields/shape |
|---|---|---|
| `TuiSetup` | `tui.rs:742` | `rlm`, `payload`, `events`, `missing_credentials`, `runtime`, `ui` — unchanged |
| `UiSetup` | `tui.rs:752` | `max_iterations`, `storage`, `registry`, `root_lm`, `sub_lm`, `store` — unchanged |
| `SessionIo` | `tui.rs:785` | `writer`, `event_rx`, `dimensions`, `embedded`, `suspend_tx`, `host_modal_tx`, `cwd` — unchanged + crossterm-`Event` forwarding |
| RLM engine types | `phi-rlm/src/engine.rs:126` | `RlmEvent` variants/fields + `Rlm`/`RlmCompletion`/`StopCause`/`ReplResult`/`RestoredExchange`/`RestoredRepl` — unchanged |
| Host-modal types | `phi-session/src/tui.rs` | `SuspendRequest`/`HostModalRequest`/`HostModalKind` oneshot protocols (these are tui.rs types, **not** `phi-rlm` engine.rs types) — unchanged |
| `phi-tui` re-exports | `phi-tui/src/lib.rs` | the full list the mux imports (see mux-byte-stable below) — unchanged |

**Theme reaches the Ui via `PHI_THEME` env + in-memory `current_theme`, NOT a new `TuiSetup`/`UiSetup`/`SessionIo` field.** `Config` (the config loader, **not** a frozen type) gains `theme: Option<String>` — sanctioned.

### Mux byte-stable surface (the untouched mux links against exactly this)
1. `phi-tui` re-exports the mux imports: **`crossterm`** (driver.rs/app.rs/keymap.rs/overlays), **`RawModeGuard`** (app.rs), **`display_cell_width` + `truncate_to_cells`** (consumed by the mux at `crates/phi-mux/src/ui/cell_surface.rs:325` as a live `pub use phi_tui::{display_cell_width, truncate_to_cells}`, used pervasively in `scene.rs`, `command_line.rs`, overlays), **`fuzzy_filter`** (overlays/model.rs).
2. `SessionIo` field layout + crossterm-`Event` forwarding.
3. The transcript `BlockSpec`/`Frame`/`Transcript::render` contract (reached in embedded mode).

The mux does **NOT** import `Terminal::<W>`/`wrap_ansi`/`wrap_ansi_locate`/`visible_width`/`fill_bg`/`strip_ansi` (grep of `crates/phi-mux` is empty for those), so replacing `Terminal::render`'s double-wrap and the manual line primitive is **free at the mux boundary**.

> **CRITICAL string-width divergence:** the mux's `display_cell_width` (`cells.rs`) is a naive per-`char` `unicode-width` sum with `.max(1)` flooring and **no grapheme clustering**; it diverges from `Bun.stringWidth`'s grapheme+wcwidth model that `phi-layout` uses for parity. Therefore `phi-layout` ships its **own** parity-grade `string_width` and the existing `cells.rs` **stays byte-stable for the mux**; unifying them is deferred until a byte-equality audit proves `phi-layout::string_width` matches the mux's `display_cell_width` for every mux input (otherwise overlay column widths shift and break the mux). `MuxConfig` is **not** given a theme field in v1.

---

## Verified current render call-graph (what the flexbox absorbs)

| Site | Line | Today emits | Flexbox absorbs |
|---|---|---|---|
| `panel_border/row/bottom` | `tui.rs:221/228/235` | border rule + padded body row | border+content-box flex item |
| `fitted_panel_width` | `tui.rs:242` | measured auto-width budget | measured flex auto-width |
| `render_panel` / `render_repl_panel` | `tui.rs:252/296` | bordered panel | bordered flex item |
| `gutter_prefix/width` | `tui.rs:306/312` | `│ `×depth indent | nested-flex left padding |
| `render_card` | `tui.rs:324` | header rule + indented body | nested depth-padding flex |
| `assistant/user_card_lines` | `tui.rs:3121/3136` | `render_markdown` + panel | markdown-children flex item |
| `tool_lines/repl_code_lines` | `tui.rs:3152/3175` | wrap+truncate+card | measured-wrap flex |
| `tool_result_rows/subcall_result_rows` | `tui.rs:3468/3501` | identity+glyph+preview | glyph+label+preview flex |
| `render_popup` | `tui.rs:944` | aligned-column grid | fixed-width-cell flex row |
| `Selector::render` | `selector.rs:178` | peer row system | fold into flex (leaf producer) |
| `chrome_lines` / `draw` | `tui.rs:1752/1625` | chrome + single `Terminal::render` | vertical stack: transcript flex + bottom chrome flex |
| `Terminal::render` | `terminal.rs:147` | double-wrap + line-diff + cursor park | `InlineCommit` (incremental diff, no double-wrap) |

---

## Stages

> One document, staged. Each stage ends with `nix develop -c cargo build` **and** `nix develop -c cargo test` green (plain `cargo` can't link in this repo). **Stage D is independent of B and parallelizable.** Stage C's *palette port + `Config.theme` plumbing + `theme_snapshot.rs`* is independent of B and can start in parallel, **but Stage C's `ThemePicker` overlay reuses Stage B's canonical `Paginated` selector** (see below), so the `ThemePicker` selection UX lands after B creates `Paginated` (or is built against a local stub selector during C, then re-pointed at B's `Paginated`). Net: A first; B on A; D parallel with B/C; the C↔B selector coupling is the one ordering constraint.

### Stage A — `phi-layout`: custom flexbox engine (bottom-up, no phi-session touch)

**Goal:** A new lower render-layout crate mirroring Ink's pipeline (`stringWidth` → measure/constraint/place → Screen → colorize/border/table/wrap-ansi → styled-row emission) over raw crossterm, with byte-grade unit tests. Depends on neither `phi-tui` nor `phi-session`; built entirely bottom-up.

> **Parity target — claude-code's TS yoga port, NOT canonical Yoga.** claude-code does **not** run Meta's C++ Yoga or its WASM build. It ships its own self-described *"Pure-TypeScript port of yoga-layout… simplified single-pass flexbox implementation that covers the subset of features Ink actually uses"* at `native-ts/yoga-layout/index.ts`. **Not implemented by the port** (so NOT in scope for `phi-layout` either): aspect-ratio, box-sizing content-box, RTL (Ink always passes `Direction.LTR`). This is good news — the byte-parity target is a bounded, enumerated surface, not the full Yoga errata. All "Yoga-fixture" language below means **fixtures resolved by this TS port**, captured as `(nodeTree → getComputedLeft/Top/Width/Height)` golden vectors.

**Steps**
- Create `crates/phi-layout`; add to workspace (members already glob `crates/*`). Deps: crossterm (output), `unicode-width`, `unicode-segmentation`, `regex-lite` (ANSI tokenize), `emoji-regex`-equivalent (an emoji grapheme regex crate, e.g. via a vendored table or `unicode-segmentation` + a supplemental emoji range check). No `phi-tui`.
- **`string_width`** — reproduce `Bun.stringWidth({ambiguousIsNarrow:true})`. Algorithm: grapheme-segment each cluster (`unicode-segmentation`); if the cluster matches the **emoji regex**, route to an `getEmojiWidth` branch — `cluster=2`, single regional indicator=1, regional-indicator **pair**=2, incomplete keycap (digit/symbol+VS16 without U+20E3)=1 (`stringWidth.ts:67–127`); **otherwise** sum `unicode-width` `wcwidth` over the cluster's code units (this is what gives Devanagari conjuncts width 2, matching Bun). Zero-width set: controls `0x00–0x1f`/`0x7f–0x9f` (incl. **tab `0x09` — zero-width**), ZWSP/ZWJ/ZWNJ, BOM, word joiners, variation selectors `0xfe00–0xfe0f`/`0xe0100–0xe01ef`, combining marks `0x0300–0x036f` + Indic/Thai/Lao + `0xfe20–0xfe2f`, Arabic formatting, surrogates, tag chars. ANSI-stripped fast path. **Tabs are expanded to 8-column POSIX stops (dom.ts `expandTabs` → tabstops.ts `DEFAULT_TAB_INTERVAL=8`) BEFORE measurement**, so measure-text never sees raw tabs; a raw tab bypassing expansion is zero-width (`0x09 <= 0x1f`). Unit-test vs a known `Bun.stringWidth` result table incl. **ZWJ family `👨‍👩‍👧`=2** (not 6), skin-tone `👍🏽`, keycap `#️⃣`, CJK wide, Devanagari=2, combining chains, ANSI-embedded.
- **Style/Color + `colorize`** — `Color = rgb| #hex | ansi256(n) | ansi:name`; `TextStyles { color, backgroundColor, dim, bold, italic, underline, strikethrough, inverse }`; wrap in fixed chalk order (bg>fg>modifiers). Implement chalk level gating: truecolor vs 256-color (tmux clamps to level-2 unless `CLAUDE_CODE_TMUX_TRUECOLOR`; xterm.js boosts to level-3); emit level-2 vs level-3 SGR. **Level-2 byte parity requires reproducing color-convert's `rgb→xterm-256` 6×6×6 cube quantization exactly** (not just the SGR envelope). Unit-test byte-exact SGR vs chalk at both levels, incl. a **Claude-orange `rgb(215,119,87)` at level-2** fixture asserting the exact quantized 256-color code + SGR bytes.
- **`node` + `flex`** — implement the TS port's props, **tiered**:
  - *Tier 1 (parity-critical, must implement + fixture):* `flexDirection` (row/column + reverse), `flexGrow`, `flexShrink` (default 1), `flexBasis`/`BasisPercent`, `alignItems`(stretch default)/`Self`, `justifyContent` (all six), `width`/`height`/`min`/`max` (point, percent, auto), `position`(relative/absolute)+insets, margin/padding per-edge, `border` (1 cell/side), `gap`, `overflow`, `display`(flex/none).
  - *Tier 2 (only if a migrated `tui.rs` site uses it):* `flexWrap`(wrap/wrap-reverse), `align-content`, `baseline` alignment.
  - *Skip (the port itself doesn't implement):* aspect-ratio, box-sizing content-box, RTL.
  - **Reproduce the port's load-bearing quirks as enumerated rules (NOT "floor computed dims"):**
    (a) **`roundLayout`** (`index.ts:2435-2506`): per-dim rounding with epsilon thresholds — `frac < 0.0001` → floor, `frac > 0.9999` → ceil, `forceCeil` for fractional **text** width, `forceFloor` for non-fractional text, else half-up (`frac >= 0.4999` → `floor+1`). The code comment states without this `justify-content: center`/`space-evenly` positions go off-by-one.
    (b) **`computeFlexBasis`** AtMost-width-only-for-measure-subtree (`index.ts:2150-2155`).
    (c) **Partial-flex** when `totalGrow`/`totalShrink < 1` (`index.ts:2233-2247`).
    (d) **Four container-sizing branches**: Exactly / AtMost+Scroll / wrap+AtMost+lineCount>1 / content (`index.ts:1554-1569`).
- **`measure_text`** (height only) — height = `w==0 ? 1 : ceil(w/maxWidth)`; `noWrap` when `maxWidth<=0`/non-finite; clamp wrap width to `min(node maxWidth, output.width - x)`. Unit-test the height formula vs Ink `measure-text`. (This produces `{width,height}` only — NOT a wrapped string.)
- **`wrap_ansi`** (the wrapped STRING bytes) — distinct from `measure_text`. Implement the `wrap-ansi` algorithm (word-wrap with ANSI-sequence span tracking, `hard` option, `trim:false`) that `MarkdownTable.tsx:52-56` + `wrap-text.ts:46-56` actually call (`Bun.wrapAnsi` else npm `wrap-ansi`). ANSI-preserving; style re-opens across row boundaries. **Byte-test** its output vs `Bun.wrapAnsi`/`wrap-ansi` for ANSI-mixed, hard-wrap, and CJK/wide-char inputs. (Both table parity and text-node parity depend on these wrapped bytes; `measure_text` alone cannot validate them.)
- **`border`** — `render-border` parity: round glyphs `╭╮╰╯─│`; `contentWidth = width - (left?1) - (right?1)`; vertical borders = height − top − bottom; per-side `borderColor`/`borderDimColor`; `embedTextInBorder` splices pre-rendered ANSI header into top border (start/center/end+offset, measured by `string_width`); borders drawn AFTER children; per-side hide (`borderTop/Bottom/Left/Right=false`) for U-shape. Unit-test vs Ink `render-border`.
- **`table`** — `MarkdownTable` parity (now **5-step**): (1) per-col min = longest word (split `\s+`) floored at `MIN_COLUMN_WIDTH=3`; ideal = `string_width(full plain cell)`; both max'd across header+rows. (2) **`availableWidth = Math.max(terminalWidth - borderOverhead - SAFETY_MARGIN=4, numCols * MIN_COLUMN_WIDTH)`** — the floor is load-bearing on narrow terminals (`MarkdownTable.tsx:128`). (3) cases: `totalIdeal<=avail`→ideal; `totalMin<=avail`→min + `floor(overflow_i/totalOverflow * extra)`; else `scale=avail/totalMin`, cols=`max(floor(min*scale),3)`, `needsHardWrap`. (4) wrap cells; if any row exceeds `MAX_ROW_LINES=4` → vertical key-value. **(5) Post-build safety re-check** (`MarkdownTable.tsx:~311-317`): after `tableLines` is built, recompute `max(string_width(stripAnsi(line)))`; if `> terminalWidth - SAFETY_MARGIN` (a resize-race), **flip the already-rendered horizontal table to vertical**. Emit the whole table as ONE opaque pre-rendered ANSI span (single `StyledSpan`) so the flexbox places it as one unit and never re-wraps mid-row. Manual box-drawing borders + `padAligned` header / per-col-align data. Unit-test vs `MarkdownTable` outputs incl. a **narrow-terminal floor** case and a **resize-race flip** case.
- **`screen`** — 2D grid of styled grapheme cells, content-tall. Placement: ANSI-tokenize SGR+OSC8, grapheme re-segment with `{value, width, styleId, hyperlink}`; wide char crossing last column → SpacerHead + wrap; zero-width skipped; tabs→8-stop (column-aware). Flatten to `Vec<String>` logical rows (the transcript contract). Unit-test wide-char wrap, zero-width skip, tabs, OSC8 preservation.
- **`renderer`** — root width = `terminalWidth`, height = `yogaHeight` (INLINE, content-tall) → `calculateLayout` → place → Screen → `Vec<String>`. Unit-test a REPL-frame-shaped tree produces expected row count + content-tall height.

**Files:** `crates/phi-layout/{Cargo.toml, src/{lib,string_width,colorize,node,flex,measure_text,wrap_ansi,border,table,screen,renderer}.rs, tests/{parity,yoga_fixtures}.rs}`, workspace `Cargo.toml`.
**Verify:** `nix develop -c cargo build -p phi-layout` **and** `nix develop -c cargo test -p phi-layout` green. Non-flexbox gates: `string_width` matches the `Bun.stringWidth` table (incl. ZWJ-family=2, tab=zero-width); `colorize` byte-exact vs chalk (level-2 incl. Claude-orange cube quantization, & level-3); `wrap_ansi` byte-exact vs `Bun.wrapAnsi`; border round+embedded-header vs Ink; table proportional + floor + resize-race-flip vs `MarkdownTable`; `measure_text` height vs Ink; Screen wide/zero/tab; a REPL-shaped frame content-tall.
**BLOCKER-DISCHARGING GATE — flexbox byte-parity fixtures (`tests/yoga_fixtures.rs`):** build the same node trees claude-code constructs and assert `phi-layout`'s `getComputedLeft/Top/Width/Height` are **byte-identical** to golden vectors captured from the TS port (`native-ts/yoga-layout/index.ts`). Fixtures MUST cover, at minimum: `flexGrow`/`flexShrink` distribution under overflow; `justify-content` center + space-evenly with **fractional rounding** (the `roundLayout` off-by-one case); `align-items` stretch + center; nested `border`+`padding` content-box math; `gap`; `position: absolute` + insets; and **text-in-flexGrow** (the AtMost-width measure-subtree basis case). This is the gate that discharges Risk #2 — without it the plan cannot promise flexbox byte-parity. No `phi-session`/`phi-tui` source modified.

---

### Stage B — migrate `phi-session/tui.rs` onto `phi-layout` via compat shim; end with the `Vec<String>` line primitive gone

**Goal:** Move the render path from manual `format!`+SGR+`visible_width` assembly onto `phi-layout`, keeping old + new coexisting behind a compat shim while each step builds+tests green, finishing with the manual line primitive and `Terminal::<W>'s` double-wrap removed — **with the transcript commit engine preserved unchanged**.

**Steps**
- Add `phi-layout` dep. Compat shim: a `FlexRenderer` (Node→Screen→`Vec<String>` rows) + an `InlineCommit` layer (already-final `(scrollback, live)` rows → Ink inline log-update: incremental cell-diff + CR/LF append, **no DECSTBM**, no hardware-scroll). Keep existing `Terminal::render` + `panel_*`/`render_card` as fallback so unmigrated panels still render.
- **Disable `Terminal::render`'s double-wrap for flex rows** (mark flex rows "final" so the inner `wrap_ansi`-to-`self.width` re-wrap + cursor re-locate are skipped). Cursor moves to the flexbox's wrap-locate for the editor Input panel.
- Migrate the box-chrome primitive first (`panel_*`/`render_panel`/`render_repl_panel` + framed chrome borders, `tui.rs:221-299/1777/1956`) → flex item with `border` + auto-width from measured label/body (replaces `fitted_panel_width`). Assistant/You/repl panels render identically before/after (render-golden test).
- Migrate the card header + indent tree (`render_card`, `gutter_*`, `card_body_width`/`repl_body_width`, `status_rows`/`push_bar`/`push_dot`) → nested flex with depth-padding + header-ellipsis fill. Anchor-once identity stays data-side (moves to `phi-toolview` in Stage D; reuses existing output here).
- Migrate wrapping-to-measured-inner-width sites (`wrap_ansi` at `:267/3158/3181/3442/3495`, editor `:1875/1883`) → engine wraps to `content_box_width`; remove `saturating_sub` chains. Keep `wrap_ansi_locate` cursor marks as the place primitive for the editor.
- Migrate message cards (`assistant/user_card_lines`) → bordered flex item with `render_markdown` children + label (replaces `markdown_width`). `render_markdown` stays the text-node producer (Stage E upgrades it).
- Migrate tables/aligned columns (`render_popup`, `Selector::render`) → flex row with fixed-width cells + per-cell SGR; fold `Selector` into the same flex (or keep as leaf producer) so framed chrome/footer fill behave consistently with cards.
- Migrate panel/region placement (`draw` chrome assembly) → vertical stack: scrollable transcript flex + fixed bottom chrome flex; `transcript_height=height-chrome.len()` becomes flex space distribution.
- Migrate popups/editor panel (`render_popup`, editor `:1799-1921`) → docked/overlay flex children; editor rows wrap to `input_inner`, cursor via flex wrap-locate; pagination/overflow become flex overflow rules.
- **Convert the open-row tracking seam:** `open_tool_rows`/`open_subcall_rows` are **already keyed by `(depth, call_id)`**; the only thing replaced is the inner `start: usize` Vec-index into `self.block`. `replace_block_rows`/`shift_open_row_starts` (`tui.rs:3264/3248`) splice live updates (spinner ticks, `ToolEnd`/`SubcallEnd`) by that absolute index. Re-key the `start` to **`(key, BlockSpec.version)`** recomputation (`tui.rs:1639/1649`) so flex positional identity doesn't break these splices. This edits only private `Ui` fields — **no frozen struct touched**.
- **CRITICAL invariant gate:** prove layout is **pure/monotonic in `(key,width,content)`**. Add a test re-rendering a streaming card at same `(key,width,content)` → identical row count + content — so flex reflow can't collapse `stable_prefix` / retrigger `VOLATILE_REARM`/`STABLE_PREFIX_COMMIT`. `commit_stable` per-block flags unchanged. **Also assert the inter-block blank is emitted as a literal `""` row in the `Vec` (not a flex gap)**, so `strip_plain_blank_edges`/`commit_safe_end` see the same rows as today — converts the acknowledged blank-row risk into a checked invariant.
- Reproduce blank-line separator + `last_activity` gate + transcript single-blank insertion (`render.rs:87-91` + `diff.rs strip_plain_blank_edges`) as flex gap/margin so `commit_safe_end` boundaries stay byte-identical. Embedded `durable_end=commit_safe_end` + standalone snapshot-safe promotion (`render.rs:151-161`) unchanged.
- **End state:** remove old `panel_*`/`render_card`/`gutter`/manual-width functions + `Terminal::render`'s double-wrap. Transcript engine (**`BlockSpec`/`Frame`/`Transcript::render`/`derive_live_commit_state`**) preserved — it still diff-commits pre-rendered logical rows; only the source changed. `fill_bg` stays SGR-only/unused for panel bodies (keeps `Terminal` diff-row equality + `normalize_row` stable).
- Commit at green per migrated panel group; keep a render-golden test suite (old-output snapshots) so each step is byte-verified before the old path is deleted. **Sequence the final removal as a distinct commit gated by `nix develop -c cargo check -p phi-session` showing zero remaining references to the removed symbols** (`panel_border`/`panel_row`/`panel_bottom`/`render_panel`/`render_card`/`gutter_*`/`fitted_panel_width`) — so the red-tree window is provably empty, not just procedurally guarded by render-golden.

**Files:** `crates/phi-session/{Cargo.toml, src/tui.rs}`, `transcript/{types,render,diff}.rs` (confirm-only, unchanged), `crates/phi-tui/src/{terminal.rs (wrap-skip + double-wrap removal; keep RawModeGuard/LiveCursor re-exports), selector.rs (fold; canonical Paginated primitive), lib.rs (do NOT change public re-export list)}`, `crates/phi-session/tests/render_golden.rs` (new).
**Verify:** `nix develop -c cargo build -p phi-session` **and** `nix develop -c cargo test -p phi-session` green. `render_golden.rs`: every migrated card/panel renders byte-identical to pre-migration snapshots. Layout-purity + blank-`""`-row tests pass. 357 baseline green. Transcript contract unchanged (`BlockSpec`/`Frame`/`Transcript::render`/constants untouched). git diff shows `panel_*`/`render_card`/`gutter`/manual-width removed, no double-wrap; `Transcript::render`/`diff.rs` unmodified.

---

### Stage C — 6 claude-code themes + ThemePicker, plumbed WITHOUT frozen boundary fields

**Goal:** Port all 6 palettes (`utils/theme.ts`) + `ThemePicker`/`commands/theme` into phi, surfaced to the REPL with **no** field added to `SessionIo`/`UiSetup`/`TuiSetup`. Seam: persist in non-frozen `Config.theme`; seed via `PHI_THEME` env set by the launchers; hold live state as in-memory `Ui.current_theme`.

**Steps**
- Port the 6 palettes into `crates/phi-session/src/theme/` as a `Theme` enum (`Dark`, `Light`, `LightDaltonized`, `DarkDaltonized`, `LightAnsi`, `DarkAnsi`) each holding the ~70 raw color keys (`claude`, `claudeBlue_FOR_SYSTEM_SPINNER`, `text`, `subtle`, `success`, `error`, `warning`, `diff*`, `userMessageBackground/Hover`, `selectionBg`, `rainbow_*`, …). RGB themes use `rgb()`; ANSI themes use the 16 named `ansi:` colors. Shimmer keys = pre-lightened RGB variants. `getTheme(name)` (default dark); `THEME_NAMES` + `auto` (→dark/light).
- Add `theme: Option<String>` to top-level **`Config`** (`config.rs:17`) — Config is the loader, **not** a frozen type. `ThemePicker` writes via `Config::save`.
- Runtime read seam: **`PHI_THEME` env, but ONLY as a one-time pre-runtime seed — never `set_var` at runtime.** `std::env::set_var` is process-global, **`unsafe` under Rust edition 2024** (this repo is edition 2024), and a **data-race + cross-session-clobber hazard** under the mux's multi-threaded tokio runtime (per-session `driver::spawn` runs concurrently). Rules:
  - **Standalone (`phi-app/src/main.rs`):** read `Config.theme`, `std::env::set_var("PHI_THEME", …)` **once, before** `tokio::main` spawns any worker thread, inside an `unsafe` block with a SAFETY comment. Acceptable: single-threaded startup.
  - **Embedded/mux (`phi-mux/src/driver.rs`):** **do NOT `set_var` at runtime.** Seed `Ui.current_theme` directly from `Config.theme` at `Ui` construction (pass the resolved theme string through the existing launch path; if that requires a non-frozen struct field, prefer reading `Config.theme` inside `Ui::new` rather than smuggling via env). Per-session live `ThemePicker` persistence writes `Config.theme` + propagates via a **per-session in-memory channel**, NOT `set_var`.
  - `Ui` reads `PHI_THEME` **once at construction** (seed), then never reads env again. Live switching mutates `Ui.current_theme` only.
- Hold `current_theme` as in-memory `Ui` state (seed from env/`Config.theme`, default `dark`/`auto`); live switching mutates `Ui.current_theme`; persistence to `Config.theme` on save. Audit ALL theme reads to go through one `current_theme` accessor (so live preview never desyncs from an env re-read).
- Replace `tui.rs` SGR `const`s (`tui.rs:36-63`: `RESET`/`BOLD`/`DIM`/`CYAN`/`YELLOW`/`TOOL_ACCENT`/`PATH_FG`/`USER_FG`/`ASSISTANT_FG` — representative subset; also the border/accent consts) with theme-driven colorize tokens (role → Theme key → `phi-layout colorize`). Switching is a palette swap; no line-assembly change.
- Build **`ThemePicker`** as a slash command + overlay (parity to `commands/theme/theme.tsx` + `components/ThemePicker.tsx`): 6 themes, **reuses Stage B's canonical `Paginated` selector** (`POPUP_MAX_VISIBLE=6`, selected row cyan, overflow count, `j/k + ctrl-n/p` nav), live-preview via `Ui.preview_theme`, persist on accept via `Config.theme` save (+ per-session channel in mux mode; NO `set_var`).
- Leave the **mux's own chrome palette** (`crates/phi-mux/src/ui/theme.rs` Charmtone constants) unchanged for v1 — REPL claude-themed ANSI flows through the embedded vt100 and the mux maps via `screen_cell_colors`. Mux-theme parity is deferred. (Verify the 6 palettes map cleanly through the mux's 16-color `ansi_palette` + truecolor passthrough; flag any unmappable color.)

**Files:** `crates/phi-session/src/theme/{mod,palettes,resolver}.rs`, `config.rs`, `tui.rs`, `crates/phi-app/src/main.rs` (one-time pre-runtime `set_var`), `crates/phi-mux/src/driver.rs` (**NO** `set_var`; per-session channel), `crates/phi-session/tests/theme_snapshot.rs`.
**Verify:** `nix develop -c cargo build` **and** `nix develop -c cargo test` green. `theme_snapshot.rs`: fixed frame under each of 6 themes emits expected palette SGR (truecolor for RGB; 16-color for ANSI). Boundary grep: no field added to `SessionIo`/`UiSetup`/`TuiSetup`; `MuxConfig` unchanged; `phi-tui` re-export list unchanged. **`set_var`-hazard grep: no `std::env::set_var` call in `phi-mux/src/driver.rs`; the only `set_var` is the one-time pre-runtime call in `phi-app/src/main.rs` inside an `unsafe` block.** `PHI_THEME` seeding verified (startup seed toggles theme; live switching mutates `current_theme` without restart and without touching env in mux mode).

---

### Stage D — shared headless crate `phi-toolview` for `tool_identity`

**Goal:** Extract the duplicated anchor-once logic into a new crate depending only on `serde_json` + `unicode-width` (NOT `phi-tui`), so `phi-session` (replacing `tui.rs:422 Identity/tool_identity`) and `phi-app` (replacing `tool_view.rs`) share ONE source of truth. Independent of the flexbox; parallelizable with B/C.

**Steps**
- Create `crates/phi-toolview` (deps `serde_json` + `unicode-width`; NO `phi-tui` — confirmed phi-app can't depend on it).
- Move `Identity { anchor, body }` (`tui.rs:422-425`) + `tool_identity(name, &Value)` (`tui.rs:427+`) and `residual_args`/`tool_shows_args_section`/`tool_shows_input_section`/`tool_input_label`/`tool_input_lines`/`tool_display_name`/`tool_title_suffix`/`first_line_trunc` from `phi-app/src/tool_view.rs:18/51+` into `phi-toolview`, reconciling the two implementations into one anchor-once rule (bash/repl keep body only for multi-line; unknown tools keep body only when value is multi-line/long/nested).
- Add shared glyph `const`s (the tool-use dot **`●`/`⏺` (BLACK_CIRCLE, platform-gated)**, result `✓`/`✗`, spin, depth_tag, limit_name, preview/indent, bash_timeout_arg, parse_bash_result) keyed off a passed color — headless-crate-agnostic. (`◇`/`◆` DIAMOND glyphs are intentionally NOT here — they belong to the out-of-scope background-task surface.)
- `phi-session` replaces local `tool_identity` with `phi_toolview::tool_identity`; `phi-app` replaces `tool_view.rs` logic with re-exports/wrappers. Unit tests for the anchor-once invariant across bash/repl/known/unknown (parity to `tui.rs:4258+` + `phi-app/tests.rs:210`).

**Verify:** `nix develop -c cargo build` **and** `nix develop -c cargo test` green. `cargo tree -p phi-app` shows NO `phi-tui` transitive dep; `phi-toolview` is the sole tool-identity source. Existing anchor-once tests pass against the shared impl.

---

### Stage E — parity polish: claude-orange spinner/verbs, `●`/`✓`/`✗` cards, borderless markdown (`formatToken` parity), `❯` input trough, dim footer, dash bullets, selectors, turn-completion verb

**Goal:** Apply the claude look/behavior onto the B/C/D foundations: orange spinner + `SPINNER_VERBS`; **`●`/`⏺` (BLACK_CIRCLE)** tool-use header + `✓`/`✗` result cards; borderless markdown with `blockquote ▎ bar`, `▏ code prefix`, *proportional* tables, OSC-8 links, **headings, depth-numbered ordered lists, `hr`, em/strong, issue-ref linkify**; `-` bullets; top-only rounded input trough with `❯`; dim footer (no perm slot, no fast-mode slot); `ctrl-l`=model, `ctrl-t`=thinking; `j/k + ctrl-n/p` selectors; turn-completion verb summary.

**Steps**
- **Spinner:** claude-orange (`theme 'claude'`) for braille frames; cycle `SPINNER_VERBS` in header (verbs orange per #7). Re-skin existing `Spinner` via theme tokens — do NOT change its public re-export.
- **Tool cards:** header = **`●`/`⏺` (BLACK_CIRCLE, platform-gated — NOT `◇`)** + `phi-toolview` display name in orange (parity to `ToolUseLoader.tsx:20`); result = `✓` (success) / `✗` (error) + identity anchor-once + preview. Subcall cards same shape. `status_rows`/`push_bar`/`push_dot` → header-only cards with the glyph set.
- **Borderless markdown:** Assistant/User cards lose the four-wall panel; use claude's borderless style (top label + indented body, no side walls) — preserve blank-line separator + commit boundaries. Upgrade `render_markdown` to full `formatToken` parity (`markdown.ts:49-359`): blockquote → `▎` bar per line; inline/background code → `▏` prefixed/styled; code blocks via `highlight_code` with claude token colors; diff via `highlight_diff_line` with theme diffAdded/Removed/AddedWord/RemovedWord; proportional tables via `phi-layout` (Stage A); **OSC-8 hyperlinks** emitted + preserved through the Screen (noSelect stays inline-off). **Distinguish *style upgrades* from *net-new* surfaces:** *style upgrades* (phi already has these, restyle to parity) — **headings** h1=bold+italic+underline, h2+=bold (phi today renders headings bold-only); **blockquote** `▎` bar (phi uses dim `─`); em/strong inline (phi already has these — verify-only, no reimplementation). **Net-new** (phi has nothing today) — **depth-numbered ordered lists** via a `getListNumber` equivalent producing `number` at user-facing depth 0, `letter` at depth 1, `roman` at depth 2 (the internal Ink `listDepth` is `nesting_level+1`; traced at `markdown.ts:347-358`); **`hr`** = literal `---` (phi has dim full-width `─` U+2500 — replace); **`linkifyIssueReferences`** (`owner/repo#NNN` → OSC-8 github link). Bullets → `-` per #8.
- **Structured-diff view:** add a dedicated `FileEdit`/`NotebookEdit` structured-hunk diff view (parity to `StructuredDiffList`/`StructuredDiff`: a hunk list with `+`/`-` lines and **ASCII `...`** (three dots, NOT U+2026) ellipsis separators between hunks, each wrapped `<NoSelect><Text dimColor>...</Text></NoSelect>` per `StructuredDiffList.tsx:27`) — distinct from the inline-markdown `highlight_diff_line` path. (Not a flex-table; a vertical hunk stack.)
- **Interrupt / error-fallback messages:** phi already renders a `{YELLOW}previous run was interrupted mid-turn…` banner (`tui.rs:2435-2441`) and maps `StopCause::Cancelled => ExchangeStatus::Interrupted` (`tui.rs:4024`). Add Stage-E parity for the deterministic interrupt/error surfaces: an **interrupt banner** parity to `InterruptedByUser.tsx` (dim "Interrupted · What should Claude do instead?" — replace phi's hard-coded YELLOW SGR with a `theme.subtle`/`theme.error`-driven banner matching claude-code's dim style), and a **tool-use error/rejected fallback card** parity to `FallbackToolUseErrorMessage.tsx` (multi-line error card + a `+N lines (ctrl+o to see all)` truncation row). Add byte/SGR snapshot assertions for both to `parity_polish.rs` and a Stage F corpus entry for the interrupt banner. (A permission-rejected surface — `FallbackToolUseRejectedMessage.tsx` — is out of scope in v1 since phi has no permission model; listed explicitly in Not-in-scope.)
- **Input trough:** top-only rounded border (`╭..─..╮`) with `❯` + editor; cursor via flex wrap-locate. No statusLine hook (#9) — header statically composed.
- **Footer:** single dim hint line; NO permission slot (#13); NO fast-mode slot (#10).
- **Keybindings:** `ctrl-l` opens model picker (#5) — reuse the existing `/model` overlay (already at `tui.rs:2711`) re-skinned; `ctrl-t` toggles thinking (#6, already at `tui.rs:2717`). Selectors navigate `j/k + ctrl-n/p` via Stage B's canonical `Paginated`. Keep crossterm-`Event` forwarding + keymap boundary untouched. **`crates/phi-mux/src/keymap.rs` needs NO change** (it has no `ctrl-l`/`ctrl-t` bindings — verified; its ctrl-defaults are mux-navigation: `ctrl+left/right/up/down/n/r/shift+r/delete/shift+d/shift+c/v/g/q`, none of which is ctrl-l/ctrl-t) — so it is NOT in this stage's file list.
- **Turn-completion verb summary — NEW Stage-E work, not an existing site.** `TURN_COMPLETION_VERBS` exists nowhere in the phi source today, and the root depth==0 `RunEnd` case has **no arm**. Add a new match arm at the `RunEnd` arm (`tui.rs:3862`) for the root depth==0 case (currently `:3871` is inside the depth>0 subcall-skip body, not a turn-completion site): emit a dim one-line `TURN_COMPLETION_VERBS` summary ("Baked for 4.2s") as a status card, honoring `commit_stable` (after-block true) so it commits cleanly. Treat `:3862` as the insertion point, not `:3871`.
- Re-skin mux-facing chrome where it appears in the REPL transcript — do NOT touch the mux's own `scene.rs`/`theme.rs`/`overlays` (#12).

**Files:** `crates/phi-session/src/tui.rs`, `crates/phi-tui/src/markdown/{render,output,constants,highlight,mod}.rs` (`highlight.rs` for `highlight_code`/`highlight_diff_line` re-color), `crates/phi-toolview/src/glyphs.rs`, `crates/phi-session/tests/parity_polish.rs`. (**`phi-mux/src/keymap.rs` is NOT touched** — ctrl-l/ctrl-t live in `tui.rs`.)
**Verify:** `nix develop -c cargo build` **and** `nix develop -c cargo test` green incl. `parity_polish.rs`. `parity_polish.rs` **encodes the Visual checklist as byte/SGR snapshot assertions** (not manual-only) so it is a real cargo gate: orange spinner+verb bytes; **`●`/`⏺` header** (assert BLACK_CIRCLE, assert absence of `◇`); `✓`/`✗` cards; borderless markdown bytes (`▎`, `▏`, proportional table, `-`, OSC-8 link bytes, heading bytes, ordered-list roman-marker bytes, `---` hr bytes, em/strong bytes, issue-ref OSC-8 bytes); structured-diff `+`/`-` + ASCII `...` bytes (assert `2e 2e 2e`, NOT U+2026 `…`); interrupt banner bytes + tool-error-fallback card bytes; `❯` trough bytes; dim single footer (assert no perm/fast slot). Keybinding assertions: `ctrl-l`→model overlay, `ctrl-t`→thinking, `j/k + ctrl-n/p`→selector move. mux grep: no `crates/phi-mux/src/ui` changes.

---

### Stage F — side-by-side parity audit vs claude-code (no mux tests; byte-level where deterministic)

**Goal:** Final adversarial audit vs `~/Dev/claude-code`: byte-level parity for deterministic surfaces (borders, tables, colorize, string_width, wrap, theme SGR), behavioral for the rest; document residual divergences. No mux tests (#12); mux byte-stability re-confirmed.

**Steps**
- Generate a deterministic frame corpus from claude-code Ink (round borders, proportional table incl. narrow-terminal floor + resize-race flip, blockquote `▎`, code block `▏`, OSC-8 link, issue-ref `owner/repo#NNN`→OSC-8, headings h1/h2/h3, depth-numbered ordered list (number→letter→roman at visible nesting 0/1/2), `hr` `---`, em/strong paragraph, **`●`/`⏺`** `✓`/`✗` cards, structured-diff `+`/`-` + ASCII `...` hunk-view separator, interrupt/error-fallback message surfaces, wrapped paragraph, emoji+CJK+Devanagari line, all 6 themes) captured as ANSI bytes; diff byte-for-byte vs `phi-layout`/`phi-session` output.
- Audit the inline commit path: content-tall frames, incremental cell-diff + CR/LF append, **no DECSTBM/hardware-scroll/selection/sticky-pill** emitted (deliberate — see Not-in-scope).
- Re-verify seam-risk invariants: layout purity/monotonicity in `(key,width,content)`; inter-block blank as literal `""` row; `commit_stable` boundaries; `durable_end` mapping; constants untouched; open-row tracking by `(key,version)`.
- Confirm boundary freeze: struct fields unchanged; RLM types unchanged; `phi-tui` re-export list intact; transcript contract unchanged.
- Confirm mux byte-stability: `nix develop -c cargo build -p phi-mux` green, grep — no new `MuxConfig` fields, no `crates/phi-mux/src/ui` palette/layout changes, `display_cell_width`/`truncate_to_cells` unchanged, no `set_var` in `phi-mux`. No mux tests added.
- Write up residual divergences (chalk level-2 cube-quantization corners, braille spinner frame timing, provider-specific text) as known-divergence notes, NOT plan scope. Overwrite this plan file with the final plan + audit-results section.

**Files:** `crates/phi-layout/tests/byte_parity_corpus.rs`, `crates/phi-session/tests/inline_commit_audit.rs`, this plan file.
**Verify:** `nix develop -c cargo test --workspace` green. `byte_parity_corpus.rs` byte-equal vs Ink for deterministic surfaces. `inline_commit_audit.rs`: no DECSTBM/selection/sticky-pill bytes; content-tall. Boundary-freeze greps pass. mux build green + unchanged. Final git diff scoped to `phi-layout`/`phi-toolview`/`phi-session`/`phi-tui(markdown,measure,wrap,render,selector,terminal wrap-skip)`/`phi-app(tool_view+main env)`/`plans`**; `crates/phi-mux` diff empty except pre-existing.

---

## Risk Areas

1. **STRING_WIDTH PARITY** — `phi-layout` must reproduce `Bun.stringWidth({ambiguousIsNarrow:true})` via `unicode-width` `wcwidth` per code unit (NOT pure grapheme count — Devanagari ligatures must read width 2). The mux's `cells.rs` uses a naive per-char sum with `.max(1)` and no grapheme clustering → diverges. Keeping the mux on `cells.rs` while the flexbox uses `phi-layout::string_width` creates **two** measurement stacks that must never be silently unified without a byte-equality audit, or mux overlay column widths shift.
2. **FLEXBOX FIDELITY (TS-port parity, NOT canonical Yoga)** — pixel-exact parity needs `phi-layout` to reproduce claude-code's **hand-rolled TS yoga port** (`native-ts/yoga-layout/index.ts`) — its measure/constraint/place resolution order + `roundLayout` epsilon/half-up rounding + the four container-sizing branches + partial-flex + AtMost-width measure-subtree quirk. A "close" solver mis-sizes proportional tables, card borders, item alignment. Largest build risk; NOT a ratified library — **must be unit-verified against TS-port-resolved fixtures** (the Stage A `tests/yoga_fixtures.rs` blocker-discharging gate). Note the port itself does NOT implement aspect-ratio/box-sizing-content-box/RTL, so neither does `phi-layout`.
3. **DOUBLE-WRAP SEAM** — `Terminal::render` re-wraps every live line to `self.width` + re-locates cursor (`terminal.rs:163-178`). If the flexbox already wraps to a content-box width, double-wrapping disagrees with flex cursor placement → mis-wrapped bodies + misplaced editor cursor. Stage B must skip the double-wrap for flex rows.
4. **LAYOUT PURITY vs COMMIT STABILITY** — the transcript diff-commits rows assuming they're a deterministic, monotonic function of `(key,width,content)`. A flex reflow (width change, streaming card) that shifts row counts collapses `stable_prefix` and retriggers `VOLATILE_REARM_FRAMES`/`STABLE_PREFIX_COMMIT` → scrollback thrash, and in embedded vt20 → irreversible duplicate rows. Layout MUST be pure/monotonic; enforced by a dedicated Stage B test.
5. **OPEN-ROW TRACKING** — `replace_block_rows`/`shift_open_row_starts` splice live updates by absolute `Vec` index. A flexbox makes row identity positional; MUST migrate to `(key, BlockSpec.version)` or spinner-tick / `ToolEnd`/`SubcallEnd` live updates splice into the wrong rows.
6. **CHALK LEVEL-2/LEVEL-3 GATING** — byte-exact SGR requires matching chalk's terminal-capability downgrade (xterm.js→level-3 truecolor; tmux→level-2 256-color unless `CLAUDE_CODE_TMUX_TRUECOLOR`). Emitting the wrong level breaks byte parity + can corrupt tmux colors.
7. **BlockSpec/Frame "REPLACEABLE" TENSION** — the original replaceable-list overreached; these are the content-agnostic commit-stability engine backing embedded-vt20 scrollback immutability. **PRESERVED** — replacing them would violate a frozen invariant. Only `Terminal<W>` + the manual line primitive are replaced.
8. **EMBEDDED-MODE THEME COLOR MAPPING** — REPL claude-themed ANSI flows through the embedded vt20 and the mux maps via `screen_cell_colors`. Stage C leaves the mux palette unchanged; if a theme color falls outside the mux's `ansi_palette`/truecolor mapping, embedded rendering may shift. Verify the 6 palettes map cleanly through the mux's 16-color `ansi_palette` + truecolor passthrough.
9. **SELECTOR FOLD-IN** — `Selector::render` (`selector.rs:178`) is a peer layout system. Folding it (vs keeping it a leaf producer) risks diverging framed chrome/footer fill from cards; verify byte-equal against its pre-migration snapshot during Stage B.
10. **THEME SEAM PERSISTENCE/LIVE-SWITCH + `set_var` HAZARD** — `PHI_THEME` seeds at startup; live switching relies on in-memory `current_theme`; any code path re-reading env desyncs live `ThemePicker` preview (audit ALL reads through one accessor). **Additionally `std::env::set_var` is `unsafe` under edition 2024 and process-global** — calling it at runtime in the mux (multi-threaded tokio, concurrent per-session `driver::spawn`) is a **data-race + cross-session-clobber hazard**. Mitigation (enforced in Stage C): `set_var` ONLY once pre-runtime in `phi-app/src/main.rs` (inside `unsafe`, before any worker thread); **never** in `phi-mux/src/driver.rs`; live persistence via per-session in-memory channel + `Config.theme` save, not `set_var`.

---

## Feasibility notes

- **Transcript engine PRESERVED, not replaced** — deliberate deviation from the original "replaceable" list. Verified seam-risk shows `derive_live_commit_state` diff-commits pre-rendered logical rows; embedded vt20 scrollback is irreversible; `commit_stable` safe-ends are load-bearing. Replacing them breaks embedded scrollback immutability (a frozen invariant). `Terminal<W>` (double-wrapping renderer) + the manual `Vec<String>` line primitive ARE replaced; `BlockSpec.lines` is repointed from `format!` to flexbox rows while the type/contract stays. Pixel-exact layout without touching the commit engine.
- **Decision #1 (pixel-exact) OVERRULES** the prior "behavior not pixels" principle. New plan targets byte-level parity for deterministic surfaces (borders, tables, colorize SGR, string_width, wrap, theme tokens) and behavioral for inherently non-deterministic ones (spinner frame timing, provider-specific text). Feasible because Ink's layout+measurement+colorize are pure functions of `(inputs, terminalWidth, color-level)` — reproducible in Rust. NOT feasible to claim 100% byte-parity for streaming timing-dependent output; the plan scopes byte-parity to the deterministic surfaces.
- **Custom flexbox (#2) feasible** — Ink's flex surface is a bounded, enumerated prop set. Implementing a Yoga-resolution-order solver in Rust is substantial but bounded; does NOT require WASM or porting Bun. The proportional table is NOT a flex-column problem — it's a custom 4-step algorithm reproduced directly in `phi-layout` (Stage A), which de-risks table parity.
- **Inline-only v1 (#3) feasible** — matches Ink's non-fullscreen path exactly: content-tall Screen (`height=yogaHeight`), incremental cell-diff + CR/LF append via `InlineCommit`, no DECSTBM/hardware-scroll/selection/sticky-pill. The existing transcript engine already does row-diff commit; Stage B replaces only the emit layer (`Terminal::render` double-wrap → `InlineCommit` log-update path).
- **Theme plumbing w/o frozen fields (Stage C) feasible** — via `Config.theme` (non-frozen loader) + `PHI_THEME` env (runtime seed) + in-memory `current_theme` (live switch). Chosen over `MuxConfig`-field (reaches only the mux, not standalone) and AuthStorage-slot/AppRuntime-status-map (poor fit). Reaches BOTH mux-embedded and standalone launch paths with zero frozen-struct change.
- **Shared headless crate (#11) feasible** — `phi-toolview` depends only on `serde_json` + `unicode-width`; both `phi-session` and `phi-app` can depend on it without pulling `phi-tui` (confirmed; logic currently duplicated across `tui.rs:422` and `tool_view.rs:18`).
- **#9/#10/#13 omissions feasible** — they remove surface area; footer becomes a single static dim hint line. **#12 no mux tests feasible** — rewrite confined to `phi-layout`/`phi-session`/`phi-tui(markdown,render,selector)`/`phi-app`; mux's `display_cell_width`-based overlays not perturbed as long as `cells.rs` stays byte-stable.
- **RESIDUAL** — braille/dots spinner frame timing and provider-specific text are non-deterministic, excluded from byte-parity; chalk level-2 downgrade under tmux (without `CLAUDE_CODE_TMUX_TRUECOLOR`) must be matched or color bytes differ (Stage A's colorize implements the gating). Theming the MUX chrome is a separate deferred seam that would break #12's no-mux-touch posture — not in v1.

---

## Open Questions resolved

All 13 answered. No open questions remain prior to execution; per-stage verification gates cover any residual unknowns (especially TS-port flexbox-fixture parity in Stage A and layout-purity in Stage B).

## Not in scope

- RLM engine, harness, session task, any `phi-rlm` internals beyond consuming the `RlmEvent` stream.
- The mux (`phi-mux`) internals, vt20 compositor, its `theme.rs` (Charmtone), `terminal_view.rs` — touched ONLY via the `SessionIo` contract + keeping byte-stable `phi-tui` re-exports. (`phi-layout` becomes a genuine **transitive Cargo dep** of the mux via `phi-session`, but no mux source uses its API — "no transitive *boundary* coupling"; verify `cargo tree -p phi-mux` shows `phi-layout` only under `phi-session`.)
- ratatui, React, Ink, chalk, Bun, or any external layout dep — a hand-rolled flexbox; the mux stays on naive `cells.rs`.
- Canonical-Meta-Yoga errata, aspect-ratio, box-sizing content-box, RTL — the TS parity target itself doesn't implement these.
- Alt-screen / fullscreen scroll mode (deferred).
- The mux's own `theme.rs` palette parity + ThemePicker there (deferred).
- `statusLine` shell-command hook, fast-mode, todos view, task:background, voice, image paste, global-search, quick-open — omitted/deferred per decisions.
- **Scroll/selection surfaces deferred v1 (justified by the inline-content-tall `InlineCommit` path):** virtual message-list windowing, mouse-wheel/pager scroll (`g`/`G`/`ctrl-u`/`d`/`b`/`f`), text selection + copy-on-select, sticky-prompt. The Stage F "no DECSTBM/selection/sticky-pill" audit is a deliberate check of this deferral, not an oversight.
- The `◇`/`◆` DIAMOND glyph surfaces (background-task / remote-session / ultrareview) — out of scope; tool cards use `●`/`⏺`.
- Changing the transcript `BlockSpec`/`Frame`/`Transcript::render` contract or commit heuristics — **preserved**, not replaced.
- `ProviderLm`, `AppRuntime`, `AuthStorage`, `Registry`, `SessionStore`, `MuxConfig` API extensions — only existing call-site patterns reused; no MuxConfig theme field in v1.
