# TUI overhaul plan: beautiful + minimal

This plan targets the real UI layers in this repo:

- **Mux chrome**: `crates/phi-app/src/mux/ui/*`
- **Standalone session UI**: `crates/phi-app/src/tui.rs`
- **Shared widgets/primitives**: `crates/phi-tui/*`

The current mux is already solid, but it still feels visually busy:
- a vivid Charmtone palette in `mux/ui/theme.rs`
- a permanently present sidebar in `mux/ui/layout.rs`
- animated/gradient-heavy session rows in `mux/ui/sidebar.rs`
- a loud colored status bar in `mux/ui/scene.rs`
- boxed overlays with inconsistent visual weight
- duplicated styling logic in standalone `tui.rs` via hardcoded ANSI constants

The goal is not “more decoration”; it is **less UI, more calm**.

---

## 1. North-star

The TUI should feel like this:

- **One quiet canvas**
- **One accent color**
- **One primary focus at a time**
- **State shown only when useful**
- **Whitespace and typography do most of the work**
- **Color used for meaning, not ornament**
- **Motion reduced to a minimum**

If a UI element does not help orientation, selection, or error recovery, it should disappear.

---

## 2. Product-level design rules

### 2.1 Visual rules
1. Reduce to **3 surfaces max**
   - canvas
   - raised surface
   - transient overlay surface

2. Reduce to **1 accent family**
   - accent should indicate focus/selection/actionability
   - do not use separate “fun” accent colors for decoration

3. Use semantic colors only for:
   - success
   - warning
   - error
   - running/busy

4. Prefer **shape/weight/spacing before color**
   - selection should read from an accent rail or weight change first
   - current/active should not require a neon background

5. Borders should be rare
   - prefer spacing, subtle contrast shifts, and single-edge rules
   - overlays should not look like modal windows from a GUI toolkit

### 2.2 Interaction rules
1. The **main pane is the product**
   - terminal/session content gets maximum space
   - chrome should retreat unless actively used

2. The sidebar is **navigational, not structural**
   - it should feel lightweight and collapsible
   - if there is only one project or one useful target, it should nearly disappear

3. The statusline should be a **whisper**
   - mostly metadata, not branding
   - no saturated permanent bar unless in command/error mode

4. Overlays should behave like **sheets**
   - centered, narrow, breathable
   - same spacing rhythm across help/model/usage/archive

5. Animation should be nearly absent
   - keep a spinner only where true waiting feedback matters
   - remove animated gradients and flashy transitions

---

## 3. Target visual system

## 3.1 New token model

Introduce a semantic design-token layer and use the same names in mux + shared widgets:

- `canvas`
- `surface`
- `surface_raised`
- `surface_overlay`
- `border_subtle`
- `text`
- `text_muted`
- `text_soft`
- `accent`
- `accent_soft`
- `success`
- `warning`
- `error`
- `focus_bg`
- `selection_bg`

### Suggested dark palette
Keep it neutral and slightly warm/cool, not candy-colored.

- canvas: `#111318`
- surface: `#151922`
- surface_raised: `#1A1F29`
- surface_overlay: `#1D2430`
- border_subtle: `#2A3140`
- text: `#E7EAF0`
- text_muted: `#9AA3B2`
- text_soft: `#6F7887`
- accent: `#8FD3C1`
- accent_soft: `#2A3B39`
- success: `#7BC099`
- warning: `#D8BE72`
- error: `#D97C7C`

Important: this is a starting palette, not sacred. The structural rule matters more than exact hexes.

## 3.2 Shared design-system work
Create a lightweight shared style layer:

- add `crates/phi-tui/src/style.rs` or `theme.rs`
- move shared ANSI roles there for standalone widgets
- keep mux `Color` tokens in `crates/phi-app/src/mux/ui/theme.rs`
- use the **same semantic token names** in both places

This lets `selector.rs`, `markdown.rs`, and standalone `tui.rs` visually match mux without forcing a single rendering backend.

---

## 4. Layout overhaul

## 4.1 Sidebar
Current: persistent left rail with project/session rows, spinner/status glyphs, gradient emphasis.

Target:
- narrower default visual weight
- calmer rows
- hidden automatically when not needed
- no decorative animation

