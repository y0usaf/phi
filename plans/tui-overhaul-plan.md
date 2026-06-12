# TUI Overhaul Plan — Beautiful + Minimal

## Goal

Make Phi's TUI feel **calm, premium, minimal, and highly legible** without changing its core interaction model.

This is **not** a renderer rewrite. The existing architecture is strong:
- `phi-tui` already owns the terminal primitives
- mux chrome is centralized in `crates/phi-app/src/mux/ui/*`
- single-session styling is concentrated in `crates/phi-app/src/tui.rs`

The overhaul should therefore focus on:
1. a unified visual language
2. reduction of color/noise/chrome
3. stricter information hierarchy
4. shared design tokens across single-session and mux modes
5. better defaults for spacing, surfaces, states, and overlays

---

## What feels visually busy today

### 1. Two separate visual systems
- `crates/phi-app/src/tui.rs` uses hardcoded ANSI constants and block backgrounds:
  - `USER_BG`
  - `REPL_BG`
  - `REPL_ERROR_BG`
  - `TOOL_SUCCESS_BG`
  - `TOOL_ERROR_BG`
- mux mode uses a separate Charmtone-derived palette in:
  - `crates/phi-app/src/mux/ui/theme.rs`

This makes the app feel assembled from parts rather than designed as one product.

### 2. The mux palette is expressive, but not minimal
`mux/ui/theme.rs` is intentionally colorful:
- bright pinks, purples, citron, cyan accents
- strong status bar color
- multiple accent families

That is attractive in isolation, but it competes with the content. For a coding TUI, the model output and terminal pane should dominate, not the chrome.

### 3. Sidebar rendering has too much ornament
`mux/ui/scene.rs` currently includes:
- animated gradient titles
- animated status colors
- heavy status glyph presence
- explicit scrollbars
- project rules with decorative treatment
- a strong mode chip and statusline

This is polished, but not minimal.

### 4. Overlays are visually framed like tools, not like quiet sheets
The overlay stack uses boxes and decorative title rules:
- `draw_box(...)` in overlay renderers
- `render_dialog_title_line(...)` with title-rule-info treatment

Useful, but still a bit “UI chrome first”.

### 5. Single-session message blocks are visually heavy
The current single-session UI uses container backgrounds for:
- user messages
- repl blocks
- tool success/error blocks

That helps grouping, but large filled blocks are one of the fastest ways to make a terminal UI feel dense.

### 6. Markdown theme is bright and generic
`crates/phi-tui/src/markdown.rs` uses:
- cyan for code/headings/links
- syntect `base16-ocean.dark`

Good default, but not tuned to the rest of Phi.

---

## Design direction

### Core aesthetic
Aim for:
- **near-monochrome default**
- **one accent color**
- **one semantic danger color**
- **very restrained success/warning usage**
- **surfaces distinguished mostly by spacing + contrast, not decoration**
- **content-first layout**
- **clear but quiet focus states**
- **fewer glyphs, fewer borders, fewer backgrounds**

Think:
- calm dark editor
- premium terminal
- linear/Arc-style restraint
- less “dashboard”, more “instrument panel”

---

## New visual principles

### 1. Content is primary
The screen should visually rank like this:

1. assistant / terminal content
2. current input
3. current selection / focus
4. status
5. chrome

If chrome draws more attention than the conversation or terminal, it is too loud.

### 2. Color is semantic, not decorative
Use color only for:
- focus
- selection
- active state
- errors
- links / code emphasis
- minimal success confirmation

Do **not** use color to decorate every region.

### 3. Surfaces over borders
Prefer:
- subtle background separation
- whitespace / padding
- typographic contrast

Over:
- full boxes everywhere
- multicolor rules
- gradient text
- repeated separators

### 4. Motion should be rare
Keep:
- a spinner for active work
- cursor
- maybe one very subtle status pulse

Remove:
- animated gradients
- animated sidebar text color
- ornamental movement

