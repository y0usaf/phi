//! Mermaid diagram rendering for terminal display.
//!
//! Simplified port of jcode's mermaid rendering crate. Provides:
//! - LRU render cache for diagram PNGs (in-memory metadata + on-disk files)
//! - Kitty graphics protocol and halfblock fallback for terminal image rendering
//! - Viewport crop/zoom for navigating large diagrams
//! - Terminal capability detection
//!
//! The actual Mermaid -> SVG -> PNG conversion is expected to be done externally
//! (e.g. by shelling out to `mmdc`); this crate handles caching and display.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use base64::Engine;
use image::{DynamicImage, Rgba, imageops};

// ---------------------------------------------------------------------------
// DiagramInfo & CachedDiagram
// ---------------------------------------------------------------------------

/// Metadata about a rendered diagram.
#[derive(Debug, Clone)]
pub struct DiagramInfo {
    /// Content hash for cache lookup.
    pub hash: u64,
    /// Original PNG width in pixels.
    pub width: u32,
    /// Original PNG height in pixels.
    pub height: u32,
    /// Optional label/title.
    pub label: Option<String>,
}

/// A cached diagram entry: the on-disk PNG path and its pixel dimensions.
#[derive(Clone)]
pub struct CachedDiagram {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
}

// ---------------------------------------------------------------------------
// LRU render cache
// ---------------------------------------------------------------------------

/// Maximum in-memory cache entries (metadata only, not pixel data).
pub const RENDER_CACHE_MAX: usize = 64;

/// Reuse a cached PNG only if it is at least this fraction of requested width.
pub const CACHE_WIDTH_MATCH_PERCENT: u32 = 85;

/// Quantize requested render widths so tiny pane-width changes reuse the same
/// cached entry.
pub const RENDER_WIDTH_BUCKET_CELLS: u32 = 4;

/// Mermaid rendering cache: in-memory metadata index with LRU eviction,
/// backed by on-disk PNG files.
pub struct MermaidCache {
    entries: HashMap<u64, CachedDiagram>,
    order: VecDeque<u64>,
    cache_dir: PathBuf,
}

impl MermaidCache {
    /// Create a new cache rooted at `<cache_dir>/phi/mermaid`.
    pub fn new() -> Self {
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("phi")
            .join("mermaid");
        let _ = fs::create_dir_all(&cache_dir);
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            cache_dir,
        }
    }

    /// Create a cache with an explicit directory (useful for tests).
    pub fn with_dir(cache_dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&cache_dir);
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            cache_dir,
        }
    }

    /// Compute the path where a diagram PNG of the given hash and target width
    /// would be cached.
    pub fn cache_path(&self, hash: u64, target_width: u32) -> PathBuf {
        self.cache_dir
            .join(format!("{:016x}_w{}.png", hash, target_width))
    }

    /// Touch an entry, moving it to the back of the LRU queue.
    fn touch(&mut self, key: u64) {
        if let Some(pos) = self.order.iter().position(|e| *e == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key);
    }

    /// Look up a cached diagram by hash, optionally requiring a minimum width.
    pub fn get(&self, hash: u64, min_width: Option<u32>) -> Option<CachedDiagram> {
        let entry = self.entries.get(&hash)?;
        if !cached_width_satisfies(entry.width, min_width) {
            return None;
        }
        if !entry.path.exists() {
            return None;
        }
        Some(entry.clone())
    }

    /// Attempt to discover a cached diagram on disk that is not in memory yet.
    pub fn discover_on_disk(&self, hash: u64, min_width: Option<u32>) -> Option<CachedDiagram> {
        let entries = fs::read_dir(&self.cache_dir).ok()?;
        let mut best: Option<(PathBuf, u32)> = None;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let prefix = format!("{:016x}", hash);
            if !name.starts_with(&prefix) || !name.ends_with(".png") {
                continue;
            }
            if let Some(w) = parse_width_from_name(&name) {
                if cached_width_satisfies(w, min_width) {
                    if best.as_ref().map_or(true, |(_, bw)| w > *bw) {
                        let path = entry.path();
                        if path.exists() {
                            best = Some((path, w));
                        }
                    }
                }
            }
        }
        let (path, width) = best?;
        let img = image::open(&path).ok()?;
        let height = img.height();
        Some(CachedDiagram { path, width, height })
    }

    /// Insert a diagram into the cache, evicting the least-recently-used entry
    /// when the cache is full.
    pub fn insert(&mut self, hash: u64, diagram: CachedDiagram) {
        if self.entries.contains_key(&hash) {
            self.touch(hash);
            self.entries.insert(hash, diagram);
            return;
        }
        self.entries.insert(hash, diagram);
        self.order.push_back(hash);
        while self.order.len() > RENDER_CACHE_MAX {
            if let Some(old) = self.order.pop_front() {
                self.entries.remove(&old);
            }
        }
    }

    /// Number of entries currently in the cache.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Cache directory path.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Clear all in-memory entries (does not delete on-disk files).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
}