Changes:
- keep sidebar width logic in `mux/ui/layout.rs`, but add:
  - **auto-collapse when only one project and one active session**
  - optional toggle key for show/hide
  - slimmer default width target
- in `mux/ui/sidebar.rs`:
  - remove animated gradient text
  - replace active row styling with:
    - subtle row background OR accent rail
    - bold title for selected row
    - muted metadata
  - replace braille spinner in list rows with either:
    - static dot for active sessions, or
    - only one minimal spinner in the statusline for the focused session
- keep sticky project headers, but make them quieter:
  - muted uppercase or small-title styling
  - no heavy rule treatment

### Sidebar content hierarchy
Each row should read in this order:
1. selection/focus
2. session title
3. state
4. metadata

Not:
1. color spectacle
2. glyph noise
3. title

## 4.2 Main pane
The terminal pane already owns the experience; preserve that.

Changes:
- reduce surrounding chrome contrast so the content feels full-screen
- ensure terminal background matches canvas/surface cleanly
- avoid visual seams between sidebar and terminal
- prefer one clean vertical division if the sidebar is visible

## 4.3 Statusline
Current: colored status bar with mode chip, project, session, status badge.

Target:
- mostly neutral by default
- colored only in command mode, errors, or short-lived confirmations
- concise wording and more empty space

Changes in `mux/ui/scene.rs`:
- default statusline background should be near `canvas` or `surface_raised`, not saturated
- keep command mode highlighted, but with a restrained warning tone
- success/error flashes should be brief and subtle
- left side:
  - reduce `NOR/CMD/OK/ERR` prominence
  - show mode only when not normal, or render normal mode as nearly invisible
- center:
  - session title only if space allows
- right:
  - concise status tokens, e.g. `thinking`, `tool: bash`, `done`
  - no decorative symbol unless it adds clarity

Ideal normal bar:
- almost empty
- project muted left
- session centered if space permits
- status muted right

---

## 5. Overlay overhaul

Files:
- `mux/ui/overlays/dialog.rs`
- `help.rs`
- `model.rs`
- `usage.rs`
- `archive.rs`

Target:
- all overlays share one shell
- centered “sheet” aesthetic
- no thick boxes unless necessary for legibility

### Overlay shell rules
- standard max width
- standard inner padding
- standard title line
- subtle backdrop dimming effect if feasible in cell renderer
- a single quiet border or edge shadow effect, not a full loud frame

### Per-overlay direction
- **Help**: turn into a compact command sheet with stronger grouping and more whitespace
- **Model picker**: make search-first, calm list styling, consistent selection rail
- **Usage**: focus on one strong table and one summary line; avoid ornamental separators
- **Archive**: emphasize title/date/project path hierarchy, de-emphasize grid noise

Implementation:
- consolidate shared spacing/title/border logic in `dialog.rs`
- convert each overlay renderer to use the same title, padding, and footer conventions

---

## 6. Standalone session UI alignment

File:
- `crates/phi-app/src/tui.rs`

This file currently contains hardcoded ANSI choices for message containers:
- `USER_BG`
- `REPL_BG`
- `REPL_ERROR_BG`
- `TOOL_SUCCESS_BG`
- `TOOL_ERROR_BG`

Target:
- same visual language as mux
- fewer colored blocks
- clearer message hierarchy
- less “chat app bubble” feeling

Changes:
1. Replace hardcoded color constants with semantic style helpers.
2. Reduce background fills:
   - user prompts may keep a subtle surface tint
   - tool blocks should rely more on prefixes/labels than large tinted rectangles
3. Standardize headings:
   - assistant prose
   - tool title
   - tool output
   - error output
4. Make tool execution blocks feel like folded logs:
   - title line with command/path
   - lightly indented body
   - only errors get stronger color
5. Align markdown styling with mux tokens.

This is one of the biggest wins: today mux and standalone mode risk feeling like different products.

---

## 7. Shared widget cleanup

Files:
- `crates/phi-tui/src/selector.rs`
- `crates/phi-tui/src/markdown.rs`
- optionally small style helpers in `terminal.rs`

## 7.1 Selector
Current selector already uses a strong selected row with accent rail.

Refine it:
- keep the rail
- reduce cyan/green multiplicity
- use one accent for focus and one semantic success checkmark
- soften selected row background
- dim detail text more consistently

