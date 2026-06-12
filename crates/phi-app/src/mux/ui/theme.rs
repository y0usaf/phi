//! Charmtone-derived palette (port of pi-harness `app::theme`): role
//! constants plus the cell-color resolution used when blitting vt100
//! screens onto the surface.

use super::color::Color;

// Charmtone hexes (subset used by the mux chrome).
pub const SRIRACHA: Color = Color::rgb(0xEB, 0x42, 0x68);
pub const DOLLY: Color = Color::rgb(0xFF, 0x60, 0xFF);
pub const CHARPLE: Color = Color::rgb(0x6B, 0x50, 0xFF);
pub const MALIBU: Color = Color::rgb(0x00, 0xA4, 0xFF);
pub const SARDINE: Color = Color::rgb(0x4F, 0xBE, 0xFE);
pub const ANCHOVY: Color = Color::rgb(0x71, 0x9A, 0xFC);
pub const GUAC: Color = Color::rgb(0x12, 0xC7, 0x8F);
pub const JULEP: Color = Color::rgb(0x00, 0xFF, 0xB2);
pub const BOK: Color = Color::rgb(0x68, 0xFF, 0xD6);
pub const MUSTARD: Color = Color::rgb(0xF5, 0xEF, 0x34);
pub const CITRON: Color = Color::rgb(0xE8, 0xFF, 0x27);
pub const BLUSH: Color = Color::rgb(0xFF, 0x84, 0xFF);
pub const CORAL: Color = Color::rgb(0xFF, 0x57, 0x7D);
pub const PEPPER: Color = Color::rgb(0x20, 0x1F, 0x26);
pub const BBQ: Color = Color::rgb(0x2D, 0x2C, 0x35);
pub const CHARCOAL: Color = Color::rgb(0x3A, 0x39, 0x43);
pub const IRON: Color = Color::rgb(0x4D, 0x4C, 0x57);
pub const SQUID: Color = Color::rgb(0x85, 0x83, 0x92);
pub const SMOKE: Color = Color::rgb(0xBF, 0xBC, 0xC8);
pub const ASH: Color = Color::rgb(0xDF, 0xDB, 0xDD);
pub const BUTTER: Color = Color::rgb(0xFF, 0xFA, 0xF1);

pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);

// Role tags consumed by the sidebar/scene code.
pub const TEXT: Color = ASH;
pub const MUTED: Color = SQUID;
pub const HEADING: Color = SMOKE;
pub const ACCENT: Color = BOK;
pub const ACCENT_2: Color = DOLLY;
pub const RUNNING: Color = CITRON;
pub const WARNING: Color = MUSTARD;
pub const ERROR: Color = SRIRACHA;
pub const SUCCESS: Color = JULEP;
pub const SUCCESS_SUBTLE: Color = GUAC;
pub const STATUS_FG: Color = BUTTER;
pub const STATUS_BG: Color = CHARPLE;
pub const BORDER: Color = CHARCOAL;
pub const SURFACE: Color = PEPPER;
pub const SURFACE_RAISED: Color = BBQ;
pub const TERM_FG: Color = TEXT;
const CURSOR: Color = DOLLY;
const TERM_SELECTION_BG: Color = CHARPLE;
const TERM_SELECTION_FG: Color = BUTTER;

pub fn default_ansi_palette() -> [Color; 16] {
    [
        PEPPER, SRIRACHA, JULEP, MUSTARD, MALIBU, DOLLY, BOK, SMOKE, IRON, CORAL, GUAC, CITRON,
        ANCHOVY, BLUSH, SARDINE, ASH,
    ]
}

