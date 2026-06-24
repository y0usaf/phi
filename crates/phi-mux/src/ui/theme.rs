//! Charmtone-derived palette (port of pi-harness `app::theme`): role
//! constants plus the cell-color resolution used when blitting vt100
//! screens onto the surface.

use super::color::Color;
use phi_tui::palette::TerminalColors;

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
/// Cursor sits in the accent family rather than shouting in pink.
const CURSOR: Color = BOK;
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

// ----- terminal-driven theme generation (port of opencode `generateSystem`) --
//
// Derives a full mux palette from the host terminal's reported colors so the
// chrome follows the user's terminal theme instead of forcing a fixed one.
// Mirrors opencode's approach: ANSI-mapped accents/status, a background-derived
// gray ramp, and a generated muted text color. Surfaces stay opaque (unlike
// opencode's transparent background) because mux draws its own grounded canvas.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
}

/// Colors resolved from the host terminal, in the mux `Color` space. Built from
/// `phi_tui::palette::TerminalColors` (OSC 10/11/4 probes); missing palette
/// entries fall back to the standard 256-color cube via `ansi_index_to_color`.
pub struct GeneratedTheme {
    pub fg: Color,
    pub bg: Color,
    pub surface_raised: Color,
    pub statusbar_fg: Color,
    pub statusbar_bg: Color,
    pub sidebar_bg: Color,
    pub border: Color,
    pub muted: Color,
    pub term_fg: Color,
    pub term_bg: Color,
    pub accent: Color,
    pub accent_2: Color,
    pub ansi: [Color; 16],
    pub running: Color,
    pub success: Color,
    pub success_subtle: Color,
    pub warning: Color,
    pub error: Color,
}

pub fn terminal_mode(colors: &TerminalColors) -> Mode {
    let c = colors.background;
    if luminance_rgb(c.0, c.1, c.2) > 127 {
        Mode::Light
    } else {
        Mode::Dark
    }
}

pub fn generate_system(colors: &TerminalColors) -> GeneratedTheme {
    let mode = terminal_mode(colors);
    let bg = color_of(colors.background);
    let fg = color_of(colors.foreground);
    let grays = generate_gray_scale(bg, mode);
    let muted = generate_muted_text(bg, mode);
    let ansi = ansi_from_terminal(colors);
    let col = |i: u8| palette_color(colors, i);

    // opencode tints diff backgrounds at ~0.22 (dark) / 0.14 (light) alpha;
    // reuse the same strength for the subtle success wash.
    let subtle_alpha = if matches!(mode, Mode::Dark) { 56 } else { 36 };

    GeneratedTheme {
        fg,
        bg: grays[2],
        surface_raised: grays[3],
        statusbar_fg: fg,
        statusbar_bg: grays[3],
        sidebar_bg: grays[2],
        border: grays[7],
        muted,
        term_fg: fg,
        term_bg: grays[2],
        accent: col(6),
        accent_2: col(5),
        ansi,
        running: col(10),
        success: col(2),
        success_subtle: fade_toward(grays[2], col(2), subtle_alpha),
        warning: col(3),
        error: col(1),
    }
}

fn palette_color(colors: &TerminalColors, idx: u8) -> Color {
    colors.palette[idx as usize]
        .map(color_of)
        .unwrap_or_else(|| ansi_index_to_color(idx))
}

fn ansi_from_terminal(colors: &TerminalColors) -> [Color; 16] {
    let mut out = [Color::rgb(0, 0, 0); 16];
    for i in 0..16u8 {
        out[i as usize] = palette_color(colors, i);
    }
    out
}

fn color_of(c: phi_tui::palette::Rgb) -> Color {
    Color::rgb(c.0, c.1, c.2)
}

