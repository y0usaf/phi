//! Terminal image display: Kitty / iTerm2 / Sixel / halfblock fallback.

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use image::DynamicImage;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::sync::LazyLock;

use crate::{ImageFit, ImageProtocol};

/// Cache whether ImageMagick is available for Sixel conversion.
pub(crate) static HAS_IMAGEMAGICK: LazyLock<bool> = LazyLock::new(|| {
    Command::new("convert")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
});

/// Display parameters for terminal images.
#[derive(Debug, Clone)]
pub struct ImageDisplayParams {
    /// Maximum width in terminal columns.
    pub max_cols: u16,
    /// Maximum height in terminal rows.
    pub max_rows: u16,
}

impl Default for ImageDisplayParams {
    fn default() -> Self {
        Self {
            max_cols: 80,
            max_rows: 24,
        }
    }
}

impl ImageDisplayParams {
    /// Create display params based on terminal size.
    pub fn from_terminal() -> Self {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((120, 40));

        Self {
            max_cols: (cols * 2 / 3).clamp(40, 100),
            max_rows: (rows / 2).clamp(10, 30),
        }
    }
}

/// Display an image in the terminal.
///
/// Returns Ok(true) if the image was displayed, Ok(false) if not
/// supported, or an error if something went wrong.
pub fn display_image(path: &Path, params: &ImageDisplayParams) -> io::Result<bool> {
    let protocol = ImageProtocol::detect();

    if !protocol.is_supported() {
        return Ok(false);
    }

    let data = std::fs::read(path)?;
    let (img_width, img_height) = get_image_dimensions(&data).unwrap_or((0, 0));

    match protocol {
        ImageProtocol::Kitty => display_kitty(&data, params, img_width, img_height),
        ImageProtocol::ITerm2 => display_iterm2(&data, path, params, img_width, img_height),
        ImageProtocol::Sixel => display_sixel(path, params, img_width, img_height),
        ImageProtocol::Halfblock => display_halfblock(&data, params),
        ImageProtocol::None => Ok(false),
    }
}

/// Get image dimensions from raw data (PNG, JPEG, GIF, WebP).
pub fn get_image_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    // PNG: check signature and parse IHDR chunk
    if data.len() > 24 && &data[0..8] == b"\x89PNG\r\n\x1a\n" {
        let width = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
        let height = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        return Some((width, height));
    }

    // JPEG: look for SOF0/SOF2 markers
    if data.len() > 2 && data[0] == 0xFF && data[1] == 0xD8 {
        let mut i = 2;
        while i + 9 < data.len() {
            if data[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = data[i + 1];
            if marker == 0xC0 || marker == 0xC2 {
                let height = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
                let width = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
                return Some((width, height));
            }
            if i + 3 < data.len() {
                let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
                i += 2 + len;
            } else {
                break;
            }
        }
    }

    // GIF: parse header
    if data.len() > 10 && (&data[0..6] == b"GIF87a" || &data[0..6] == b"GIF89a") {
        let width = u16::from_le_bytes([data[6], data[7]]) as u32;
        let height = u16::from_le_bytes([data[8], data[9]]) as u32;
        return Some((width, height));
    }

    // WebP: parse RIFF header
    if data.len() > 30 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        if &data[12..16] == b"VP8 " && data.len() > 30 {
            if data[23] == 0x9D && data[24] == 0x01 && data[25] == 0x2A {
                let width = (u16::from_le_bytes([data[26], data[27]]) & 0x3FFF) as u32;
                let height = (u16::from_le_bytes([data[28], data[29]]) & 0x3FFF) as u32;
                return Some((width, height));
            }
        }
        if &data[12..16] == b"VP8L" && data.len() > 25 {
            let bits = u32::from_be_bytes([data[21], data[22], data[23], data[24]]);
            let width = (bits & 0x3FFF) + 1;
            let height = ((bits >> 14) & 0x3FFF) + 1;
            return Some((width, height));
        }
    }

    None
}

/// Calculate display size maintaining aspect ratio.
pub fn calculate_display_size(
    img_width: u32,
    img_height: u32,
    max_cols: u16,
    max_rows: u16,
) -> ImageFit {
    if img_width == 0 || img_height == 0 {
        return ImageFit {
            cols: max_cols.min(40),
            rows: max_rows.min(20),
        };
    }

    let cell_aspect = 2.0;
    let img_aspect = img_width as f64 / img_height as f64;
    let max_width = max_cols as f64;
    let max_height = max_rows as f64 * cell_aspect;

    let (display_width, display_height) = if img_aspect > max_width / max_height {
        (max_width, max_width / img_aspect)
    } else {
        (max_height * img_aspect, max_height)
    };

    ImageFit {
        cols: (display_width as u16).max(1),
        rows: ((display_height / cell_aspect) as u16).max(1),
    }
}

