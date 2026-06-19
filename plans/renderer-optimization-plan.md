# Implementation Plan: Porting Claude-Code Renderer Techniques to Phi

## 1. Strategy Overview

To maximize phi's rendering performance and reduce PTY bandwidth, we must transition from a naive, imperative full-grid diff to a modern, cache-friendly compiler pipeline. The strategy prioritizes foundational data structure transformations first (inturning and packed cells), as they unlock algorithmic improvements and drastically reduce heap allocations. This enables spatial damage tracking and an IR-based patch optimization pipeline, culminating in semantic scrollback modeling. Wide-character compensation is deferred as a low-value, high-risk band-aid that modern terminals no longer need.

---

## 2. Techniques in Priority Order

### Priority 1: Interning / Packed Cells

**What it does & why it matters for phi:**
Currently, phi's `CellSurface` uses `Cell { text: String, ... }`, causing 1,920+ heap allocations per frame for an 80x24 terminal. By interning text (returning a `u32` handle) and packing styles into integers, we reduce each cell to 16 bytes of pure stack data with zero per-cell heap allocations. This transforms the O(N) diff loop from a pointer-chasing, `String`-dereferencing nightmare into an insanely fast, SIMD-friendly linear scan of contiguous `u64` comparisons. 

**Implementation Difficulty:** Low to Medium

**Concrete Rust Types:**
*CRITICAL: Do not pack style booleans into the `Color`'s bits. Truecolor ARGB uses `0xFFaabbcc` and ANSI uses `0x01000000 | index`. Stealing bits corrupts colors. Use a dedicated `u32` for style flags.*

```rust
// crates/phi-mux/src/packed.rs

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
#[repr(C)]
pub struct PackedStyle {
    pub fg: u32,
    pub bg: u32,
    pub flags: u32, // Only lowest 4 bits used
}

impl PackedStyle {
    const BOLD_MASK: u32 = 1 << 0;
    const UNDERLINE_MASK: u32 = 1 << 1;
    const REVERSE_MASK: u32 = 1 << 2;
    const CONTINUATION_MASK: u32 = 1 << 3;

    pub fn new(fg: Color, bg: Color, bold: bool, underline: bool, reverse: bool, continuation: bool) -> Self {
        let mut flags = 0u32;
        if bold        { flags |= Self::BOLD_MASK; }
        if underline   { flags |= Self::UNDERLINE_MASK; }
        if reverse     { flags |= Self::REVERSE_MASK; }
        if continuation{ flags |= Self::CONTINUATION_MASK; }
        Self { fg: fg.0, bg: bg.0, flags }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct PackedText(pub u32);

impl PackedText {
    const INTERNED_FLAG: u32 = 0x80000000;
    const MASK: u32 = 0x7FFFFFFF;

    pub fn ascii(byte: u8) -> Self { Self(byte as u32) }
    pub fn interned(index: u32) -> Self { Self(index | Self::INTERNED_FLAG) }
    pub fn is_interned(&self) -> bool { (self.0 & Self::INTERNED_FLAG) != 0 }
    pub fn index(&self) -> usize { (self.0 & Self::MASK) as usize }
    pub fn ascii_byte(&self) -> u8 { (self.0 & Self::MASK) as u8 }
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct PackedCell {
    pub style: PackedStyle,
    pub text: PackedText,
}

pub struct StringInterner {
    map: HashMap<String, u32>,
    strings: Vec<String>,
}

impl StringInterner {
    pub fn get_or_intern(&mut self, text: &str) -> PackedText {
        if text.len() == 1 && text.is_ascii() {
            return PackedText::ascii(text.as_bytes()[0]);
        }
        if let Some(&idx) = self.map.get(text) {
            return PackedText::interned(idx);
        }
        let idx = self.strings.len() as u32;
        self.strings.push(text.to_string());
        self.map.insert(text.to_string(), idx);
        PackedText::interned(idx)
    }

    pub fn resolve(&self, text: PackedText) -> &str {
        if text.is_interned() {
            &self.strings[text.index()]
        } else {
            std::str::from_utf8(std::slice::from_ref(&text.ascii_byte())).unwrap()
        }
    }
}

pub struct CellSurface {
    pub cols: i32,
    pub rows: i32,
    pub cells: Vec<PackedCell>, // Flat array, 16 bytes per cell
    pub interner: StringInterner,
}
```

**Risks & What to Avoid:**
- **Avoid Global Singletons:** Keep `StringInterner` owned by `CellSurface`. Global interners cause lock contention and make testing painful.
- **Unbounded Growth:** If streaming random unicode noise, the `Vec<String>` can grow forever. Add a circuit breaker: if `interner.strings.len() > 50_000`, fall back to allocating strings in a side-table.
- **Generational Pool Complexity:** JS implementations use generational pools for GC. In Rust, an append-only `Vec<String>` is O(1) and safe. Do not build complex lifetime arenas.