### 5. One design system for both UIs
Single-session and mux mode should feel like the same app.

---

## Target visual system

## Palette

Replace the current colorful mux palette with a restrained token set.

### Proposed token model
Introduce semantic tokens instead of theme-specific named colors:

- `bg_app`
- `bg_panel`
- `bg_panel_alt`
- `bg_overlay`
- `bg_selected`
- `bg_active_subtle`
- `fg_primary`
- `fg_muted`
- `fg_dim`
- `fg_accent`
- `fg_success`
- `fg_warning`
- `fg_error`
- `border_subtle`
- `border_strong`
- `cursor`
- `selection_bg`
- `selection_fg`

### Suggested dark palette direction
Not exact final values, but roughly:

- app background: very dark graphite
- panel background: slightly lifted graphite
- overlay background: same or +1 step
- primary text: warm off-white
- muted text: medium cool gray
- dim text: deeper gray
- accent: one desaturated blue or teal
- error: muted coral/red
- success: muted green, used sparingly
- warning: muted amber, used sparingly

Important:
- eliminate magenta/pink as a primary chrome color
- eliminate dual-accent competition (`ACCENT` + `ACCENT_2`) except where absolutely necessary
- keep ANSI terminal colors, but tune the defaults to the new environment

### Immediate file target
- `crates/phi-app/src/mux/ui/theme.rs`

Refactor it from a “brand palette” to a real semantic theme file.

---

## Layout and chrome changes

## 1. Statusline: reduce to a quiet utility bar
Current statusline is expressive and colorful.

Target:
- same height: 1 row
- flatter background
- no loud chip feeling
- mode label should be subtle, maybe:
  - `NORMAL`
  - `COMMAND`
  - or even just `:` state when command line is open
- right-side status should be terse:
  - `thinking`
  - `tool: bash`
  - `idle`
  - `error`
- color should only intensify for true error state

Changes:
- simplify `statusline_mode_label(...)`
- simplify `statusline_right_text(...)`
- reduce use of glyphs from `statusline_status_symbol(...)`
- make normal bar background panel-like, not a saturated badge

Files:
- `crates/phi-app/src/mux/ui/scene.rs`

### Specific recommendation
Use fewer symbols:
- busy: `•`
- tool: `ƒ` or `>`
- error: `×`
- done/idle: none or dim dot

If text alone is clear enough, remove the symbol entirely.

---

## 2. Sidebar: from animated showcase to quiet navigator
The sidebar should behave like a finder/source list, not a live decorative widget.

### Keep
- projects
- sessions
- current selection
- active/busy indicator
- notification state
- sticky project header if needed

### Remove or tone down
- animated gradient text
- animated project rules
- strong status glyph coloration
- visual complexity in selector branches
- explicit scrollbar unless needed

### Target look
- plain rows
- selected row gets one quiet background fill
- current/active row gets either:
  - a left accent bar, or
  - slightly brighter text
- project headers in uppercase/small-label style via dim/bold, not ornaments
- active spinner stays, but small and monochrome/accented

### Specific code targets
- `render_sidebar(...)`
- `render_sidebar_gradient_text(...)` → remove entirely
- `render_sidebar_project_rule(...)` → simplify into plain heading row
- `render_sidebar_scrollbar(...)` → hide by default or render only when actively scrolling
- `sidebar_status_glyph(...)` in `sidebar.rs` → reduce visual emphasis

Files:
- `crates/phi-app/src/mux/ui/scene.rs`
- `crates/phi-app/src/mux/ui/sidebar.rs`

### Recommendation
Turn the sidebar into:
- one background
- one selected-row fill
- one accent marker column
- muted metadata
- minimal status glyphs

No gradients.

---

## 3. Overlays: make them feel like sheets, not boxes
Current overlays use `draw_box(...)` and title rules.

Target:
- centered sheet/card
- subtle border, or even no full border
- title row with simple text + dim shortcut hint
- body with generous inner padding
- no decorative line flourish