fn luminance_rgb(r: u8, g: u8, b: u8) -> u32 {
    (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000
}

/// 12-step ramp (indices 1..=12; index 0 unused) derived from the terminal
/// background, matching opencode's `generateGrayScale`.
fn generate_gray_scale(bg: Color, mode: Mode) -> [Color; 13] {
    let mut grays = [Color::rgb(0, 0, 0); 13];
    let (br, bgc, bb) = bg.rgb_components();
    let lum = luminance_rgb(br, bgc, bb);
    let dark = matches!(mode, Mode::Dark);

    for i in 1..=12u8 {
        let factor = f64::from(i) / 12.0;
        let (r, g, b) = if dark {
            if lum < 10 {
                let v = (factor * 0.4 * 255.0).floor() as u8;
                (v, v, v)
            } else {
                let new_lum = f64::from(lum) + (255.0 - f64::from(lum)) * factor * 0.4;
                let ratio = new_lum / f64::from(lum);
                scale_toward(br, bgc, bb, ratio, false)
            }
        } else if lum > 245 {
            let v = (255.0 - factor * 0.4 * 255.0).floor() as u8;
            (v, v, v)
        } else {
            let new_lum = f64::from(lum) * (1.0 - factor * 0.4);
            let ratio = new_lum / f64::from(lum);
            scale_toward(br, bgc, bb, ratio, true)
        };
        grays[i as usize] = Color::rgb(r, g, b);
    }
    grays
}

fn scale_toward(r: u8, g: u8, b: u8, ratio: f64, floor: bool) -> (u8, u8, u8) {
    let clamp = |v: f64| {
        let v = v * ratio;
        if floor {
            v.max(0.0)
        } else {
            v.min(255.0)
        }
    };
    (clamp(f64::from(r)) as u8, clamp(f64::from(g)) as u8, clamp(f64::from(b)) as u8)
}

/// Port of opencode's `generateMutedTextColor`: a neutral gray tuned to the
/// background luminance so muted text stays readable in both modes.
fn generate_muted_text(bg: Color, mode: Mode) -> Color {
    let (r, g, b) = bg.rgb_components();
    let lum = luminance_rgb(r, g, b);
    let gray = if matches!(mode, Mode::Dark) {
        if lum < 10 {
            180u16
        } else {
            ((160.0 + f64::from(lum) * 0.3).min(200.0)) as u16
        }
    } else if lum > 245 {
        75u16
    } else {
        ((100.0 - (255.0 - f64::from(lum)) * 0.2).max(60.0)) as u16
    };
    Color::rgb(gray as u8, gray as u8, gray as u8)
}

// ---------------------------------------------------------------------------
// Semantic palette (port of jcode-tui-style `theme` semantic colors)
// ---------------------------------------------------------------------------
// Rich, role-specific colors for chat / message rendering. These complement
// the structural palette above. `ACCENT` (BOK) above is the primary accent;
// `accent_color()` here provides jcode's purple semantic accent.

pub fn user_color() -> Color {
    Color::rgb(138, 180, 248)
}
pub fn ai_color() -> Color {
    Color::rgb(129, 199, 132)
}
pub fn tool_color() -> Color {
    Color::rgb(120, 120, 120)
}
pub fn file_link_color() -> Color {
    Color::rgb(180, 200, 255)
}
pub fn accent_color() -> Color {
    Color::rgb(186, 139, 255)
}
pub fn system_message_color() -> Color {
    Color::rgb(255, 170, 220)
}
pub fn dim_color() -> Color {
    Color::rgb(80, 80, 80)
}
pub fn queued_color() -> Color {
    Color::rgb(255, 193, 7)
}
pub fn asap_color() -> Color {
    Color::rgb(110, 210, 255)
}
pub fn pending_color() -> Color {
    Color::rgb(140, 140, 140)
}
pub fn user_text() -> Color {
    Color::rgb(245, 245, 255)
}
pub fn user_bg() -> Color {
    Color::rgb(35, 40, 50)
}
pub fn ai_text() -> Color {
    Color::rgb(220, 220, 215)
}
pub fn header_icon_color() -> Color {
    Color::rgb(120, 210, 230)
}
pub fn header_name_color() -> Color {
    Color::rgb(190, 210, 235)
}
pub fn header_session_color() -> Color {
    Color::rgb(255, 255, 255)
}

// ---------------------------------------------------------------------------
// Multi-tier animation FPS settings (port of jcode spinner system)
// ---------------------------------------------------------------------------

/// Spinner frames for animated status. Keep these single-cell because the
/// fast spinner-only renderer patches one status cell between full TUI
/// redraws. This sequence should read as a circular spin, not a pulse.
const SPINNER_FRAMES: &[&str] = &["\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}", "\u{2827}", "\u{2807}", "\u{280f}"];

/// Frame rate for slow, full-line "liveness" indicators that can only be
/// repainted by a full TUI redraw (e.g. a running-tool progress bar) when
/// decorative animations are disabled (Minimal tier, SSH, WSL, etc.). These
/// ride the ~1 Hz passive-liveness redraw, so advancing them faster would
/// just skip frames.
pub const LIVENESS_INDICATOR_FPS: f32 = 1.5;

/// Frame rate for the low-cost single-cell circular spinner when decorative
/// animations are disabled. Unlike the full-line indicators above, this
/// spinner is patched by the cheap one-cell fast path between full redraws,
/// so it can animate at a smooth, responsive cadence well above ~1 Hz while
/// still staying very light on resources.
pub const LIVENESS_SPINNER_FPS: f32 = 12.5;

pub fn spinner_frame_index(elapsed: f32, fps: f32) -> usize {
    ((elapsed * fps) as usize) % SPINNER_FRAMES.len()
}

pub fn spinner_frame(elapsed: f32, fps: f32) -> &'static str {
    SPINNER_FRAMES[spinner_frame_index(elapsed, fps)]
}