impl Default for MermaidCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if a cached width is acceptable for a requested minimum width.
fn cached_width_satisfies(cached: u32, min_width: Option<u32>) -> bool {
    match min_width {
        None => true,
        Some(min) => cached as u32 * 100 >= min as u32 * CACHE_WIDTH_MATCH_PERCENT,
    }
}

/// Parse the width value from a cache filename like `<hash>_w<W>.png`.
fn parse_width_from_name(name: &str) -> Option<u32> {
    let start = name.rfind("_w")?;
    let end = name.rfind(".png")?;
    let num_str = &name[start + 2..end];
    num_str.parse().ok()
}

/// Compute a content hash for a Mermaid diagram source string.
pub fn diagram_hash(source: &str) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}

/// Quantize a cell width to a render-width bucket to reduce cache fragmentation.
pub fn bucket_render_width(cell_width: u16, font_width: u16) -> u32 {
    let raw = (cell_width as u32).saturating_mul(font_width.max(1) as u32);
    let bucket = RENDER_WIDTH_BUCKET_CELLS.max(1);
    (raw / bucket) * bucket
}

// ---------------------------------------------------------------------------
// Source image cache (decoded DynamicImage, keyed by hash)
// ---------------------------------------------------------------------------

/// Maximum decoded source images held in memory.
const SOURCE_CACHE_MAX: usize = 16;

/// In-memory cache of decoded source images, keyed by diagram hash.
struct SourceCache {
    entries: HashMap<u64, Arc<DynamicImage>>,
    order: VecDeque<u64>,
}

impl SourceCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&self, hash: u64) -> Option<Arc<DynamicImage>> {
        self.entries.get(&hash).cloned()
    }

    fn insert(&mut self, hash: u64, img: Arc<DynamicImage>) {
        if !self.entries.contains_key(&hash) {
            self.entries.insert(hash, img);
            self.order.push_back(hash);
            while self.order.len() > SOURCE_CACHE_MAX {
                if let Some(old) = self.order.pop_front() {
                    self.entries.remove(&old);
                }
            }
        }
    }
}

static SOURCE_CACHE: LazyLock<Mutex<SourceCache>> =
    LazyLock::new(|| Mutex::new(SourceCache::new()));

/// Load a source image from disk, using the in-memory cache when available.
pub fn load_source_image(hash: u64, path: &Path) -> Option<Arc<DynamicImage>> {
    if let Ok(cache) = SOURCE_CACHE.lock() {
        if let Some(img) = cache.get(hash) {
            return Some(img);
        }
    }
    let img = image::open(path).ok()?;
    let arc = Arc::new(img);
    if let Ok(mut cache) = SOURCE_CACHE.lock() {
        cache.insert(hash, arc.clone());
    }
    Some(arc)
}

// ---------------------------------------------------------------------------
// Terminal capability detection
// ---------------------------------------------------------------------------

/// Terminal graphics protocol support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalProtocol {
    /// Kitty graphics protocol (virtual placements, RGBA transmission).
    Kitty,
    /// Halfblock rendering (two pixels per terminal cell via upper half block).
    Halfblock,
}