---

### Priority 2: Damage Tracking

**What it does & why it matters for phi:**
Instead of recomposing the entire scene and running a full structural diff over all 1,920 cells every frame, damage tracking introduces spatial invalidation. Drawing primitives (`fill_rect`, `set_cell`) automatically update a `DirtyRegion`. The diff loop then skips clean regions entirely, dropping complexity from O(Cols * Rows) to O(DirtyCells).

**Implementation Difficulty:** Medium

**Concrete Rust Types:**

```rust
// crates/phi-mux/src/damage.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn intersection(&self, other: &Rect) -> Option<Rect> {
        let x1 = self.x.max(other.x);
        let y1 = self.y.max(other.y);
        let x2 = (self.x + self.width).min(other.x + other.width);
        let y2 = (self.y + self.height).min(other.y + other.height);

        if x2 > x1 && y2 > y1 {
            Some(Rect { x: x1, y: y1, width: x2 - x1, height: y2 - y1 })
        } else {
            None
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct DirtyRegion {
    rects: Vec<Rect>,
}

impl DirtyRegion {
    pub fn iter_indices(&self, cols: i32, rows: i32) -> impl Iterator<Item = usize> + '_ {
        let cols = cols as usize;
        let bounds = Rect { x: 0, y: 0, width: cols as i32, height: rows as i32 };
        self.rects.iter()
            .filter_map(move |r| r.intersection(&bounds))
            .flat_map(move |r| {
                let start_x = r.x as usize;
                let start_y = r.y as usize;
                let end_x = (r.x + r.width) as usize;
                let end_y = (r.y + r.height) as usize;
                (start_y..end_y).flat_map(move |y| {
                    (start_x..end_x).map(move |x| y * cols + x)
                })
            })
    }
}

pub struct CellSurface {
    // ... existing fields ...
    pub dirty_region: DirtyRegion,
    pub dirty_all: bool,
}

impl CellSurface {
    pub fn take_dirty_indices(&mut self) -> Option<Vec<usize>> {
        if self.dirty_all {
            self.dirty_all = false;
            self.dirty_region.clear();
            return None; // None signals full repaint
        }
        if self.dirty_region.is_empty() {
            return Some(Vec::new());
        }
        let indices: Vec<usize> = self.dirty_region.iter_indices(self.cols, self.rows).collect();
        self.dirty_region.clear();
        Some(indices)
    }

    pub fn set_cell_styled(&mut self, col: i32, row: i32, cell: PackedCell) {
        // ... existing write logic ...
        self.dirty_region.add(Rect { x: col, y: row, width: 1, height: 1 });
    }
}
```

**Risks & What to Avoid:**
- **Forgetting Semantic Dirty Flags:** If the app updates a clock in the statusbar but forgets to pass `statusbar_dirty = true` to `render_scene`, the UI will freeze. The imperative drawer must be synchronized with state mutations.
- **Mouse Selection Artifacts:** If `for_each_terminal_screen_cell` naively marks only the *new* selection rect dirty, the de-selection of *old* cells won't be tracked. You must explicitly invalidate the union of the previous and new selection rects.

---

### Priority 3: Patch Optimization

**What it does & why it matters for phi:**
Transforms the renderer from directly writing escape sequences into a multi-pass compiler pipeline. `render_diff` emits an intermediate representation (IR) of `Patch`es. An optimizer pass analyzes the `Vec<Patch>` to merge runs, use relative cursor moves, and eliminate redundant SGR style resets before final codegen. This cuts PTY traffic by 40-60%.

**Implementation Difficulty:** Medium

**Concrete Rust Types:**