### Overlay style rules
- same width logic can remain
- more padding inside
- quieter title
- one subtle border color if needed
- no heavy corner/box emphasis
- scrollbar only if content exceeds bounds, and very faint

Files:
- `crates/phi-app/src/mux/ui/overlays/dialog.rs`
- `crates/phi-app/src/mux/ui/overlays/help.rs`
- `crates/phi-app/src/mux/ui/overlays/model.rs`
- `crates/phi-app/src/mux/ui/overlays/usage.rs`
- `crates/phi-app/src/mux/ui/overlays/archive.rs`
- `crates/phi-app/src/mux/ui/cell_surface.rs` (`draw_box` usage)

### Recommendation
Keep `draw_box(...)`, but add a “minimal dialog” mode:
- optional border
- optional shadow-ish contrast via surface tint
- simplified title rendering

---

## 4. Terminal pane: let the content breathe
The terminal pane is already the strongest part of the app conceptually.

Target:
- no unnecessary framing around the terminal content
- terminal area should visually merge with the app background or sit on a very slightly raised surface
- cursor and text selection should be crisp and obvious
- scrollbars should be faint or transient

Files:
- `crates/phi-app/src/mux/ui/scene.rs`
- `crates/phi-app/src/mux/ui/terminal_view.rs`
- `crates/phi-app/src/mux/ui/theme.rs`

### Recommendation
- keep selection strong
- make cursor accent-colored but not neon
- keep terminal default bg aligned with app theme
- reduce border usage around the main pane to near zero

---

## 5. Command line: make it feel native to the app
The `:` command line should feel like a quiet input affordance, not a mode alarm.

Current architecture is fine in:
- `crates/phi-app/src/mux/command_line.rs`

Visual target:
- statusline morphs into an input row
- only the `:` and current input matter
- suggestions/help remain elsewhere, not in the row itself
- avoid high-warning yellow unless command mode must be unmistakable

Recommendation:
- command mode background should be only slightly lifted from normal statusline
- use accent caret/colon rather than warning fill for the whole row

Files:
- `crates/phi-app/src/mux/ui/scene.rs`
- `crates/phi-app/src/mux/command_line.rs`

---

## Single-session UI overhaul

## 1. Remove hardcoded visual islands
The single-session TUI currently hardcodes:
- `USER_BG`
- `REPL_BG`
- `REPL_ERROR_BG`
- `TOOL_SUCCESS_BG`
- `TOOL_ERROR_BG`

This should be replaced by shared semantic tokens.

### New rule
Use message grouping by:
- spacing
- prefixes/labels
- subtle left rails
- restrained tint only when semantically necessary

Instead of full-width filled blocks for everything.

### Proposed structure
For user messages:
- dim label like `You`
- plain content on app surface
- maybe subtle left rail in muted/accent

For assistant output:
- no container by default
- markdown styling carries the structure

For tool/repl blocks:
- subtle inset surface
- dim header line
- error state gets muted red tint, not saturated fill
- success should usually not get a success-colored container at all

File:
- `crates/phi-app/src/tui.rs`

---

## 2. Simplify tool block presentation
Tool call rendering is a big opportunity.

Current direction appears inspired by chat block UIs. For minimalism, better is:
- one compact title line
- optional collapsed preview
- plain monospace block body
- one subtle divider before/after if necessary

### Good structure
- `bash ls -la`
- result preview
- optional dim metadata

Not:
- large colored chunk with nested color semantics unless error

Recommendation:
- neutral background for all tool blocks
- only failure gets red-tinted border/rail/header
- remove green success backgrounds

File:
- `crates/phi-app/src/tui.rs`

---

## 3. Markdown theme should match the app
`crates/phi-tui/src/markdown.rs` currently uses:
- cyan for headings/code/links
- `base16-ocean.dark`