/// Detect terminal graphics support by querying environment variables.
pub fn detect_protocol() -> TerminalProtocol {
    if supports_kitty_graphics() {
        TerminalProtocol::Kitty
    } else {
        TerminalProtocol::Halfblock
    }
}

/// Whether the terminal advertises Kitty graphics protocol support.
pub fn supports_kitty_graphics() -> bool {
    let term = std::env::var("TERM").unwrap_or_default();
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();

    if term_program == "ghostty"
        || term_program == "WezTerm"
        || term == "xterm-kitty"
        || term == "ghostty"
    {
        return true;
    }
    false
}

/// Detect font cell size (cols, rows, cell_width_px, cell_height_px).
pub fn detect_font_size() -> (u16, u16, u16, u16) {
    let cols = std::env::var("COLUMNS").ok().and_then(|s| s.parse().ok()).unwrap_or(80) as u16;
    let rows = std::env::var("LINES").ok().and_then(|s| s.parse().ok()).unwrap_or(24) as u16;
    (cols, rows, 8, 16)
}

/// Check if running inside tmux.
pub fn is_tmux() -> bool {
    std::env::var("TERM").is_ok_and(|t| t.starts_with("tmux"))
        || std::env::var("TERM_PROGRAM").is_ok_and(|t| t == "tmux")
}

// ---------------------------------------------------------------------------
// Halfblock rendering
// ---------------------------------------------------------------------------

/// Halfblock renderer: converts a region of a `DynamicImage` into a grid of
/// half-block cells, each representing two vertical pixels.
pub struct HalfblockRenderer {
    image: Arc<DynamicImage>,
    scroll_x: u32,
    scroll_y: u32,
    zoom_percent: u8,
}

impl HalfblockRenderer {
    /// Create a new halfblock renderer for the given image.
    pub fn new(image: Arc<DynamicImage>) -> Self {
        Self {
            image,
            scroll_x: 0,
            scroll_y: 0,
            zoom_percent: 100,
        }
    }

    /// Set horizontal scroll offset in pixels.
    pub fn scroll_x(mut self, x: u32) -> Self {
        self.scroll_x = x;
        self
    }

    /// Set vertical scroll offset in pixels.
    pub fn scroll_y(mut self, y: u32) -> Self {
        self.scroll_y = y;
        self
    }

    /// Set zoom percentage (50-200). 100 = original size.
    pub fn zoom(mut self, percent: u8) -> Self {
        self.zoom_percent = percent.clamp(50, 200);
        self
    }

    /// Get the (possibly zoomed) image.
    fn source_image(&self) -> DynamicImage {
        let zoom = self.zoom_percent as u32;
        if zoom == 100 {
            return (*self.image).clone();
        }
        let w = (self.image.width() as u64 * zoom as u64 / 100).max(1) as u32;
        let h = (self.image.height() as u64 * zoom as u64 / 100).max(1) as u32;
        self.image.resize_exact(w, h, imageops::FilterType::Nearest)
    }

    /// Render a region of the image as halfblock lines.
    ///
    /// Returns a vector of strings, one per terminal row. Each cell uses
    /// upper-half-block to represent two pixels vertically.
    /// Colors are set via ANSI 24-bit escape codes.
    pub fn render(&self, cols: u16, rows: u16) -> Vec<String> {
        let src = self.source_image();
        let rgba = src.to_rgba8();
        let (img_w, img_h) = (rgba.width(), rgba.height());
        let cols = cols as u32;
        let rows = rows as u32;

        let mut lines = Vec::with_capacity(rows as usize);
        for row in 0..rows {
            let py_top = self.scroll_y + row * 2;
            let py_bot = py_top + 1;
            let mut line = String::new();
            for col in 0..cols {
                let px = self.scroll_x + col;
                let top = if px < img_w && py_top < img_h {
                    Some(*rgba.get_pixel(px, py_top))
                } else {
                    None
                };
                let bot = if px < img_w && py_bot < img_h {
                    Some(*rgba.get_pixel(px, py_bot))
                } else {
                    None
                };
                let (ch, fg, bg) = halfblock_cell(top, bot);
                match (fg, bg) {
                    (Some(f), Some(b)) => {
                        line.push_str(&format!(
                            "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m{}",
                            f[0], f[1], f[2], b[0], b[1], b[2], ch
                        ));
                    }
                    (Some(f), None) => {
                        line.push_str(&format!(
                            "\x1b[38;2;{};{};{}m{}",
                            f[0], f[1], f[2], ch
                        ));
                    }
                    (None, Some(b)) => {
                        line.push_str(&format!(
                            "\x1b[48;2;{};{};{}m ",
                            b[0], b[1], b[2]
                        ));
                    }
                    _ => {
                        line.push(' ');
                    }
                }
            }
            line.push_str("\x1b[0m");
            lines.push(line);
        }
        lines
    }