```rust
// crates/phi-mux/src/ansi.rs

pub enum Patch {
    Move { row: u16, col: u16 },
    Text { string: String, width: u16 },
    Style(Style),
    Newline,
}

pub struct AnsiCodegen {
    pub current_style: Style,
    pub cur_row: u16,
    pub cur_col: u16,
    pub buf: String,
}

impl AnsiCodegen {
    /// Only emits the differential style attributes
    pub fn apply_style(&mut self, target: Style) {
        let cur = self.current_style;
        if target == cur { return; }
        
        // Safe reset if complex attributes are turning off
        if (cur.bold || cur.reverse || cur.underline) 
           != (target.bold || target.reverse || target.underline) 
           && (cur.bold || cur.reverse || cur.underline) {
            self.buf.push_str("\x1b[0m");
            self.current_style = Style::default();
        }
        // ... diff fg/bg/bold against current and append to buf ...
        self.current_style = target;
    }

    pub fn emit_move(&mut self, row: u16, col: u16) {
        if self.cur_row == row && col > self.cur_col {
            let diff = col - self.cur_col;
            write!(self.buf, "\x1b[{}C", diff).ok(); // Relative move
        } else {
            write!(self.buf, "\x1b[{};{}H", row, col).ok(); // Absolute move
        }
        self.cur_row = row;
        self.cur_col = col;
    }
}

pub struct PatchOptimizer {
    patches: Vec<Patch>,
}

impl PatchOptimizer {
    pub fn optimize(mut self) -> Vec<Patch> {
        self.pass_merge_adjacent_text();
        self.pass_skip_redundant_styles();
        self.patches
    }

    fn pass_merge_adjacent_text(&mut self) {
        let mut out = Vec::with_capacity(self.patches.len());
        for patch in self.patches.drain(..) {
            match (patch, out.last_mut()) {
                (Patch::Text { string, width }, Some(Patch::Text { string: prev_str, width: prev_w })) => {
                    prev_str.push_str(&string);
                    *prev_w += width;
                },
                (p, _) => out.push(p),
            }
        }
        self.patches = out;
    }
    
    fn pass_skip_redundant_styles(&mut self) {
        let mut out = Vec::with_capacity(self.patches.len());
        let mut cur_style = Style::default();
        for patch in self.patches.drain(..) {
            if let Patch::Style(target) = &patch {
                if *target == cur_style { continue; }
                cur_style = *target;
            }
            out.push(patch);
        }
        self.patches = out;
    }
}
```

**Risks & What to Avoid:**
- **The "Blank Intervening Cells" Trap:** Never emit spaces to bridge two dirty cells just to merge text runs. This overwrites wide-character placeholders and background colors. Only merge if cells are actually empty.
- **Relative Cursor Wrapping:** If `cur_col + diff > cols`, relative moves (`\x1b[5C`) may wrap to the next line in some terminals. Always force an absolute `Move` if the calculation exceeds the column bounds.

---

### Priority 4: Scrollback Modeling

**What it does & why it matters for phi:**
Detects semantic viewport scrolls rather than treating them as O(W*H) content changes. When a newline prints at the bottom of the screen, the `vt100` screen shifts rows up. Instead of re-diffing 1,840 cells, this detects the uniform shift, emits a `\x1b[NS` (Scroll Up) escape sequence, and only repaints the bottom row. 

**Implementation Difficulty:** Medium

**Concrete Rust Types:**

```rust
use ahash::AHasher;
use std::hash::Hasher;

// In ansi.rs
pub struct AnsiRenderer {
    pub previous: Option<CellSurface>,
    pub prev_row_hashes: Option<Vec<u64>>, // NEW: Cache of row hashes
    pub scratch: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollDirective {
    None,
    Up(u32),
    Down(u32),
}

impl AnsiRenderer {
    fn detect_scroll(
        &self,
        prev: &CellSurface,
        curr: &CellSurface,
        prev_hashes: &[u64],
        curr_hashes: &[u64],
    ) -> (ScrollDirective, usize) {
        let rows = curr.rows as usize;
        
        // Try matching top of curr to lower part of prev (Scroll Up)
        // Limit search depth to 20 to prevent O(N^2) worst case
        for n in 1..rows.min(20) { 
            let curr_slice = &curr_hashes[..rows - n];
            let prev_slice = &prev_hashes[n..];
            
            if curr_slice == prev_slice {
                return (ScrollDirective::Up(n as u32), rows - n);
            }
        }
        
        (ScrollDirective::None, 0)
    }

    fn hash_row(cells: &[PackedCell], interner: &StringInterner) -> u64 {
        let mut hasher = AHasher::default();
        for cell in cells {
            hasher.write(interner.resolve(cell.text).as_bytes());
            hasher.write_u32(cell.style.fg);
            hasher.write_u32(cell.style.bg);
            hasher.write_u32(cell.style.flags);
        }
        hasher.finish()
    }
}
```

**Risks & What to Avoid:**
- **Unbounded O(N^2) Matching:** A completely blank terminal screen will trigger false-positive scrolls everywhere. Limit the search depth (e.g., `1..rows.min(20)`) and require the shifted slice to cover the entire remaining viewport.
- **Heap Allocating Strings for Hashing:** Hash rows directly from the flat `Vec<PackedCell>` slice using `hasher.write(interner.resolve(cell.text).as_bytes())`. Do not allocate temporary `String` objects for comparisons.
- **Alt-Screen Transitions:** If `vt100::Parser` invokes an alt-screen switch, drop `prev_row_hashes` and force a full repaint to prevent scrolling the alt-screen into primary history.

---

### Priority 5: Wide-Char WCwidth Compensation (Low Priority)

**What it does & why it matters for phi:**
A synchronization layer for when phi's logical character width disagrees with the host terminal's physical width (e.g., outdated emoji tables). It forces cursor alignment using CHA (`\x1b[G`) after wide characters. 