pub fn screen_cell_colors(
    cell: &vt100::Cell,
    cursor_here: bool,
    selected: bool,
    default_fg: Color,
    default_bg: Color,
    ansi_palette: &[Color; 16],
) -> (Color, Color) {
    let mut fg = terminal_color(cell.fgcolor(), default_fg, ansi_palette);
    let mut bg = terminal_color(cell.bgcolor(), default_bg, ansi_palette);

    if cell.inverse() {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.bold() {
        fg = brighten(fg, 18);
    }
    if cell.dim() {
        fg = fade_toward(fg, bg, 110);
    }
    if selected {
        return (TERM_SELECTION_FG, TERM_SELECTION_BG);
    }
    if cursor_here {
        return (Color::rgb(9, 12, 18), CURSOR);
    }

    (fg, bg)
}

fn terminal_color(color: vt100::Color, default: Color, ansi_palette: &[Color; 16]) -> Color {
    match color {
        vt100::Color::Default => default,
        vt100::Color::Idx(idx) if idx < 16 => ansi_palette[idx as usize],
        vt100::Color::Idx(idx) => ansi_index_to_color(idx),
        vt100::Color::Rgb(r, g, b) => Color::rgb(r, g, b),
    }
}

const fn cube_component(component: u8) -> u8 {
    if component == 0 {
        0
    } else {
        55 + component * 40
    }
}

pub fn ansi_index_to_color(idx: u8) -> Color {
    let (r, g, b) = match idx {
        0 => (0x1d, 0x23, 0x2f),
        1 => (0xf7, 0x76, 0x8e),
        2 => (0x6f, 0xc7, 0x88),
        3 => (0xf4, 0xd0, 0x6b),
        4 => (0x7a, 0xa2, 0xf7),
        5 => (0xbb, 0x9a, 0xf7),
        6 => (0x7d, 0xd8, 0xe8),
        7 => (0xc8, 0xd3, 0xf5),
        8 => (0x44, 0x4b, 0x6a),
        9 => (0xff, 0x9e, 0xb0),
        10 => (0x8b, 0xde, 0xa3),
        11 => (0xfa, 0xe3, 0x8c),
        12 => (0x91, 0xb7, 0xff),
        13 => (0xce, 0xb6, 0xff),
        14 => (0x99, 0xe2, 0xf0),
        15 => (0xe5, 0xe9, 0xf0),
        16..=231 => {
            let index = idx - 16;
            let r = index / 36;
            let g = (index % 36) / 6;
            let b = index % 6;
            (cube_component(r), cube_component(g), cube_component(b))
        }
        232..=255 => {
            let shade = 8 + (idx - 232) * 10;
            (shade, shade, shade)
        }
    };
    Color::rgb(r, g, b)
}

pub fn brighten(color: Color, amount: u8) -> Color {
    if color.ansi_index_value().is_some() {
        return color;
    }

    let (r, g, b) = color.rgb_components();
    Color::rgb(
        r.saturating_add(amount),
        g.saturating_add(amount),
        b.saturating_add(amount),
    )
}

pub fn fade_toward(color: Color, target: Color, mix: u8) -> Color {
    if color.ansi_index_value().is_some() || target.ansi_index_value().is_some() {
        return if mix < 128 { color } else { target };
    }

    let (r1, g1, b1) = color.rgb_components();
    let (r2, g2, b2) = target.rgb_components();
    let blend = |a: u8, b: u8| -> u8 {
        let a = u16::from(a);
        let b = u16::from(b);
        let mix = u16::from(mix);
        (((a * (255 - mix)) + (b * mix)) / 255) as u8
    };
    Color::rgb(blend(r1, r2), blend(g1, g2), blend(b1, b2))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell_from_bytes(bytes: &[u8]) -> vt100::Cell {
        let mut parser = vt100::Parser::new(1, 8, 0);
        parser.process(bytes);
        parser.screen().cell(0, 0).unwrap().clone()
    }

    #[test]
    fn ansi_index_to_color_handles_base_cube_and_grayscale_ranges() {
        assert_eq!(ansi_index_to_color(1), Color::rgb(0xf7, 0x76, 0x8e));
        assert_eq!(ansi_index_to_color(16), Color::rgb(0, 0, 0));
        assert_eq!(ansi_index_to_color(231), Color::rgb(255, 255, 255));
        assert_eq!(ansi_index_to_color(232), Color::rgb(8, 8, 8));
        assert_eq!(ansi_index_to_color(255), Color::rgb(238, 238, 238));
    }

    #[test]
    fn indexed_colors_survive_brighten_and_fade() {
        let indexed = Color::ansi_index(12);
        let target = Color::rgb(100, 120, 140);
        assert_eq!(brighten(indexed, 20), indexed);
        assert_eq!(fade_toward(indexed, target, 64), indexed);
        assert_eq!(fade_toward(indexed, target, 192), target);
    }

    #[test]
    fn cell_colors_apply_selection_and_cursor_after_styles() {
        let styled = cell_from_bytes(b"\x1b[1;2;7mX");
        let bg = Color::rgb(10, 13, 18);
        let ansi = default_ansi_palette();
        assert_eq!(
            screen_cell_colors(&styled, false, true, TERM_FG, bg, &ansi),
            (TERM_SELECTION_FG, TERM_SELECTION_BG)
        );
        assert_eq!(
            screen_cell_colors(&styled, true, false, TERM_FG, bg, &ansi),
            (Color::rgb(9, 12, 18), CURSOR)
        );
    }
}