pub fn activity_indicator_frame_index(
    elapsed: f32,
    fps: f32,
    enable_decorative_animations: bool,
) -> usize {
    if enable_decorative_animations {
        spinner_frame_index(elapsed, fps)
    } else {
        spinner_frame_index(elapsed, LIVENESS_SPINNER_FPS)
    }
}

pub fn activity_indicator(
    elapsed: f32,
    fps: f32,
    enable_decorative_animations: bool,
) -> &'static str {
    SPINNER_FRAMES[activity_indicator_frame_index(elapsed, fps, enable_decorative_animations)]
}

// ---------------------------------------------------------------------------
// Color blending utilities (port of jcode blend + color_to_floats)
// ---------------------------------------------------------------------------

/// Extract (r, g, b) floats from a `Color`, resolving ANSI-indexed colors
/// via the 256-color cube. Falls back for fully transparent (default) colors.
pub fn color_to_floats(c: Color, fallback: (f32, f32, f32)) -> (f32, f32, f32) {
    if let Some(idx) = c.ansi_index_value() {
        let resolved = ansi_index_to_color(idx);
        let (r, g, b) = resolved.rgb_components();
        (r as f32, g as f32, b as f32)
    } else {
        let (r, g, b) = c.rgb_components();
        // Detect transparent/default (alpha == 0) and use fallback.
        if c.0 >> 24 == 0 {
            fallback
        } else {
            (r as f32, g as f32, b as f32)
        }
    }
}

pub fn blend_color(from: Color, to: Color, t: f32) -> Color {
    let (fr, fg, fb) = color_to_floats(from, (80.0, 80.0, 80.0));
    let (tr, tg, tb) = color_to_floats(to, (200.0, 200.0, 200.0));
    let r = fr + (tr - fr) * t;
    let g = fg + (tg - fg) * t;
    let b = fb + (tb - fb) * t;
    Color::rgb(
        r.clamp(0.0, 255.0) as u8,
        g.clamp(0.0, 255.0) as u8,
        b.clamp(0.0, 255.0) as u8,
    )
}

// ---------------------------------------------------------------------------
// Prompt + tool animation functions (port of jcode prompt animations)
// ---------------------------------------------------------------------------

pub fn rainbow_prompt_color(distance: usize) -> Color {
    const RAINBOW: [(u8, u8, u8); 7] = [
        (255, 80, 80),   // Red (softened)
        (255, 160, 80),  // Orange
        (255, 230, 80),  // Yellow
        (80, 220, 100),  // Green
        (80, 200, 220),  // Cyan
        (100, 140, 255), // Blue
        (180, 100, 255), // Violet
    ];
    const GRAY: (u8, u8, u8) = (80, 80, 80);
    let decay = (-0.4 * distance as f32).exp();
    let rainbow_idx = distance.min(RAINBOW.len() - 1);
    let (r, g, b) = RAINBOW[rainbow_idx];
    let blend = |rainbow: u8, gray: u8| -> u8 {
        (rainbow as f32 * decay + gray as f32 * (1.0 - decay)) as u8
    };
    Color::rgb(blend(r, GRAY.0), blend(g, GRAY.1), blend(b, GRAY.2))
}