## 7.2 Markdown
Current markdown renderer uses cyan heavily for code/headings/links.

Target:
- headings: mostly weight, not color
- links: underline + mild accent
- inline code: subtle tonal distinction, not bright cyan everywhere
- code fences: calmer syntax theme if current one is too vivid
- block quotes and list structure should rely on indentation and dimness

If needed:
- swap the bundled syntect theme to something lower-contrast and more neutral
- add a tiny theme adapter layer rather than using hardcoded `CYAN`

---

## 8. Concrete file-by-file implementation plan

## Phase 1 — Establish tokens
1. Add semantic token vocabulary to:
   - `crates/phi-app/src/mux/ui/theme.rs`
   - new `crates/phi-tui/src/style.rs`
2. Replace ad-hoc color names in code with semantic names.
3. Keep old palette values temporarily, just routed through new roles.

Deliverable:
- no visible redesign yet
- only style plumbing

## Phase 2 — Quiet the mux chrome
1. `mux/ui/scene.rs`
   - neutral default statusline
   - reduce mode chip weight
   - simplify right-status symbols/colors
2. `mux/ui/sidebar.rs`
   - remove animated gradient text
   - reduce glyph noise
   - simplify selected/current/active styling
3. `mux/ui/layout.rs`
   - slimmer sidebar behavior
   - optional auto-collapse rules

Deliverable:
- mux immediately feels calmer without changing workflow

## Phase 3 — Unify overlays
1. Build a single overlay shell in `overlays/dialog.rs`
2. Port:
   - `help.rs`
   - `model.rs`
   - `usage.rs`
   - `archive.rs`
3. Standardize title, padding, footer, scrollbars, and empty states

Deliverable:
- overlays feel like one family

## Phase 4 — Align standalone TUI
1. Introduce style helpers into `crates/phi-app/src/tui.rs`
2. Replace hardcoded bright/tinted containers
3. Standardize tool block rendering and message hierarchy
4. Align markdown output with shared style tokens

Deliverable:
- standalone mode and mux feel like one app

## Phase 5 — Shared widget refinement
1. `phi-tui/src/selector.rs`
2. `phi-tui/src/markdown.rs`
3. optional polish in `spinner.rs`

Deliverable:
- pickers and markdown adopt the same restrained visual language

## Phase 6 — Final polish pass
1. tune spacing/padding/glyphs
2. audit every visible symbol
3. remove any remaining decorative color usage
4. validate at:
   - 80x24
   - 100x30
   - 120x40
   - very wide terminals

Deliverable:
- production-ready aesthetic consistency

---

## 9. Specific simplifications to make

If you want the shortest path to “beautiful + minimal”, do these first:

### Must-do removals
- remove sidebar animated gradient text
- reduce permanent status bar saturation
- reduce the number of accent colors from 2+ to 1
- stop using large tinted blocks for non-error tool output
- unify overlay framing

### Must-do additions
- semantic design tokens
- auto-collapsing sidebar behavior
- consistent overlay shell
- consistent typography hierarchy
- calmer markdown theme

---

## 10. Acceptance criteria

The overhaul is successful if:

1. A screenshot of mux at rest looks calm even without context.
2. The eye goes to the main pane first, not the chrome.
3. There is no frame where more than one area is “shouting”.
4. Busy, success, warning, and error states are clear without neon.
5. Mux and standalone session mode clearly look like the same product.
6. The UI remains legible in small terminals.
7. Removing color entirely would still leave the hierarchy understandable.

A useful rule:
> if the UI still reads well in grayscale, the structure is probably right.

---

## 11. Recommended implementation order for this repo

Best ROI order:

1. `mux/ui/theme.rs`
2. `mux/ui/scene.rs`
3. `mux/ui/sidebar.rs`
4. `mux/ui/overlays/dialog.rs`
5. `mux/ui/overlays/{help,model,usage,archive}.rs`
6. `phi-app/src/tui.rs`
7. `phi-tui/src/{selector,markdown}.rs`

This gives the fastest visible improvement with the least architectural risk.

---

## 12. Practical note

“Perfectly beautiful” is subjective, so the safest way to ship this is:

- first make the UI **quiet**
- then make it **consistent**
- only then add tiny amounts of personality

Minimalism should come from subtraction, not from inventing a new style language.
