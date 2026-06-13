//! Frame layout (port of pi-harness `app::layout`): left sidebar, terminal
//! pane, and a one-row statusline at the bottom.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellRect {
    pub col: i32,
    pub row: i32,
    pub cols: i32,
    pub rows: i32,
}

impl CellRect {
    pub fn new(col: i32, row: i32, cols: i32, rows: i32) -> Self {
        Self {
            col,
            row,
            cols,
            rows,
        }
    }

    pub fn inset_edges(self, left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            col: self.col + left,
            row: self.row + top,
            cols: (self.cols - left - right).max(0),
            rows: (self.rows - top - bottom).max(0),
        }
    }

    pub fn contains_cell(self, col: i32, row: i32) -> bool {
        col >= self.col
            && row >= self.row
            && col < self.col + self.cols
            && row < self.row + self.rows
    }
}

const STATUSBAR_ROWS: i32 = 1;
const SIDEBAR_SHOW_MIN_COLS: i32 = 64;
const MIN_TERMINAL_COLS_WITH_SIDEBAR: i32 = 32;
pub const SIDEBAR_WIDTH_DEFAULT: u16 = 36;
/// Sidebar never drops below this many columns once shown.
const SIDEBAR_MIN_COLS: i32 = 24;
/// On wide screens the sidebar tops out at roughly a quarter of the frame.
const SIDEBAR_MAX_FRACTION_DENOM: i32 = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellLayout {
    pub sidebar: CellRect,
    pub terminal: CellRect,
    pub statusbar: CellRect,
}

pub fn compute_cell_layout(cols: u16, rows: u16, sidebar_width: u16) -> CellLayout {
    let cols = i32::from(cols).max(1);
    let rows = i32::from(rows).max(1);

    let statusbar_rows = STATUSBAR_ROWS.min(rows);
    let body_rows = (rows - statusbar_rows).max(0);

    let show_sidebar = cols >= SIDEBAR_SHOW_MIN_COLS && body_rows >= 4;
    let sidebar_cols = if show_sidebar {
        // Scale with the frame instead of disappearing abruptly: narrow
        // screens get a slimmer sidebar, wide screens cap it at the
        // configured width or a quarter of the frame, whichever is smaller.
        let cap = (cols / SIDEBAR_MAX_FRACTION_DENOM).max(SIDEBAR_MIN_COLS);
        i32::from(sidebar_width)
            .min(cap)
            .min((cols - MIN_TERMINAL_COLS_WITH_SIDEBAR).max(0))
            .max(0)
    } else {
        0
    };

    let sidebar = if sidebar_cols > 0 {
        CellRect::new(0, 0, sidebar_cols, body_rows)
    } else {
        CellRect::default()
    };
    let terminal = CellRect::new(sidebar_cols, 0, (cols - sidebar_cols).max(0), body_rows);
    let statusbar = CellRect::new(0, rows - statusbar_rows, cols, statusbar_rows);

    CellLayout {
        sidebar,
        terminal,
        statusbar,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_sidebar_when_compact() {
        let layout = compute_cell_layout(60, 20, SIDEBAR_WIDTH_DEFAULT);
        assert_eq!(layout.sidebar, CellRect::default());
        assert_eq!(layout.statusbar, CellRect::new(0, 19, 60, 1));
        assert_eq!(layout.terminal, CellRect::new(0, 0, 60, 19));
    }

    #[test]
    fn shows_sidebar_on_wide_grid() {
        // Sidebar scales with the frame: a quarter of 120 cols.
        let layout = compute_cell_layout(120, 40, SIDEBAR_WIDTH_DEFAULT);
        assert_eq!(layout.sidebar, CellRect::new(0, 0, 30, 39));
        assert_eq!(layout.terminal, CellRect::new(30, 0, 90, 39));
        assert_eq!(layout.statusbar, CellRect::new(0, 39, 120, 1));
    }

    #[test]
    fn sidebar_caps_at_configured_width_on_very_wide_grid() {
        let layout = compute_cell_layout(200, 40, SIDEBAR_WIDTH_DEFAULT);
        assert_eq!(layout.sidebar, CellRect::new(0, 0, 36, 39));
    }

    #[test]
    fn sidebar_slims_down_instead_of_vanishing_on_medium_grid() {
        let layout = compute_cell_layout(70, 30, SIDEBAR_WIDTH_DEFAULT);
        assert_eq!(layout.sidebar.cols, 24);
        assert!(layout.terminal.cols >= 32);
    }
}
