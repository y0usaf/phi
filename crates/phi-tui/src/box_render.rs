//! Rounded box renderer with ╭─Title─╮ style borders.
//!
//! Renders Unicode box-drawing borders with an optional title embedded in
//! the top border. Uses shared ANSI style constants from [`crate::style`] and
//! cell-width helpers from [`crate::cells`].

use crate::cells::{display_cell_width, truncate_to_cells};
use crate::style::{DIM, RESET};

/// Rounded box-drawing characters.
const TL: char = '╭'; // top-left  (U+256D)
const TR: char = '╮'; // top-right (U+256E)
const BL: char = '╰'; // bottom-left  (U+2570)
const BR: char = '╯'; // bottom-right (U+256F)
const H: char = '─'; // horizontal (U+2500)
const V: char = '│'; // vertical   (U+2502)

/// Render rounded box border lines (top + bottom) with an optional title
/// embedded in the top border.
///
/// Returns exactly two strings:
/// - index 0 — top border: `╭─ Title ─...─╮`
/// - index 1 — bottom border: `╰─...─╯`
///
/// `content_width` is the inner width between the vertical walls (i.e. the
/// number of columns available for content). The title is truncated with an
/// ellipsis if it doesn't fit. When the title is empty the top border is a
/// plain `╭─...─╮` line. Values are clamped to a minimum of `0`.
pub fn render_rounded_box(title: &str, content_width: usize) -> Vec<String> {
    let width = content_width.max(0);
    let title_w = display_cell_width(title);

    let top = if title.is_empty() || title_w == 0 {
        format!("{DIM}{TL}{}{TR}{RESET}", H.to_string().repeat(width))
    } else {
        // Layout inside corners: "─ {title} {trailing dashes}"
        // overhead = "─ " (2 cols) + " " (1 col) = 3
        let overhead = 3;
        let max_title = width.saturating_sub(overhead);
        let title_str = if title_w > max_title && max_title > 0 {
            truncate_to_cells(title, max_title)
        } else if max_title == 0 {
            String::new()
        } else {
            title.to_string()
        };
        let actual_w = display_cell_width(&title_str);
        let remaining = width.saturating_sub(actual_w + overhead);
        format!(
            "{DIM}{TL}{RESET}{H} {title_str} {DIM}{}{TR}{RESET}",
            H.to_string().repeat(remaining)
        )
    };

    let bottom = format!("{DIM}{BL}{}{BR}{RESET}", H.to_string().repeat(width));

    vec![top, bottom]
}

/// Wrap a single content line with vertical borders: `│ text    │`.
///
/// The text is padded with spaces to fill `content_width`. If the text is
/// wider than `content_width`, it is truncated with an ellipsis.
pub fn wrap_content_line(text: &str, content_width: usize) -> String {
    let width = content_width.max(0);
    let text_w = display_cell_width(text);
    let text_str = if text_w > width && width > 0 {
        truncate_to_cells(text, width)
    } else if width == 0 {
        String::new()
    } else {
        text.to_string()
    };
    let actual_w = display_cell_width(&text_str);
    // Account for leading + trailing space inside the walls.
    let pad = width.saturating_sub(actual_w + 2);
    format!(
        "{DIM}{V}{RESET} {text_str}{} {DIM}{V}{RESET}",
        " ".repeat(pad)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(s: &str) -> String {
        crate::terminal::strip_ansi(s).to_string()
    }

    #[test]
    fn no_title_plain_box() {
        let lines = render_rounded_box("", 10);
        assert_eq!(lines.len(), 2);
        assert_eq!(strip(&lines[0]), "╭──────────╮");
        assert_eq!(strip(&lines[1]), "╰──────────╯");
    }

    #[test]
    fn title_in_border() {
        let lines = render_rounded_box("Title", 20);
        assert_eq!(lines.len(), 2);
        let top = strip(&lines[0]);
        assert_eq!(top, "╭─ Title ────────────╮");
        assert_eq!(strip(&lines[1]), "╰────────────────────╯");
    }

    #[test]
    fn width_zero() {
        let lines = render_rounded_box("", 0);
        assert_eq!(strip(&lines[0]), "╭╮");
        assert_eq!(strip(&lines[1]), "╰╯");
    }

    #[test]
    fn title_too_wide_truncates() {
        let lines = render_rounded_box("A very long title", 10);
        let top = strip(&lines[0]);
        // overhead = 3 ( "─ " + " " ), max_title = 7
        // truncation should produce ≤7 chars including ellipsis
        assert!(top.starts_with("╭─ "));
        assert!(top.ends_with("╮"));
        assert_eq!(strip(&lines[1]), "╰──────────╯");
    }

    #[test]
    fn content_line_pads() {
        let line = strip(&wrap_content_line("hi", 10));
        assert_eq!(line, "│ hi       │");
    }

    #[test]
    fn content_line_truncates() {
        let line = strip(&wrap_content_line("hello world", 5));
        assert_eq!(line, "│ hell… │");
    }
}
