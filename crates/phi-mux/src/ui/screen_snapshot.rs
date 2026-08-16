//! Lightweight snapshot of a vt100 screen's visible cells and metadata.
//!
//! Built under the parser lock in [`MuxApp::draw`], then used for compositing
//! after the lock is released, so the session's `ParserWriter` never blocks
//! waiting for the mux to finish rendering (Fix 5 from the renderer diagnosis).

/// A snapshot of the visible portion of a vt100 terminal screen.
pub struct ScreenSnapshot {
    /// Visible screen dimensions: (rows, cols).
    pub size: (u16, u16),
    /// Cursor position: (row, col).
    pub cursor_position: (u16, u16),
    pub hide_cursor: bool,
    pub alternate_screen: bool,
    /// Current scrollback offset (0 = bottom/normal view).
    pub scrollback: usize,
    /// Total scrollback lines available (offset + visible rows when scrolled up).
    pub scrollback_total: usize,
    /// Row-major visible cells (size.0 * size.1 entries). `None` for cells
    /// the vt100 grid didn't have (rare; treated as blank by the compositor).
    pub cells: Vec<Option<vt100::Cell>>,
}

impl ScreenSnapshot {
    /// Build a snapshot from a vt100 screen, capturing only the visible
    /// region — not the full scrollback buffer — plus cursor/mode metadata
    /// and the total scrollback count.
    pub fn from_screen(screen: &vt100::Screen) -> Self {
        let (rows, cols) = screen.size();
        let cursor_position = screen.cursor_position();
        let hide_cursor = screen.hide_cursor();
        let alternate_screen = screen.alternate_screen();
        let scrollback_offset = screen.scrollback();
        let size = (rows, cols);

        // Copy only the visible cells (this is the fast part — scrollback
        // history is not cloned).
        let mut cells = Vec::with_capacity(usize::from(rows) * usize::from(cols));
        for row in 0..rows {
            for col in 0..cols {
                cells.push(screen.cell(row, col).cloned());
            }
        }

        // scrollback_total is set to the offset as a placeholder; the caller
        // (MuxApp::draw) computes the true total under the lock and updates
        // this field.
        let scrollback_total = scrollback_offset;

        Self {
            size,
            cursor_position,
            hide_cursor,
            alternate_screen,
            scrollback: scrollback_offset,
            scrollback_total,
            cells,
        }
    }

    /// Access a cell at `(row, col)` in the visible region.
    pub fn cell(&self, row: u16, col: u16) -> Option<&vt100::Cell> {
        let (rows, cols) = self.size;
        if row >= rows || col >= cols {
            return None;
        }
        self.cells
            .get(usize::from(row) * usize::from(cols) + usize::from(col))
            .and_then(|c| c.as_ref())
    }

    pub fn size(&self) -> (u16, u16) {
        self.size
    }

    pub fn cursor_position(&self) -> (u16, u16) {
        self.cursor_position
    }

    pub fn hide_cursor(&self) -> bool {
        self.hide_cursor
    }

    pub fn alternate_screen(&self) -> bool {
        self.alternate_screen
    }

    pub fn scrollback(&self) -> usize {
        self.scrollback
    }
}