    /// Render as a grid of `HalfblockCell` structs (for programmatic use).
    pub fn render_cells(&self, cols: u16, rows: u16) -> Vec<Vec<HalfblockCell>> {
        let src = self.source_image();
        let rgba = src.to_rgba8();
        let (img_w, img_h) = (rgba.width(), rgba.height());
        let cols = cols as u32;
        let rows = rows as u32;

        let mut grid = Vec::with_capacity(rows as usize);
        for row in 0..rows {
            let py_top = self.scroll_y + row * 2;
            let py_bot = py_top + 1;
            let mut line = Vec::with_capacity(cols as usize);
            for col in 0..cols {
                let px = self.scroll_x + col;
                let top = if px < img_w && py_top < img_h {
                    Some(*rgba.get_pixel(px, py_top))
                } else {
                    None
                };
                let bot = if px < img_w && py_bot < img_h {
                    Some(*rgba.get_pixel(px, py_bot))
                } else {
                    None
                };
                line.push(HalfblockCell::from_pixels(top, bot));
            }
            grid.push(line);
        }
        grid
    }
}

/// A single halfblock cell: foreground and background colors.
#[derive(Debug, Clone, Copy, Default)]
pub struct HalfblockCell {
    /// Foreground color (top pixel).
    pub fg: Option<Rgba<u8>>,
    /// Background color (bottom pixel).
    pub bg: Option<Rgba<u8>>,
}

impl HalfblockCell {
    /// Create from optional top/bottom pixel values.
    fn from_pixels(top: Option<Rgba<u8>>, bot: Option<Rgba<u8>>) -> Self {
        Self { fg: top, bg: bot }
    }

    /// Render this cell as an ANSI escape sequence + character.
    pub fn to_ansi(&self) -> String {
        match (self.fg, self.bg) {
            (Some(f), Some(b)) => format!(
                "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m\u{2580}",
                f[0], f[1], f[2], b[0], b[1], b[2]
            ),
            (Some(f), None) => format!("\x1b[38;2;{};{};{}m\u{2580}", f[0], f[1], f[2]),
            (None, Some(b)) => format!("\x1b[48;2;{};{};{}m ", b[0], b[1], b[2]),
            (None, None) => " ".to_string(),
        }
    }
}

/// Compute the character and fg/bg colors for a halfblock cell.
fn halfblock_cell(
    top: Option<Rgba<u8>>,
    bot: Option<Rgba<u8>>,
) -> (char, Option<Rgba<u8>>, Option<Rgba<u8>>) {
    match (top, bot) {
        (Some(t), Some(b)) if t == b => ('\u{2588}', Some(t), None),
        (Some(t), Some(b)) => ('\u{2580}', Some(t), Some(b)),
        (Some(t), None) => ('\u{2580}', Some(t), None),
        (None, Some(b)) => ('\u{2584}', None, Some(b)),
        (None, None) => (' ', None, None),
    }
}

// ---------------------------------------------------------------------------
// Kitty graphics protocol rendering
// ---------------------------------------------------------------------------

/// Kitty graphics renderer: transmits an image via the Kitty graphics
/// protocol using base64-encoded RGBA chunks.
pub struct KittyRenderer {
    image: Arc<DynamicImage>,
    unique_id: u32,
    zoom_percent: u8,
}