**Implementation Difficulty:** High | **Value:** Low

**Concrete Rust Types:**

```rust
use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostWidthProfile {
    Standard,      // Trusts unicode_width implicitly (modern VTE, Kitty, WezTerm)
    NarrowEmoji,   // Older xterm, Apple Terminal
    StrictNarrow,  // Very outdated environments
}

impl HostWidthProfile {
    pub fn host_width(&self, ch: char) -> u16 {
        let logical_w = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
        match self {
            HostWidthProfile::Standard => logical_w,
            HostWidthProfile::NarrowEmoji => {
                if (ch >= '\u{1F000}' && ch <= '\u{1FAFF}') || (ch >= '\u{E000}' && ch <= '\u{F8FF}') {
                    1
                } else { logical_w }
            }
            HostWidthProfile::StrictNarrow => if ch >= '\u{1F000}' { 1 } else { logical_w },
        }
    }
}

impl AnsiRenderer {
    fn write_run_with_compensation(&mut self, run: &[&PackedCell], target_col: i32, row: i32, out: &mut String) {
        out.push_str(&format!("\x1b[{};{}H", row + 1, target_col + 1));
        let mut write_cursor_col = target_col;

        for cell in run {
            let text = self.interner.resolve(cell.text);
            out.push_str(text);
            
            let logical_w = if cell.style.flags & PackedStyle::CONTINUATION_MASK != 0 { 0 } 
                else { text.chars().map(|c| UnicodeWidthChar::width(c).unwrap_or(1)).sum::<usize>() as i32 };
            write_cursor_col += logical_w;

            let physical_w = text.chars().map(|c| self.host_profile.host_width(c) as i32).sum::<i32>();

            if physical_w != logical_w {
                out.push_str(&format!("\x1b[{}G", write_cursor_col + 1));
            }
        }
    }
}
```

**Risks & What to Avoid:**
- **YAGNI:** Do not implement this unless specific users report misalignment. Modern terminals have standardized `wcwidth` tables. The performance overhead of heap allocations will bottleneck rendering long before wide-char misalignment does.
- **Unconditional CHA Emission:** Never overuse CHA (`\x1b[G`). It creates massive ANSI payload bloat. Only emit when `physical_w != logical_w`.
- **Avoid Modifying vt10::Screen width:** The terminal emulation grid must remain pristine. Compensation exists strictly at the `AnsiRenderer` presentation boundary.

---

## 3. Implementation Order & Dependencies

1. **Phase 1: Interning / Packed Cells.** Absolutely everything depends on this. The `PackedCell` struct reduces memory bandwidth and enables fast hashing later. Complete this and update `fill_rect` and `set_cell` before touching the diff layer.
2. **Phase 2: Damage Tracking.** With fast diffing enabled, weave `DirtyRegion` into `CellSurface`'s drawing primitives. Update `AnsiRenderer::render` to accept `Option<Vec<usize>>` of dirty indices.
3. **Phase 3: Patch Optimization.** Refactor `render_diff` to populate `Vec<Patch>` instead of writing to `self.scratch` directly. Implement `PatchOptimizer` and `AnsiCodegen`.
4. **Phase 4: Scrollback Modeling.** With the `Patch` IR in place, add row hashing to `AnsiRenderer` to inject `ScrollDirective` patches at the top of the stream.
5. **Phase 5: Wide-Char Compensation.** Dormant/Deferred. Wire `HostWidthProfile` into `AnsiCodegen` only if terminal misalignment reports occur.

---

## 4. Cross-Cutting Concerns

### Memory vs. Compute Tradeoffs
The `StringInterner` turns per-cell `String` allocations into a single O(1) lookup, but the `Vec<String>` can grow unbounded if streaming random unicode noise. The circuit breaker (`len() > 50_000`) ensures memory is capped at ~2MB, but you must gracefully handle the fallback path during rendering.

### State Coupling
- **Damage Tracking + Scrollback:** If `AnsiRenderer` detects a `ScrollDirective::Up(1)`, it must *not* generate patches for the cells that logically scrolled. The dirty indices produced by `CellSurface` will include all 1,840 shifted cells. The scroll detection must suppress the dirty patches for the moved region and only emit patches for the newly exposed bottom row. 
- **Selection Overlays + Damage:** Mouse selections directly mutate `CellSurface`. You must explicitly mark the *union* of the previous and new selection rects dirty, or visual artifacts will remain when de-selecting.

### Terminal Assumptions
The patch optimizer assumes standard VT100 behavior for relative cursor moves (`\x1b[NC`). If the terminal is at column 80 and receives a `\x1b[5C` command, some terminals wrap to the next line while others clamp at the margin. Always force an absolute `Move` if `cur_col + diff > cols`.