/// Decode image data using the `image` crate.
pub fn decode_image(data: &[u8]) -> io::Result<DynamicImage> {
    image::load_from_memory(data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Display image using Kitty graphics protocol.
fn display_kitty(
    data: &[u8],
    params: &ImageDisplayParams,
    img_width: u32,
    img_height: u32,
) -> io::Result<bool> {
    let fit = calculate_display_size(img_width, img_height, params.max_cols, params.max_rows);
    let encoded = BASE64.encode(data);
    let mut stdout = io::stdout().lock();

    const CHUNK_SIZE: usize = 4096;
    let chunks: Vec<&str> = encoded
        .as_bytes()
        .chunks(CHUNK_SIZE)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect();

    for (i, chunk) in chunks.iter().enumerate() {
        let is_first = i == 0;
        let is_last = i == chunks.len() - 1;
        let more = if is_last { 0 } else { 1 };

        if is_first {
            write!(
                stdout,
                "\x1b_Ga=T,f=100,c={},r={},m={};{}\x1b\\",
                fit.cols, fit.rows, more, chunk
            )?;
        } else {
            write!(stdout, "\x1b_Gm={};{}\x1b\\", more, chunk)?;
        }
    }

    writeln!(stdout)?;
    stdout.flush()?;
    Ok(true)
}

/// Display image using iTerm2 inline image protocol.
fn display_iterm2(
    data: &[u8],
    path: &Path,
    params: &ImageDisplayParams,
    img_width: u32,
    img_height: u32,
) -> io::Result<bool> {
    let fit = calculate_display_size(img_width, img_height, params.max_cols, params.max_rows);
    let encoded = BASE64.encode(data);
    let filename = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "image".to_string());
    let filename_b64 = BASE64.encode(filename.as_bytes());

    let mut stdout = io::stdout().lock();

    write!(
        stdout,
        "\x1b]1337;File=name={};size={};inline=1;width={}:{}\x07",
        filename_b64,
        data.len(),
        fit.cols,
        encoded
    )?;

    writeln!(stdout)?;
    stdout.flush()?;
    Ok(true)
}

/// Display image using Sixel graphics protocol.
fn display_sixel(
    path: &Path,
    params: &ImageDisplayParams,
    img_width: u32,
    img_height: u32,
) -> io::Result<bool> {
    if !*HAS_IMAGEMAGICK {
        return Ok(false);
    }

    let fit = calculate_display_size(img_width, img_height, params.max_cols, params.max_rows);
    let pixel_width = (fit.cols as u32) * 8;
    let pixel_height = (fit.rows as u32) * 16;

    let output = Command::new("convert")
        .arg(path)
        .arg("-geometry")
        .arg(format!("{}x{}>", pixel_width, pixel_height))
        .arg("-colors")
        .arg("256")
        .arg("sixel:-")
        .output()?;

    if !output.status.success() {
        return Ok(false);
    }

    let mut stdout = io::stdout().lock();
    stdout.write_all(&output.stdout)?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(true)
}

/// Display image using halfblock rendering (universal fallback).
///
/// Decodes the image, resizes to fit the terminal, and renders using
/// Unicode half-block characters with 24-bit ANSI colors.
pub fn display_halfblock(data: &[u8], params: &ImageDisplayParams) -> io::Result<bool> {
    let img = decode_image(data)?;
    let rgb = img.to_rgb8();
    let (iw, ih) = rgb.dimensions();

    let fit = calculate_display_size(iw, ih, params.max_cols, params.max_rows);
    let target_w = fit.cols.max(1) as u32;
    let target_h = (fit.rows.max(1) as u32) * 2;

    let resized = image::imageops::resize(
        &rgb,
        target_w,
        target_h,
        image::imageops::FilterType::Nearest,
    );

    let mut stdout = io::stdout().lock();

    let mut y = 0u32;
    while y < target_h {
        let lower_y = (y + 1).min(target_h - 1);
        for x in 0..target_w {
            let upper = resized.get_pixel(x, y);
            let lower = resized.get_pixel(x, lower_y);
            write!(
                stdout,
                "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m\u{2580}",
                upper.0[0], upper.0[1], upper.0[2],
                lower.0[0], lower.0[1], lower.0[2]
            )?;
        }
        write!(stdout, "\x1b[0m")?;
        writeln!(stdout)?;
        y += 2;
    }

    stdout.flush()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protocol_detection() {
        let protocol = ImageProtocol::detect();
        println!("Detected protocol: {:?}", protocol);
    }

    #[test]
    fn test_calculate_display_size() {
        let fit = calculate_display_size(1920, 1080, 80, 24);
        assert!(fit.cols <= 80);
        assert!(fit.rows <= 24);

        let fit = calculate_display_size(1080, 1920, 80, 24);
        assert!(fit.cols <= 80);
        assert!(fit.rows <= 24);

        let fit = calculate_display_size(500, 500, 80, 24);
        assert!(fit.cols <= 80);
        assert!(fit.rows <= 24);
    }

    #[test]
    fn test_get_image_dimensions_png() {
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00\x90wS\xde";
        let dims = get_image_dimensions(png);
        assert_eq!(dims, Some((1, 1)));
    }
}
