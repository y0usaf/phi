//! Packed ARGB color (port of pi-harness `render::Color`); an alpha tag of
//! 0x01 marks an ANSI palette index instead of truecolor.
//!
//! Also provides terminal color-capability detection (truecolor vs 256-color)
//! ported from jcode's `jcode-tui-style::color`. The detection uses env-var
//! fingerprinting (`COLORTERM`, `TERM_PROGRAM`, `TERM`, etc.) and auto-downgrades
//! fragile-glyph-cache terminals (macOS VS Code / Apple Terminal) to 256-color
//! to prevent glyph atlas overflow under color animation.

use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Packed color type
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Color(pub u32);

impl Color {
    const ANSI_INDEX_ALPHA: u32 = 0x01;

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self(0xFF00_0000 | ((r as u32) << 16) | ((g as u32) << 8) | b as u32)
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self(((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32)
    }

    pub const fn ansi_index(index: u8) -> Self {
        Self((Self::ANSI_INDEX_ALPHA << 24) | index as u32)
    }

    pub const fn ansi_index_value(self) -> Option<u8> {
        if self.0 >> 24 == Self::ANSI_INDEX_ALPHA {
            Some((self.0 & 0xff) as u8)
        } else {
            None
        }
    }

    #[inline]
    pub const fn rgb_components(self) -> (u8, u8, u8) {
        let value = self.0;
        (
            ((value >> 16) & 0xff) as u8,
            ((value >> 8) & 0xff) as u8,
            (value & 0xff) as u8,
        )
    }
}

// ---------------------------------------------------------------------------
// Color capability detection (ported from jcode)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorCapability {
    TrueColor,
    Color256,
}

static CAPABILITY: OnceLock<ColorCapability> = OnceLock::new();

pub fn color_capability() -> ColorCapability {
    *CAPABILITY.get_or_init(detect_color_capability)
}

/// Terminals whose GPU glyph atlas corrupts under heavy per-cell *truecolor*
/// churn (the macOS 26 "garbled glyphs" bug in the VS Code integrated terminal
/// and Apple Terminal). These renderers key their rasterized-glyph cache on
/// the full 24-bit color, so continuous color animations generate an
/// effectively unbounded set of atlas entries, overflowing it and
/// re-rendering stale cached glyphs as boxes.
///
/// Capping these terminals to the 256-color palette bounds the distinct-color
/// space to a value the atlas can actually cache. Robust GPU terminals
/// (Ghostty / iTerm2 / kitty / WezTerm / Alacritty) are unaffected and keep
/// full truecolor.
///
/// Overridable with `JCODE_GLYPH_SAFE_MODE=on|off`.
fn fragile_glyph_cache_terminal() -> bool {
    if let Ok(raw) = std::env::var("JCODE_GLYPH_SAFE_MODE") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => return true,
            "0" | "false" | "no" | "off" => return false,
            _ => {}
        }
    }

    if !cfg!(target_os = "macos") {
        return false;
    }

    match std::env::var("TERM_PROGRAM") {
        Ok(tp) => {
            let tp = tp.to_ascii_lowercase();
            tp == "vscode" || tp == "apple_terminal"
        }
        Err(_) => false,
    }
}

fn detect_color_capability() -> ColorCapability {
    let raw = detect_raw_color_capability();
    if raw == ColorCapability::TrueColor && fragile_glyph_cache_terminal() {
        return ColorCapability::Color256;
    }
    raw
}

