//! Diff renderer (port of pi-harness `tui::ansi`): emits only the cells
//! that changed since the previous frame, wrapped in synchronized-update
//! markers so the terminal applies each frame atomically.

use std::io::{self, Write};

use super::cell_surface::{Cell, CellSurface};
use super::color::Color;

pub const DEFAULT_FG: Color = Color::rgba(0, 0, 0, 0);
pub const DEFAULT_BG: Color = Color::rgba(0, 0, 0, 0);

/// Where to park the real terminal cursor after a frame (1-based ANSI is
/// handled internally; fields are 0-based cells).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HardwareCursor {
    pub col: i32,
    pub row: i32,
}

#[derive(Default)]
pub struct AnsiRenderer {
    previous: Option<CellSurface>,
    scratch: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AnsiStyle {
    fg: Color,
    bg: Color,
    bold: bool,
    underline: bool,
    reverse: bool,
}

impl AnsiStyle {
    fn from_cell(cell: &Cell) -> Self {
        Self {
            fg: cell.fg,
            bg: cell.bg,
            bold: cell.bold,
            underline: cell.underline,
            reverse: cell.reverse,
        }
    }
}

impl AnsiRenderer {
    /// Drop frame history so the next render repaints everything.
    pub fn invalidate(&mut self) {
        self.previous = None;
    }

