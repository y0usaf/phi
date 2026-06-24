//! Inline raster image support for phi.
//!
//! Provides terminal image rendering via:
//! - Kitty graphics protocol (Kitty, Ghostty)
//! - iTerm2 inline images
//! - Sixel graphics (xterm, foot, mlterm, WezTerm)
//! - Halfblock fallback (any terminal with ANSI 24-bit colors)
//!
//! Images are lazy-decoded only when on-screen, with a configurable
//! prefetch band and a stable fit cache to avoid recomputing display
//! dimensions on every render.

pub mod cache;
pub mod display;
pub mod metadata;

pub use cache::{FitCache, ImageCache, ImageEntry, PrefetchConfig};
pub use display::{display_halfblock, display_image, ImageDisplayParams};
pub use metadata::{
    aspect_ratio, compact_image_format, compact_path_label, estimate_base64_decoded_len,
    format_byte_count, format_dimensions,
};

use std::path::PathBuf;

/// Terminal image protocol support level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    /// Kitty graphics protocol (most feature-rich).
    Kitty,
    /// iTerm2 inline images.
    ITerm2,
    /// Sixel graphics (xterm, foot, mlterm, WezTerm).
    Sixel,
    /// Halfblock rendering via ANSI 24-bit colors (universal fallback).
    Halfblock,
    /// No image support at all.
    None,
}

impl Default for ImageProtocol {
    fn default() -> Self {
        Self::detect()
    }
}

impl ImageProtocol {
    /// Detect the best available image protocol for the current terminal.
    pub fn detect() -> Self {
        // Check for Kitty first (most capable)
        if std::env::var("KITTY_WINDOW_ID").is_ok() {
            return Self::Kitty;
        }

        // Check TERM for kitty or ghostty
        if let Ok(term) = std::env::var("TERM")
            && (term.contains("kitty") || term.contains("ghostty"))
        {
            return Self::Kitty;
        }

        // Check TERM_PROGRAM for Ghostty / iTerm2 / WezTerm
        if let Ok(term_program) = std::env::var("TERM_PROGRAM") {
            if term_program == "ghostty" {
                return Self::Kitty;
            }
            if term_program == "iTerm.app" {
                return Self::ITerm2;
            }
            // WezTerm supports Sixel
            if term_program == "WezTerm" {
                return Self::Sixel;
            }
        }

        // Check LC_TERMINAL for iTerm2
        if let Ok(lc_terminal) = std::env::var("LC_TERMINAL")
            && lc_terminal == "iTerm2"
        {
            return Self::ITerm2;
        }

        // Check for Sixel-capable terminals
        if Self::detect_sixel() {
            return Self::Sixel;
        }

        // Halfblock is always available as a fallback (needs 24-bit color)
        Self::Halfblock
    }

    /// Detect if terminal supports Sixel graphics.
    fn detect_sixel() -> bool {
        if !*display::HAS_IMAGEMAGICK {
            return false;
        }

        if let Ok(term) = std::env::var("TERM") {
            let term_lower = term.to_lowercase();
            // Known Sixel-capable terminals
            if term_lower.contains("xterm")
                || term_lower.contains("foot")
                || term_lower.contains("mlterm")
                || term_lower.contains("yaft")
                || term_lower.contains("mintty")
                || term_lower.contains("contour")
            {
                return true;
            }
        }

        // Check TERM_PROGRAM for other Sixel terminals
        if let Ok(prog) = std::env::var("TERM_PROGRAM")
            && (prog == "mintty" || prog == "contour")
        {
            return true;
        }

        false
    }

    /// Check if image display is supported (anything other than None).
    pub fn is_supported(&self) -> bool {
        *self != Self::None
    }
}

/// Source of image data - either a file path or raw bytes with a format hint.
#[derive(Debug, Clone)]
pub enum ImageSource {
    /// Load from a file path.
    Path(PathBuf),
    /// Raw image bytes with an optional format/mime hint.
    Bytes {
        data: Vec<u8>,
        format_hint: Option<String>,
    },
}

impl ImageSource {
    /// Read the raw bytes of this image source.
    pub fn read_bytes(&self) -> std::io::Result<Vec<u8>> {
        match self {
            Self::Path(p) => std::fs::read(p),
            Self::Bytes { data, .. } => Ok(data.clone()),
        }
    }

    /// Best-effort filename for display purposes.
    pub fn label(&self) -> String {
        match self {
            Self::Path(p) => p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("image")
                .to_string(),
            Self::Bytes { .. } => "image".to_string(),
        }
    }
}

/// Stable identifier for a placed image in the scrollback.
pub type ImageId = u64;

/// Computed fit: display dimensions after aspect-ratio adjustment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImageFit {
    /// Display width in terminal columns.
    pub cols: u16,
    /// Display height in terminal rows.
    pub rows: u16,
}