fn detect_raw_color_capability() -> ColorCapability {
    if let Ok(val) = std::env::var("COLORTERM") {
        let v = val.to_lowercase();
        if v == "truecolor" || v == "24bit" {
            return ColorCapability::TrueColor;
        }
    }

    if let Ok(term_program) = std::env::var("TERM_PROGRAM") {
        let tp = term_program.to_lowercase();
        if tp == "ghostty"
            || tp == "iterm.app"
            || tp == "wezterm"
            || tp == "warp"
            || tp == "alacritty"
            || tp == "hyper"
        {
            return ColorCapability::TrueColor;
        }
    }

    if std::env::var("GHOSTTY_RESOURCES_DIR").is_ok()
        || std::env::var("GHOSTTY_BIN_DIR").is_ok()
        || std::env::var("WEZTERM_EXECUTABLE").is_ok()
        || std::env::var("WEZTERM_PANE").is_ok()
    {
        return ColorCapability::TrueColor;
    }

    if let Ok(term) = std::env::var("TERM") {
        let t = term.to_lowercase();
        if t.contains("kitty") || t.contains("ghostty") || t.contains("alacritty") {
            return ColorCapability::TrueColor;
        }
        if t.contains("256color") {
            return ColorCapability::Color256;
        }
    }

    ColorCapability::Color256
}

pub fn has_truecolor() -> bool {
    color_capability() == ColorCapability::TrueColor
}

/// Returns a truecolor `Color` when the terminal supports it, or a quantized
/// 256-color `Color` (ANSI index) otherwise.
#[inline]
pub fn rgb(r: u8, g: u8, b: u8) -> Color {
    if has_truecolor() {
        Color::rgb(r, g, b)
    } else {
        Color::ansi_index(rgb_to_xterm256(r, g, b))
    }
}

// ---------------------------------------------------------------------------
// 256-color quantization
// ---------------------------------------------------------------------------

/// The xterm-256 color cube: indices 16-231 map to a 6x6x6 RGB cube.
/// Each axis uses values: 0, 95, 135, 175, 215, 255 (indices 0-5).
/// Indices 232-255 are a grayscale ramp from rgb(8,8,8) to rgb(238,238,238).
fn rgb_to_xterm256(r: u8, g: u8, b: u8) -> u8 {
    let gray_avg = (r as u16 + g as u16 + b as u16) / 3;
    let is_grayish = (r as i16 - g as i16).unsigned_abs() < 15
        && (g as i16 - b as i16).unsigned_abs() < 15
        && (r as i16 - b as i16).unsigned_abs() < 15;

    let cube_idx = nearest_cube_index(r, g, b);
    let cube_color = cube_index_to_rgb(cube_idx);
    let cube_dist = color_distance(r, g, b, cube_color.0, cube_color.1, cube_color.2);

    if is_grayish {
        let gray_idx = nearest_gray_index(gray_avg as u8);
        let gray_val = gray_index_to_value(gray_idx);
        let gray_dist = color_distance(r, g, b, gray_val, gray_val, gray_val);

        if gray_dist < cube_dist {
            return 232 + gray_idx;
        }
    }

    cube_idx as u8 + 16
}

const CUBE_VALUES: [u8; 6] = [0, 95, 135, 175, 215, 255];

fn nearest_cube_component(v: u8) -> u8 {
    let mut best = 0u8;
    let mut best_dist = 255u16;
    for (i, &cv) in CUBE_VALUES.iter().enumerate() {
        let d = (v as i16 - cv as i16).unsigned_abs();
        if d < best_dist {
            best_dist = d;
            best = i as u8;
        }
    }
    best
}

fn nearest_cube_index(r: u8, g: u8, b: u8) -> u16 {
    let ri = nearest_cube_component(r) as u16;
    let gi = nearest_cube_component(g) as u16;
    let bi = nearest_cube_component(b) as u16;
    ri * 36 + gi * 6 + bi
}

fn cube_index_to_rgb(idx: u16) -> (u8, u8, u8) {
    let bi = (idx % 6) as usize;
    let gi = ((idx / 6) % 6) as usize;
    let ri = (idx / 36) as usize;
    (CUBE_VALUES[ri], CUBE_VALUES[gi], CUBE_VALUES[bi])
}

fn nearest_gray_index(v: u8) -> u8 {
    if v > 243 {
        return 23;
    }
    (((v as i16 - 8 + 5) / 10).clamp(0, 23)) as u8
}

fn gray_index_to_value(idx: u8) -> u8 {
    8 + idx * 10
}