    pub fn render<W: Write>(
        &mut self,
        out: &mut W,
        surface: CellSurface,
        hardware_cursor: Option<HardwareCursor>,
    ) -> io::Result<CellSurface> {
        write!(out, "\x1b[?2026h\x1b[?25l")?;
        match self.previous.as_ref() {
            Some(previous) if previous.cols == surface.cols && previous.rows == surface.rows => {
                render_diff(out, previous, &surface, &mut self.scratch)?;
            }
            _ => render_full(out, &surface)?,
        }
        render_hardware_cursor(out, hardware_cursor)?;
        write!(out, "\x1b[?2026l")?;
        let reusable = self.previous.replace(surface).unwrap_or_default();
        out.flush()?;
        Ok(reusable)
    }
}

fn render_full<W: Write>(out: &mut W, surface: &CellSurface) -> io::Result<()> {
    write!(out, "\x1b[H")?;
    for row in 0..surface.rows {
        write!(out, "\x1b[{};1H", row + 1)?;
        let mut current_style = None;
        for col in 0..surface.cols {
            let cell = &surface.cells[(row * surface.cols + col) as usize];
            if cell.continuation {
                continue;
            }
            let style = AnsiStyle::from_cell(cell);
            if current_style != Some(style) {
                write_style(out, style)?;
                current_style = Some(style);
            }
            out.write_all(cell.text.as_bytes())?;
        }
        write!(out, "\x1b[0m")?;
    }
    Ok(())
}

fn render_diff<W: Write>(
    out: &mut W,
    previous: &CellSurface,
    surface: &CellSurface,
    scratch: &mut String,
) -> io::Result<()> {
    for row in 0..surface.rows {
        let mut col = 0;
        while col < surface.cols {
            let index = (row * surface.cols + col) as usize;
            let cell = &surface.cells[index];
            if previous.cells[index] == *cell || cell.continuation {
                col += 1;
                continue;
            }

            let style = AnsiStyle::from_cell(cell);
            let start_col = col;
            scratch.clear();
            while col < surface.cols {
                let index = (row * surface.cols + col) as usize;
                let cell = &surface.cells[index];
                if previous.cells[index] == *cell {
                    break;
                }
                if cell.continuation {
                    col += 1;
                    continue;
                }
                if AnsiStyle::from_cell(cell) != style {
                    break;
                }
                scratch.push_str(&cell.text);
                col += 1;
            }

            if !scratch.is_empty() {
                write!(out, "\x1b[{};{}H", row + 1, start_col + 1)?;
                write_style(out, style)?;
                out.write_all(scratch.as_bytes())?;
            }
        }
    }
    write!(out, "\x1b[0m")
}

fn render_hardware_cursor<W: Write>(
    out: &mut W,
    hardware_cursor: Option<HardwareCursor>,
) -> io::Result<()> {
    if let Some(cursor) = hardware_cursor {
        write!(out, "\x1b[{};{}H\x1b[?25h", cursor.row + 1, cursor.col + 1)
    } else {
        write!(out, "\x1b[?25l")
    }
}

fn write_style<W: Write>(out: &mut W, style: AnsiStyle) -> io::Result<()> {
    write!(out, "\x1b[0m")?;
    write_fg(out, style.fg)?;
    write_bg(out, style.bg)?;
    if style.bold {
        write!(out, "\x1b[1m")?;
    }
    if style.underline {
        write!(out, "\x1b[4m")?;
    }
    if style.reverse {
        write!(out, "\x1b[7m")?;
    }
    Ok(())
}

fn write_fg<W: Write>(out: &mut W, color: Color) -> io::Result<()> {
    if color == DEFAULT_FG {
        return write!(out, "\x1b[39m");
    }
    if let Some(index) = color.ansi_index_value() {
        return match index {
            0..=7 => write!(out, "\x1b[{}m", 30 + index),
            8..=15 => write!(out, "\x1b[{}m", 90 + index - 8),
            _ => write!(out, "\x1b[38;5;{index}m"),
        };
    }
    let (r, g, b) = color.rgb_components();
    write!(out, "\x1b[38;2;{r};{g};{b}m")
}

fn write_bg<W: Write>(out: &mut W, color: Color) -> io::Result<()> {
    if color == DEFAULT_BG {
        return write!(out, "\x1b[49m");
    }
    if let Some(index) = color.ansi_index_value() {
        return match index {
            0..=7 => write!(out, "\x1b[{}m", 40 + index),
            8..=15 => write!(out, "\x1b[{}m", 100 + index - 8),
            _ => write!(out, "\x1b[48;5;{index}m"),
        };
    }
    let (r, g, b) = color.rgb_components();
    write!(out, "\x1b[48;2;{r};{g};{b}m")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(renderer: &mut AnsiRenderer, surface: CellSurface) -> String {
        let mut out = Vec::new();
        renderer.render(&mut out, surface, None).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn second_identical_frame_emits_no_cells() {
        let mut renderer = AnsiRenderer::default();
        let mut surface = CellSurface::new(4, 2, DEFAULT_FG, DEFAULT_BG);
        surface.put_text(0, 0, 4, DEFAULT_FG, DEFAULT_BG, "hi");
        let first = frame(&mut renderer, surface.clone());
        assert!(first.contains("hi"));
        assert!(first.starts_with("\x1b[?2026h"));
        assert!(first.ends_with("\x1b[?2026l"));
        let second = frame(&mut renderer, surface);
        assert!(!second.contains("hi"));
    }

    #[test]
    fn changed_cell_is_rewritten_in_place() {
        let mut renderer = AnsiRenderer::default();
        let mut surface = CellSurface::new(4, 2, DEFAULT_FG, DEFAULT_BG);
        surface.put_text(0, 0, 4, DEFAULT_FG, DEFAULT_BG, "ab");
        frame(&mut renderer, surface.clone());
        surface.put_text(1, 0, 4, DEFAULT_FG, DEFAULT_BG, "X");
        let diff = frame(&mut renderer, surface);
        assert!(diff.contains("\x1b[1;2H"));
        assert!(diff.contains('X'));
        assert!(!diff.contains('a'));
    }

    #[test]
    fn shrinking_line_rewrites_blank_tail_cells() {
        let mut renderer = AnsiRenderer::default();
        let mut first = CellSurface::new(6, 1, DEFAULT_FG, DEFAULT_BG);
        first.put_text(0, 0, 6, DEFAULT_FG, DEFAULT_BG, "abcdef");
        frame(&mut renderer, first);

        let mut second = CellSurface::new(6, 1, DEFAULT_FG, DEFAULT_BG);
        second.put_text(0, 0, 6, DEFAULT_FG, DEFAULT_BG, "ab");
        let diff = frame(&mut renderer, second);

        assert!(diff.contains("\x1b[1;3H"), "diff={:?}", diff);
        assert!(diff.contains("    "), "diff={:?}", diff);
    }

    #[test]
    fn recolors_reversed_cells_when_only_colors_change() {
        let mut renderer = AnsiRenderer::default();
        let mut first = CellSurface::new(2, 1, DEFAULT_FG, DEFAULT_BG);
        first.put_text_styled(
            0,
            0,
            2,
            Color::rgb(255, 0, 0),
            Color::rgb(0, 0, 0),
            "X",
            true,
            false,
        );
        frame(&mut renderer, first);

        let mut second = CellSurface::new(2, 1, DEFAULT_FG, DEFAULT_BG);
        second.put_text_styled(
            0,
            0,
            2,
            Color::rgb(0, 255, 0),
            Color::rgb(0, 0, 0),
            "X",
            true,
            false,
        );
        let diff = frame(&mut renderer, second);

        assert!(diff.contains("\x1b[1;1H"), "diff={:?}", diff);
        assert!(diff.contains("\x1b[38;2;0;255;0m"), "diff={:?}", diff);
        assert!(diff.contains("\x1b[7m"), "diff={:?}", diff);
        assert!(diff.contains('X'), "diff={:?}", diff);
    }
}