impl KittyRenderer {
    /// Create a new Kitty renderer with a unique image ID derived from a hash.
    pub fn new(image: Arc<DynamicImage>, hash: u64) -> Self {
        let unique_id = ((hash as u32) ^ ((hash >> 32) as u32) ^ 0x4B49_5459).max(1);
        Self {
            image,
            unique_id,
            zoom_percent: 100,
        }
    }

    /// Set zoom percentage (50-200).
    pub fn zoom(mut self, percent: u8) -> Self {
        self.zoom_percent = percent.clamp(50, 200);
        self
    }

    /// Build the Kitty graphics protocol escape sequence to transmit the image
    /// (a=T action: transmit + create a virtual placement).
    ///
    /// The image is sent as base64-encoded RGBA32 data in chunks.
    pub fn transmit(&self) -> String {
        let img = self.scaled_image();
        let (w, h) = (img.width(), img.height());
        let rgba = img.to_rgba8();
        let bytes = rgba.as_raw();

        let (start, escape, end) = tmux_wrap();
        let mut data = String::from(start);

        let chunk_size = 4096 / 4 * 3;
        let chunks: Vec<&[u8]> = bytes.chunks(chunk_size).collect();
        let chunk_count = chunks.len();

        for (i, chunk) in chunks.iter().enumerate() {
            let payload = base64::engine::general_purpose::STANDARD.encode(chunk);
            data.push_str(escape);
            match i {
                0 => {
                    let more = if chunk_count > 1 { 1 } else { 0 };
                    data.push_str(&format!(
                        "_Gq=2,i={},a=T,U=1,f=32,t=d,s={},v={},m={};{}",
                        self.unique_id, w, h, more, payload
                    ));
                }
                n if n + 1 == chunk_count => {
                    data.push_str(&format!("_Gq=2,m=0;{}", payload));
                }
                _ => {
                    data.push_str(&format!("_Gq=2,m=1;{}", payload));
                }
            }
            data.push_str(escape);
            data.push('\\');
        }
        data.push_str(end);
        data
    }

    /// Build a Kitty delete command for this image's unique ID.
    pub fn delete(&self) -> String {
        format!("\x1b_Ga=d,i={}\x1b\\", self.unique_id)
    }

    /// Get the unique image ID.
    pub fn id(&self) -> u32 {
        self.unique_id
    }

    /// Get the (possibly zoomed) image.
    fn scaled_image(&self) -> DynamicImage {
        let zoom = self.zoom_percent as u32;
        if zoom == 100 {
            return (*self.image).clone();
        }
        let w = (self.image.width() as u64 * zoom as u64 / 100).max(1) as u32;
        let h = (self.image.height() as u64 * zoom as u64 / 100).max(1) as u32;
        self.image.resize_exact(w, h, imageops::FilterType::Nearest)
    }
}

/// Get tmux passthrough escape wrappers if running inside tmux.
fn tmux_wrap() -> (&'static str, &'static str, &'static str) {
    if is_tmux() {
        ("\x1bPtmux;", "\x1b", "\x1b\\")
    } else {
        ("", "\x1b", "\x1b\\")
    }
}

// ---------------------------------------------------------------------------
// Viewport
// ---------------------------------------------------------------------------

/// Viewport state for navigating a diagram: scroll offset and zoom level.
#[derive(Debug, Clone, Copy, Default)]
pub struct Viewport {
    /// Horizontal scroll offset in pixels.
    pub scroll_x: u32,
    /// Vertical scroll offset in pixels.
    pub scroll_y: u32,
    /// Zoom percentage (50-200). 100 = 1:1.
    pub zoom_percent: u8,
}

impl Viewport {
    /// Create a new viewport at origin with 100% zoom.
    pub fn new() -> Self {
        Self::default()
    }

    /// Scroll the viewport by (dx, dy) pixels, clamped to image bounds.
    pub fn scroll_by(&mut self, dx: i32, dy: i32, img_w: u32, img_h: u32) {
        self.scroll_x = (self.scroll_x as i32 + dx)
            .max(0)
            .min(img_w as i32 - 1)
            .max(0) as u32;
        self.scroll_y = (self.scroll_y as i32 + dy)
            .max(0)
            .min(img_h as i32 - 1)
            .max(0) as u32;
    }