fn color_distance(r1: u8, g1: u8, b1: u8, r2: u8, g2: u8, b2: u8) -> u32 {
    let dr = r1 as i32 - r2 as i32;
    let dg = g1 as i32 - g2 as i32;
    let db = b1 as i32 - b2 as i32;
    (2 * dr * dr + 4 * dg * dg + 3 * db * db) as u32
}

/// Convert a 256-color palette index back to its approximate RGB value.
pub fn indexed_to_rgb(idx: u8) -> (u8, u8, u8) {
    if idx >= 232 {
        let v = gray_index_to_value(idx - 232);
        (v, v, v)
    } else if idx >= 16 {
        cube_index_to_rgb((idx - 16) as u16)
    } else {
        match idx {
            0 => (0, 0, 0),
            1 => (128, 0, 0),
            2 => (0, 128, 0),
            3 => (128, 128, 0),
            4 => (0, 0, 128),
            5 => (128, 0, 128),
            6 => (0, 128, 128),
            7 => (192, 192, 192),
            8 => (128, 128, 128),
            9 => (255, 0, 0),
            10 => (0, 255, 0),
            11 => (255, 255, 0),
            12 => (0, 0, 255),
            13 => (255, 0, 255),
            14 => (0, 255, 255),
            _ => (255, 255, 255),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pure_black() {
        let idx = rgb_to_xterm256(0, 0, 0);
        assert_eq!(idx, 16);
    }

    #[test]
    fn test_pure_white() {
        let idx = rgb_to_xterm256(255, 255, 255);
        assert_eq!(idx, 231);
    }

    #[test]
    fn test_mid_gray() {
        let idx = rgb_to_xterm256(128, 128, 128);
        assert!(
            (232..=255).contains(&u16::from(idx)),
            "Expected grayscale, got {}",
            idx
        );
    }

    #[test]
    fn test_dim_gray() {
        let idx = rgb_to_xterm256(80, 80, 80);
        assert!(
            (232..=255).contains(&u16::from(idx)),
            "Expected grayscale for dim, got {}",
            idx
        );
    }

    #[test]
    fn test_red() {
        let idx = rgb_to_xterm256(255, 0, 0);
        assert_eq!(idx, 196);
    }

    #[test]
    fn test_green() {
        let idx = rgb_to_xterm256(0, 255, 0);
        assert_eq!(idx, 46);
    }

    #[test]
    fn test_blue() {
        let idx = rgb_to_xterm256(0, 0, 255);
        assert_eq!(idx, 21);
    }

    #[test]
    fn test_near_colors_are_stable() {
        let a = rgb_to_xterm256(80, 80, 80);
        let b = rgb_to_xterm256(82, 82, 82);
        assert_eq!(a, b, "Similar grays should map to same index");
    }

    /// Map a single (r,g,b) the way `rgb()` would under a given capability.
    fn atlas_key_for(cap: ColorCapability, r: u8, g: u8, b: u8) -> u32 {
        match cap {
            ColorCapability::TrueColor => {
                0x0100_0000 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
            }
            ColorCapability::Color256 => rgb_to_xterm256(r, g, b) as u32,
        }
    }

    #[test]
    fn test_glyph_safe_bounds_atlas_keyspace() {
        use std::collections::HashSet;

        let mut truecolor_keys = HashSet::new();
        let mut quantized_keys = HashSet::new();

        let mut samples = 0u32;
        for r in (0..=255u16).step_by(8) {
            for g in (0..=255u16).step_by(8) {
                for b in (0..=255u16).step_by(8) {
                    let (r, g, b) = (r as u8, g as u8, b as u8);
                    truecolor_keys.insert(atlas_key_for(ColorCapability::TrueColor, r, g, b));
                    quantized_keys.insert(atlas_key_for(ColorCapability::Color256, r, g, b));
                    samples += 1;
                }
            }
        }

        assert!(samples > 10_000, "sweep should be dense, got {samples}");
        assert!(
            truecolor_keys.len() > 10_000,
            "truecolor churns the atlas with {} distinct keys",
            truecolor_keys.len()
        );
        assert!(
            quantized_keys.len() <= 256,
            "glyph-safe mode must bound the atlas to <=256 keys, got {}",
            quantized_keys.len()
        );
    }

    #[test]
    fn test_fragile_terminal_override_off_forces_truecolor() {
        temp_env_scope(
            &[
                ("JCODE_GLYPH_SAFE_MODE", Some("off")),
                ("TERM_PROGRAM", Some("vscode")),
            ],
            || {
                assert!(!fragile_glyph_cache_terminal());
            },
        );
    }

    #[test]
    fn test_fragile_terminal_override_on_forces_quantize() {
        temp_env_scope(&[("JCODE_GLYPH_SAFE_MODE", Some("on"))], || {
            assert!(fragile_glyph_cache_terminal());
        });
    }

    #[test]
    fn test_detect_color_capability_downgrades_on_fragile_override() {
        temp_env_scope(
            &[
                ("JCODE_GLYPH_SAFE_MODE", Some("on")),
                ("COLORTERM", Some("truecolor")),
            ],
            || assert_eq!(detect_color_capability(), ColorCapability::Color256),
        );
        temp_env_scope(
            &[
                ("JCODE_GLYPH_SAFE_MODE", Some("off")),
                ("COLORTERM", Some("truecolor")),
            ],
            || assert_eq!(detect_color_capability(), ColorCapability::TrueColor),
        );
    }

    /// Render-path proof: the quantized path produces `Color::ansi_index`
    /// (38;5;<n> wire encoding) and the truecolor path produces `Color::rgb`.
    #[test]
    fn test_indexed_color_serializes_as_256_not_truecolor() {
        let quantized = rgb_via(ColorCapability::Color256, 138, 180, 248);
        assert!(
            quantized.ansi_index_value().is_some(),
            "expected ANSI index, got {quantized:?}"
        );

        let tc = rgb_via(ColorCapability::TrueColor, 138, 180, 248);
        assert!(
            tc.ansi_index_value().is_none(),
            "truecolor must not be an ANSI index, got {tc:?}"
        );
        assert_eq!(tc.rgb_components(), (138, 180, 248));
    }

    fn rgb_via(cap: ColorCapability, r: u8, g: u8, b: u8) -> Color {
        match cap {
            ColorCapability::TrueColor => Color::rgb(r, g, b),
            ColorCapability::Color256 => Color::ansi_index(rgb_to_xterm256(r, g, b)),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_fragile_terminal_detects_vscode_and_apple_terminal() {
        temp_env_scope(
            &[
                ("JCODE_GLYPH_SAFE_MODE", None),
                ("TERM_PROGRAM", Some("vscode")),
            ],
            || assert!(fragile_glyph_cache_terminal()),
        );
        temp_env_scope(
            &[
                ("JCODE_GLYPH_SAFE_MODE", None),
                ("TERM_PROGRAM", Some("Apple_Terminal")),
            ],
            || assert!(fragile_glyph_cache_terminal()),
        );
        temp_env_scope(
            &[
                ("JCODE_GLYPH_SAFE_MODE", None),
                ("TERM_PROGRAM", Some("ghostty")),
            ],
            || assert!(!fragile_glyph_cache_terminal()),
        );
    }

    fn temp_env_scope(vars: &[(&str, Option<&str>)], body: impl FnOnce()) {
        use std::sync::Mutex;
        static ENV_LOCK: Mutex<()> = Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let saved: Vec<(String, Option<String>)> = vars
            .iter()
            .map(|(k, _)| ((*k).to_string(), std::env::var(k).ok()))
            .collect();
        for (k, v) in vars {
            match v {
                Some(val) => unsafe { std::env::set_var(k, val) },
                None => unsafe { std::env::remove_var(k) },
            }
        }

        body();

        for (k, v) in saved {
            match v {
                Some(val) => unsafe { std::env::set_var(&k, val) },
                None => unsafe { std::env::remove_var(&k) },
            }
        }
    }
}