pub fn prompt_entry_color(base: Color, t: f32) -> Color {
    let peak = Color::rgb(255, 230, 120);
    let phase = if t < 0.5 { t * 2.0 } else { (1.0 - t) * 2.0 };
    blend_color(base, peak, phase.clamp(0.0, 1.0) * 0.7)
}

pub fn prompt_entry_bg_color(base: Color, t: f32) -> Color {
    let spotlight = Color::rgb(58, 66, 82);
    let ease_in = 1.0 - (1.0 - t).powi(3);
    let ease_out = (1.0 - t).powi(2);
    let phase = (ease_in * ease_out * 1.65).clamp(0.0, 1.0);
    blend_color(base, spotlight, phase * 0.85)
}

pub fn prompt_entry_shimmer_color(base: Color, pos: f32, t: f32) -> Color {
    let travel = (t * 1.15).clamp(0.0, 1.0);
    let width = 0.18;
    let dist = (pos - travel).abs();
    let shimmer = (1.0 - (dist / width).clamp(0.0, 1.0)).powf(2.2);
    let pulse = (1.0 - t).powf(0.55);
    let highlight = Color::rgb(255, 248, 210);
    blend_color(base, highlight, shimmer * pulse * 0.7)
}

/// Generate an animated color that pulses between two colors.
pub fn animated_tool_color(elapsed: f32, enable_decorative_animations: bool) -> Color {
    if !enable_decorative_animations {
        return tool_color();
    }
    let t = (elapsed * 2.0).sin() * 0.5 + 0.5;
    let r = (80.0 + t * 106.0) as u8;
    let g = (200.0 - t * 61.0) as u8;
    let b = (220.0 + t * 35.0) as u8;
    Color::rgb(r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phi_tui::palette::{Rgb, TerminalColors};

    fn colors_from(bg: Rgb, fg: Rgb) -> TerminalColors {
        TerminalColors {
            background: bg,
            foreground: fg,
            palette: std::array::from_fn(|i| {
                Some(Rgb(i as u8 * 8, i as u8 * 8, i as u8 * 8))
            }),
        }
    }

    #[test]
    fn terminal_mode_splits_on_background_luminance() {
        assert_eq!(terminal_mode(&colors_from(Rgb(0, 0, 0), Rgb(255, 255, 255))), Mode::Dark);
        assert_eq!(
            terminal_mode(&colors_from(Rgb(255, 255, 255), Rgb(0, 0, 0))),
            Mode::Light
        );
    }

    #[test]
    fn generate_system_maps_ansi_roles_and_surfaces() {
        let colors = colors_from(Rgb(20, 20, 26), Rgb(220, 220, 220));
        let theme = generate_system(&colors);

        // Accents/status follow the ANSI palette, not the background.
        assert_eq!(theme.accent, theme.ansi[6]);
        assert_eq!(theme.accent_2, theme.ansi[5]);
        assert_eq!(theme.error, theme.ansi[1]);
        assert_eq!(theme.success, theme.ansi[2]);
        assert_eq!(theme.warning, theme.ansi[3]);
        // Text mirrors the terminal foreground; the canvas is a generated surface.
        assert_eq!(theme.fg, Color::rgb(220, 220, 220));
        assert_eq!(theme.term_fg, theme.fg);
        assert_ne!(theme.bg, Color::rgb(20, 20, 26));
    }

    #[test]
    fn generate_system_falls_back_to_cube_colors_for_missing_palette_entries() {
        let mut colors = colors_from(Rgb(0, 0, 0), Rgb(255, 255, 255));
        colors.palette[6] = None;
        let theme = generate_system(&colors);
        assert_eq!(theme.accent, ansi_index_to_color(6));
    }

    #[test]
    fn muted_text_is_neutral_and_lighter_on_dark_than_background() {
        let colors = colors_from(Rgb(20, 20, 26), Rgb(220, 220, 220));
        let theme = generate_system(&colors);
        let (r, g, b) = theme.muted.rgb_components();
        assert_eq!(r, g);
        assert_eq!(g, b);
        assert!(u32::from(r) > luminance_rgb(20, 20, 26));
    }

    #[test]
    fn success_subtle_is_a_tint_toward_green() {
        let colors = colors_from(Rgb(20, 20, 26), Rgb(220, 220, 220));
        let theme = generate_system(&colors);
        let expected = fade_toward(theme.bg, theme.ansi[2], 56);
        assert_eq!(theme.success_subtle, expected);
    }

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