    /// Set zoom, keeping the center stable when possible.
    pub fn set_zoom(&mut self, percent: u8, img_w: u32, img_h: u32) {
        let old_zoom = self.zoom_percent.max(1) as f32;
        self.zoom_percent = percent.clamp(50, 200);
        let new_zoom = self.zoom_percent as f32;
        let ratio = old_zoom / new_zoom;
        self.scroll_x = ((self.scroll_x as f32 * ratio) as u32).min(img_w.saturating_sub(1));
        self.scroll_y = ((self.scroll_y as f32 * ratio) as u32).min(img_h.saturating_sub(1));
    }

    /// Compute the scaled image dimensions for the current zoom.
    pub fn scaled_dims(&self, w: u32, h: u32) -> (u32, u32) {
        let z = self.zoom_percent.max(1) as u64;
        (
            (w as u64 * z / 100).max(1) as u32,
            (h as u64 * z / 100).max(1) as u32,
        )
    }

    /// Whether the viewport should scale to fit (1:1 crop vs fit-to-area).
    pub fn should_scale_to_area(crop_w: u32, crop_h: u32, view_w: u32, view_h: u32) -> bool {
        crop_w == view_w && crop_h == view_h
    }
}

// ---------------------------------------------------------------------------
// Diagram renderer: ties cache + protocol + viewport together
// ---------------------------------------------------------------------------

/// Top-level diagram renderer that dispatches to the appropriate protocol.
pub struct DiagramRenderer {
    protocol: TerminalProtocol,
    viewport: Viewport,
    font_size: (u16, u16),
}

impl DiagramRenderer {
    /// Create a renderer with auto-detected protocol.
    pub fn new() -> Self {
        Self {
            protocol: detect_protocol(),
            viewport: Viewport::new(),
            font_size: (8, 16),
        }
    }

    /// Create a renderer with an explicit protocol.
    pub fn with_protocol(protocol: TerminalProtocol) -> Self {
        Self {
            protocol,
            viewport: Viewport::new(),
            font_size: (8, 16),
        }
    }

    /// Set the font cell size in pixels (width, height).
    pub fn font_size(mut self, w: u16, h: u16) -> Self {
        self.font_size = (w, h);
        self
    }

    /// Get the active protocol.
    pub fn protocol(&self) -> TerminalProtocol {
        self.protocol
    }

    /// Get a reference to the viewport.
    pub fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    /// Get a mutable reference to the viewport.
    pub fn viewport_mut(&mut self) -> &mut Viewport {
        &mut self.viewport
    }

    /// Render a diagram image to a render output.
    /// Uses the Kitty protocol or halfblock depending on terminal support.
    pub fn render(&self, image: Arc<DynamicImage>, cols: u16, rows: u16) -> RenderOutput {
        match self.protocol {
            TerminalProtocol::Kitty => {
                let kitty = KittyRenderer::new(image, self.viewport.zoom_percent as u64 ^ 0xDEAD)
                    .zoom(self.viewport.zoom_percent);
                let transmission = kitty.transmit();
                let _ = cols;
                let _ = rows;
                RenderOutput::Kitty {
                    transmission,
                    unique_id: kitty.id(),
                    cols,
                    rows,
                }
            }
            TerminalProtocol::Halfblock => {
                let renderer = HalfblockRenderer::new(image)
                    .scroll_x(self.viewport.scroll_x)
                    .scroll_y(self.viewport.scroll_y)
                    .zoom(self.viewport.zoom_percent);
                let lines = renderer.render(cols, rows);
                RenderOutput::Halfblock { lines }
            }
        }
    }
}

impl Default for DiagramRenderer {
    fn default() -> Self {
        Self::new()
    }
}