Target:
- headings: primary text, maybe bold, maybe accent only for H1/H2
- inline code: subtle raised background or softened accent, not bright cyan
- links: underline + accent
- code fences: syntax theme that matches the app palette
- rules and quotes: dim gray, not decorative

### Recommendation
Introduce a Phi markdown theme:
- replace global cyan-heavy styling
- choose a syntect theme closer to the app palette or ship a custom one

Files:
- `crates/phi-tui/src/markdown.rs`

---

## 4. Selector and picker components should inherit the new system
`crates/phi-tui/src/selector.rs` currently uses:
- cyan
- green
- selected background via ANSI 236
- bold/dim hardcoded ANSI

This should be tokenized.

Target:
- selected row = subtle fill + accent marker
- current item checkmark = muted accent
- query prompt = same command/input styling as app
- no green/cyan assumptions

Files:
- `crates/phi-tui/src/selector.rs`
- optionally expose a `Theme` or style config from `phi-tui`

---

## 5. Spinner should be quieter
The spinner itself is fine structurally:
- `crates/phi-tui/src/spinner.rs`

But visually it should be:
- dim/accented
- used only where needed
- never the most colorful element on screen

No code change may be needed beyond styling where consumed.

---

## Shared design system refactor

## Introduce a single TUI theme layer
Right now, style values are scattered:
- mux theme in `mux/ui/theme.rs`
- ANSI constants in `tui.rs`
- markdown styles in `phi-tui/src/markdown.rs`
- selector styles in `phi-tui/src/selector.rs`

Create a shared theme module so the app has one visual vocabulary.

### Proposed structure
Possible new files:
- `crates/phi-tui/src/theme.rs`
- or `crates/phi-app/src/ui_theme.rs` plus adapters for `phi-tui`

Best long-term: put reusable semantic style tokens in `phi-tui`, because both mux and single-session consume its components.

### Needed abstractions
A small style API:
- palette tokens
- emphasis styles
- surface roles
- maybe helper methods for ANSI conversion

Do not overengineer it into CSS-in-Rust. A simple semantic token struct is enough.

---

## Minimal component spec

## Surfaces
- App canvas: darkest
- Panels: +1 brightness step
- Raised/selected: +1 more step
- Errors: low-saturation tinted panel
- Success: usually text-only, not container-only

## Typography
- Primary: off-white
- Muted: mid gray
- Dim/meta: darker gray
- Bold: only for headings, titles, active focus
- Underline: only for links / strong interaction hint

## Borders
- Default: none
- When needed: one subtle border color
- Avoid double framing

## Selection
- Strong enough to be unmistakable
- Keep terminal selection distinct from sidebar/list selection

## Accent
- Exactly one accent family
- Use for focus, cursor, links, selected marker, active command prompt

## Error
- Muted red/coral
- Only use saturation on actual failure

---

## Concrete implementation phases

## Phase 1 — Unify theme tokens
Goal: eliminate ad hoc color decisions.

### Tasks
1. Refactor `crates/phi-app/src/mux/ui/theme.rs` into semantic tokens.
2. Add equivalent shared tokens for:
   - single-session TUI
   - markdown
   - selector
3. Remove hardcoded color assumptions in:
   - `crates/phi-app/src/tui.rs`
   - `crates/phi-tui/src/selector.rs`
   - `crates/phi-tui/src/markdown.rs`

### Deliverable
One palette, one accent, one error color, shared surfaces.

---

## Phase 2 — Simplify mux chrome
Goal: remove decorative complexity.

### Tasks
1. Simplify statusline in `scene.rs`
2. Remove gradient text and animated sidebar coloring
3. Simplify project headers and session rows
4. Tone down or hide scrollbars
5. Make overlays feel like sheets

### Deliverable
Mux UI becomes quiet, flat, and content-first.

---

## Phase 3 — Simplify single-session conversation rendering
Goal: make the chat/agent experience elegant.

