//! Mouse selection over the embedded terminal (port of pi-harness
//! `terminal::selection`): screen-space anchor/focus pair normalized into a
//! row-major range.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectionPoint {
    pub row: u16,
    pub col: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionRange {
    pub start: SelectionPoint,
    pub end: SelectionPoint,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalSelection {
    anchor: Option<SelectionPoint>,
    focus: Option<SelectionPoint>,
}

impl TerminalSelection {
    pub fn clear(&mut self) {
        self.anchor = None;
        self.focus = None;
    }

    pub fn set(&mut self, point: SelectionPoint) {
        self.anchor = Some(point);
        self.focus = Some(point);
    }

    pub fn update_focus(&mut self, point: SelectionPoint) {
        self.focus = Some(point);
    }

    pub fn is_active(&self) -> bool {
        self.anchor.is_some()
    }

    pub fn normalized(&self) -> Option<SelectionRange> {
        let (anchor, focus) = (self.anchor?, self.focus?);
        if anchor == focus {
            None
        } else if (anchor.row, anchor.col) <= (focus.row, focus.col) {
            Some(SelectionRange {
                start: anchor,
                end: focus,
            })
        } else {
            Some(SelectionRange {
                start: focus,
                end: anchor,
            })
        }
    }
}

/// The selected `(start_col, width)` of `row`, if any.
pub fn selection_span(
    selection: Option<SelectionRange>,
    row: u16,
    cols: u16,
) -> Option<(u16, u16)> {
    let selection = selection?;
    if row < selection.start.row || row > selection.end.row {
        return None;
    }

    let row_start = if row == selection.start.row {
        selection.start.col.min(cols)
    } else {
        0
    };
    let row_end = if row == selection.end.row {
        selection.end.col.min(cols)
    } else {
        cols
    };
    (row_start < row_end).then_some((row_start, row_end - row_start))
}

/// The selected text of a vt100 screen, rows joined with newlines and
/// trailing blanks per row trimmed.
pub fn selected_text(screen: &vt100::Screen, range: SelectionRange) -> String {
    let (_, cols) = screen.size();
    let mut lines = Vec::new();
    for row in range.start.row..=range.end.row {
        let Some((start, width)) = selection_span(Some(range), row, cols) else {
            lines.push(String::new());
            continue;
        };
        let mut line = String::new();
        let mut col = start;
        while col < start + width {
            match screen.cell(row, col) {
                Some(cell) if cell.is_wide_continuation() => {}
                Some(cell) if cell.contents().is_empty() => line.push(' '),
                Some(cell) => line.push_str(cell.contents()),
                None => {}
            }
            col += 1;
        }
        lines.push(line.trim_end().to_owned());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(row: u16, col: u16) -> SelectionPoint {
        SelectionPoint { row, col }
    }

    #[test]
    fn normalized_orders_endpoints_and_drops_empty() {
        let mut selection = TerminalSelection::default();
        selection.set(point(2, 5));
        assert_eq!(selection.normalized(), None);
        selection.update_focus(point(1, 3));
        let range = selection.normalized().unwrap();
        assert_eq!(range.start, point(1, 3));
        assert_eq!(range.end, point(2, 5));
    }

    #[test]
    fn span_covers_full_middle_rows() {
        let range = SelectionRange {
            start: point(1, 4),
            end: point(3, 2),
        };
        assert_eq!(selection_span(Some(range), 0, 10), None);
        assert_eq!(selection_span(Some(range), 1, 10), Some((4, 6)));
        assert_eq!(selection_span(Some(range), 2, 10), Some((0, 10)));
        assert_eq!(selection_span(Some(range), 3, 10), Some((0, 2)));
    }

    #[test]
    fn selected_text_extracts_screen_contents() {
        let mut parser = vt100::Parser::new(3, 10, 0);
        parser.process(b"hello\r\nworld");
        let text = selected_text(
            parser.screen(),
            SelectionRange {
                start: point(0, 0),
                end: point(1, 5),
            },
        );
        assert_eq!(text, "hello\nworld");
    }
}