/// Output from rendering a diagram.
#[derive(Debug)]
pub enum RenderOutput {
    /// Kitty graphics protocol output: the transmission escape sequence and
    /// placement metadata.
    Kitty {
        transmission: String,
        unique_id: u32,
        cols: u16,
        rows: u16,
    },
    /// Halfblock output: one ANSI string per terminal row.
    Halfblock {
        lines: Vec<String>,
    },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, RgbaImage};

    fn make_test_image(w: u32, h: u32) -> DynamicImage {
        let img: RgbaImage = ImageBuffer::from_fn(w, h, |x, y| {
            Rgba([(x * 4 % 255) as u8, (y * 4 % 255) as u8, 128, 255])
        });
        DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn test_cache_basic() {
        let dir = std::env::temp_dir().join("phi_mermaid_test");
        let _ = fs::remove_dir_all(&dir);
        let mut cache = MermaidCache::with_dir(dir.clone());
        assert_eq!(cache.len(), 0);
        let hash = 42u64;
        let dummy_path = dir.join("test.png");
        let _ = fs::write(&dummy_path, b"fake");
        cache.insert(hash, CachedDiagram { path: dummy_path.clone(), width: 800, height: 600 });
        assert_eq!(cache.len(), 1);
        let got = cache.get(hash, Some(700)).expect("should find cached");
        assert_eq!(got.width, 800);
        assert!(cache.get(hash, Some(1000)).is_none());
    }

    #[test]
    fn test_cache_lru_eviction() {
        let dir = std::env::temp_dir().join("phi_mermaid_lru_test");
        let _ = fs::remove_dir_all(&dir);
        let mut cache = MermaidCache::with_dir(dir);
        for i in 0..RENDER_CACHE_MAX + 5 {
            let path = std::env::temp_dir().join(format!("phi_mermaid_lru_test/{}.png", i));
            let _ = fs::write(&path, b"fake");
            cache.insert(i as u64, CachedDiagram { path, width: 100, height: 100 });
        }
        assert_eq!(cache.len(), RENDER_CACHE_MAX);
    }

    #[test]
    fn test_halfblock_render() {
        let img = make_test_image(10, 10);
        let renderer = HalfblockRenderer::new(Arc::new(img));
        let lines = renderer.render(5, 5);
        assert_eq!(lines.len(), 5);
        for line in &lines {
            assert!(!line.is_empty());
        }
    }

    #[test]
    fn test_halfblock_zoom() {
        let img = make_test_image(10, 10);
        let renderer = HalfblockRenderer::new(Arc::new(img)).zoom(200);
        let _grid = renderer.render_cells(5, 5);
    }

    #[test]
    fn test_viewport_scroll() {
        let mut vp = Viewport::new();
        vp.scroll_by(5, 3, 100, 100);
        assert_eq!(vp.scroll_x, 5);
        assert_eq!(vp.scroll_y, 3);
        vp.scroll_by(-2, -1, 100, 100);
        assert_eq!(vp.scroll_x, 3);
        assert_eq!(vp.scroll_y, 2);
    }

    #[test]
    fn test_viewport_zoom() {
        let mut vp = Viewport::new();
        vp.set_zoom(150, 100, 100);
        assert_eq!(vp.zoom_percent, 150);
        let (w, h) = vp.scaled_dims(100, 100);
        assert_eq!(w, 150);
        assert_eq!(h, 150);
    }

    #[test]
    fn test_diagram_hash_stable() {
        assert_eq!(diagram_hash("graph TD; A-->B"), diagram_hash("graph TD; A-->B"));
        assert_ne!(diagram_hash("graph TD; A-->B"), diagram_hash("graph TD; A-->C"));
    }

    #[test]
    fn test_kitty_renderer() {
        let img = make_test_image(4, 4);
        let renderer = KittyRenderer::new(Arc::new(img), 12345);
        let transmission = renderer.transmit();
        assert!(transmission.contains("_Gq=2"));
        assert!(transmission.contains("i="));
    }

    #[test]
    fn test_bucket_render_width() {
        let w = bucket_render_width(81, 8);
        assert_eq!(w, 648);
        let w2 = bucket_render_width(83, 8);
        assert_eq!(w2, 664);
    }
}