### Tasks
1. Replace filled message containers with lighter grouping
2. Standardize tool block appearance
3. Tone down success/error block backgrounds
4. Improve spacing between logical sections
5. Align labels, rails, and metadata styles to the new theme

### Deliverable
Single-session TUI looks premium and calm, not block-heavy.

---

## Phase 4 — Tune markdown and embedded widgets
Goal: remove the last mismatched visuals.

### Tasks
1. Redesign markdown color rules
2. Swap syntax theme
3. Restyle selector
4. Audit spinner placement and status emphasis

### Deliverable
Everything inside the app feels like one system.

---

## Phase 5 — Add visual regression protection
Goal: keep the design coherent.

### Recommended additions
1. Snapshot/golden tests for rendered rows where feasible
2. Theme unit tests for token mapping
3. Manual review checklist for:
   - low-width terminals
   - high scrollback
   - selection states
   - command mode
   - overlays
   - error states
4. Optional dev command to preview components

Possible locations:
- `crates/phi-tui/tests/`
- mux UI tests near render helpers

---

## File-by-file action map

## Highest-priority files
- `crates/phi-app/src/mux/ui/theme.rs`
- `crates/phi-app/src/mux/ui/scene.rs`
- `crates/phi-app/src/mux/ui/sidebar.rs`
- `crates/phi-app/src/mux/ui/overlays/dialog.rs`
- `crates/phi-app/src/tui.rs`
- `crates/phi-tui/src/markdown.rs`
- `crates/phi-tui/src/selector.rs`

## Secondary files
- `crates/phi-app/src/mux/ui/cell_surface.rs`
- `crates/phi-app/src/mux/ui/terminal_view.rs`
- `crates/phi-tui/src/spinner.rs`
- `crates/phi-app/src/mux/command_line.rs`

---

## Opinionated visual decisions I recommend

1. **Kill gradients**
   - remove sidebar gradient text
   - remove animated color blending

2. **Reduce borders by at least 70%**
   - use surface contrast instead

3. **One accent only**
   - blue or teal, not pink + purple + cyan all together

4. **No green success panels**
   - green belongs in tiny confirmations, not large blocks

5. **Error tint only on actual failure**
   - make red meaningful

6. **Project headers should be typographic**
   - dim uppercase or spaced label
   - no ornamental rules unless extremely subtle

7. **Selected row should be a quiet fill**
   - not a lightshow

8. **Statusline should almost disappear when idle**
   - if nothing is happening, the bar should be visually quiet

9. **Assistant content should be mostly unboxed**
   - let the text breathe

10. **Tool output should look like disciplined logs**
   - not chat bubbles

---

## Suggested first implementation slice

If you want the highest impact with the least code churn, do this order:

### Slice A — Theme + sidebar
- simplify `mux/ui/theme.rs`
- remove sidebar gradients/animation from `scene.rs`
- reduce statusline saturation

This alone will make the app feel much more minimal.

### Slice B — Single-session block cleanup
- replace hardcoded block backgrounds in `tui.rs`
- neutralize success states
- keep only muted error tint

This will make the conversation view much calmer.

### Slice C — Markdown + selector
- restyle `markdown.rs`
- restyle `selector.rs`

This removes the last obvious style mismatch.

---

## Definition of done

The overhaul is successful when:

- mux mode and single-session mode clearly look like the same app
- the eye goes first to content, not chrome
- busy states are legible without becoming flashy
- overlays feel lightweight
- colors are sparse and meaningful
- there are no gradients, ornamental status theatrics, or loud success blocks
- the app feels deliberate, premium, and calm even in a dense terminal session

---

## Short version

This should be a **design-system simplification**, not a rewrite:
- unify theme tokens
- remove decorative color and animation
- flatten chrome
- simplify sidebar
- unbox content
- make overlays quieter
- retune markdown and selectors
- preserve the current rendering architecture

That will get Phi to “beautiful and minimal” much faster than rebuilding the TUI stack.
