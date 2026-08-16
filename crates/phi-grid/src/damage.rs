//! Spatial damage tracking (Priority 2 of the renderer optimization plan).
//! Drawing primitives mark a `DirtyRegion`; the diff loop can then skip clean
//! regions entirely, dropping complexity from O(Cols * Rows) to O(DirtyCells).

use crate::layout::CellRect;

/// A collection of dirty rectangles. Reuses `layout::CellRect` as the rect
/// type (the single 4-i32 cell-rect in the grid, coalesced from the former
/// `damage::Rect`).
#[derive(Debug, Default, Clone)]
pub struct DirtyRegion {
    rects: Vec<CellRect>,
}

impl DirtyRegion {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    pub fn clear(&mut self) {
        self.rects.clear();
    }

    pub fn add(&mut self, rect: CellRect) {
        if rect.cols <= 0 || rect.rows <= 0 {
            return;
        }
        // Simple, fast union: coalesce with the last rect if they overlap or
        // are directly adjacent on the same row band. This keeps the rect
        // count bounded without a full sweep-line merge.
        if let Some(last) = self.rects.last_mut() {
            // Same single row, horizontally adjacent/overlapping → extend.
            if last.row == rect.row
                && last.rows == rect.rows
                && rect.col <= last.col + last.cols
                && last.col <= rect.col + rect.cols
            {
                let new_right = (last.col + last.cols).max(rect.col + rect.cols);
                last.col = last.col.min(rect.col);
                last.cols = new_right - last.col;
                return;
            }
            // Same columns, vertically adjacent/overlapping → extend.
            if last.col == rect.col
                && last.cols == rect.cols
                && rect.row <= last.row + last.rows
                && last.row <= rect.row + rect.rows
            {
                let new_bottom = (last.row + last.rows).max(rect.row + rect.rows);
                last.row = last.row.min(rect.row);
                last.rows = new_bottom - last.row;
                return;
            }
        }
        self.rects.push(rect);
    }

    /// Mark the entire region dirty by pushing a full-screen rect.
    pub fn add_full(&mut self, cols: i32, rows: i32) {
        self.rects.clear();
        self.rects.push(CellRect::new(0, 0, cols, rows));
    }

    pub fn add_rect(&mut self, col: i32, row: i32, cols: i32, rows: i32) {
        self.add(CellRect::new(col, row, cols, rows));
    }

    /// Iterate flat row-major cell indices covered by the dirty rects,
    /// clamped to `(cols, rows)`.
    pub fn iter_indices(&self, cols: i32, rows: i32) -> impl Iterator<Item = usize> + '_ {
        let cols_us = cols as usize;
        let bounds = CellRect::new(0, 0, cols, rows);
        self.rects
            .iter()
            .filter_map(move |r| r.intersection(&bounds))
            .flat_map(move |r| {
                let start_col = r.col.max(0) as usize;
                let start_row = r.row.max(0) as usize;
                let end_col = (r.col + r.cols).max(0) as usize;
                let end_row = (r.row + r.rows).max(0) as usize;
                (start_row..end_row)
                    .flat_map(move |y| (start_col..end_col).map(move |x| y * cols_us + x))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_coalesces_horizontal_strips() {
        let mut d = DirtyRegion::new();
        d.add_rect(0, 0, 1, 1);
        d.add_rect(1, 0, 1, 1);
        d.add_rect(2, 0, 1, 1);
        assert_eq!(d.rects.len(), 1);
        assert_eq!(d.rects[0], CellRect::new(0, 0, 3, 1));
    }

    #[test]
    fn iter_indices_covers_dirty_cells() {
        let mut d = DirtyRegion::new();
        d.add_rect(1, 1, 2, 1);
        let idx: Vec<usize> = d.iter_indices(4, 3).collect();
        // row 1, cols 1 and 2 → 1*4+1=5, 1*4+2=6
        assert_eq!(idx, vec![5, 6]);
    }

    #[test]
    #[allow(unused)]
    fn iter_indices_clamps_to_bounds() {
        let mut d = DirtyRegion::new();
        d.add_rect(-2, -2, 10, 10);
        let idx: Vec<usize> = d.iter_indices(3, 2).collect();
        // clamped to (0,0)-(3,2) → 0..6
        assert_eq!(idx, vec![0, 1, 2, 3, 4, 5]);
    }
}
