//! Lazy-decoding image cache with prefetch band and stable fit cache.

use std::collections::HashMap;
use std::time::Instant;

use image::DynamicImage;

use crate::{ImageDisplayParams, ImageFit, ImageId, ImageSource};

/// Configuration for the prefetch band and cache eviction.
#[derive(Debug, Clone)]
pub struct PrefetchConfig {
    /// Number of rows above/below the visible viewport to prefetch.
    pub band_rows: usize,
    /// Maximum number of simultaneously-decoded images.
    pub max_decoded: usize,
}

impl Default for PrefetchConfig {
    fn default() -> Self {
        Self {
            band_rows: 50,
            max_decoded: 32,
        }
    }
}

/// A decoded image entry in the cache.
#[derive(Debug)]
pub struct ImageEntry {
    /// Unique identifier.
    pub id: ImageId,
    /// Source path or bytes.
    pub source: ImageSource,
    /// Decoded image (lazily populated).
    pub decoded: Option<DynamicImage>,
    /// Cached fit result.
    pub fit: Option<ImageFit>,
    /// Anchor row in scrollback where the image starts.
    pub anchor_row: usize,
    /// Last access time (for LRU eviction).
    pub last_accessed: Instant,
}

/// Stable fit cache: maps (content hash, viewport constraints) to display fit.
#[derive(Debug, Default)]
pub struct FitCache {
    entries: HashMap<(u64, u16, u16), ImageFit>,
}

impl FitCache {
    /// Look up a cached fit.
    pub fn get(&self, id: ImageId, max_cols: u16, max_rows: u16) -> Option<ImageFit> {
        self.entries.get(&(id, max_cols, max_rows)).copied()
    }

    /// Insert a fit into the cache.
    pub fn put(&mut self, id: ImageId, max_cols: u16, max_rows: u16, fit: ImageFit) {
        self.entries.insert((id, max_cols, max_rows), fit);
    }

    /// Number of cached fits.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the fit cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clear all cached fits.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Lazy-decoding image cache with prefetch band.
pub struct ImageCache {
    entries: HashMap<ImageId, ImageEntry>,
    fit_cache: FitCache,
    config: PrefetchConfig,
}

impl ImageCache {
    /// Create a new image cache with default prefetch config.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            fit_cache: FitCache::default(),
            config: PrefetchConfig::default(),
        }
    }

    /// Create a new image cache with custom prefetch config.
    pub fn with_config(config: PrefetchConfig) -> Self {
        Self {
            entries: HashMap::new(),
            fit_cache: FitCache::default(),
            config,
        }
    }

    /// Register an image source at a given anchor row.
    pub fn register(&mut self, id: ImageId, source: ImageSource, anchor_row: usize) {
        self.entries.insert(
            id,
            ImageEntry {
                id,
                source,
                decoded: None,
                fit: None,
                anchor_row,
                last_accessed: Instant::now(),
            },
        );
    }

    /// Unregister an image.
    pub fn unregister(&mut self, id: ImageId) {
        self.entries.remove(&id);
    }

    /// Look up an image entry by ID.
    pub fn get(&self, id: ImageId) -> Option<&ImageEntry> {
        self.entries.get(&id)
    }

    /// Look up an image entry by ID (mutable).
    pub fn get_mut(&mut self, id: ImageId) -> Option<&mut ImageEntry> {
        self.entries.get_mut(&id)
    }

    /// Ensure images within the viewport (plus prefetch band) are decoded.
    pub fn ensure_visible(&mut self, viewport_top: usize, viewport_bottom: usize) {
        let band = self.config.band_rows;
        let lo = viewport_top.saturating_sub(band);
        let hi = viewport_bottom.saturating_add(band);

        for entry in self.entries.values_mut() {
            let in_band = entry.anchor_row >= lo && entry.anchor_row <= hi;
            if in_band {
                entry.last_accessed = Instant::now();
                if entry.decoded.is_none() {
                    if let Ok(data) = entry.source.read_bytes() {
                        if let Ok(img) = crate::display::decode_image(&data) {
                            entry.decoded = Some(img);
                        }
                    }
                }
            } else if entry.decoded.is_some() {
                entry.decoded = None;
            }
        }

        let max = self.config.max_decoded;
        let decoded_count = self.entries.values().filter(|e| e.decoded.is_some()).count();
        if decoded_count > max {
            let excess = decoded_count - max;
            let mut decoded_ids: Vec<(ImageId, Instant)> = self
                .entries
                .iter()
                .filter(|(_, e)| e.decoded.is_some())
                .map(|(id, e)| (*id, e.last_accessed))
                .collect();
            decoded_ids.sort_by_key(|(_, t)| *t);
            for (id, _) in decoded_ids.into_iter().take(excess) {
                if let Some(e) = self.entries.get_mut(&id) {
                    e.decoded = None;
                }
            }
        }
    }

    /// Get or compute the display fit for an image, using the stable fit cache.
    pub fn fit(&mut self, id: ImageId, params: &ImageDisplayParams) -> Option<ImageFit> {
        if let Some(cached) = self.fit_cache.get(id, params.max_cols, params.max_rows) {
            return Some(cached);
        }

        let entry = self.entries.get(&id)?;
        let img = entry.decoded.as_ref()?;

        let (iw, ih) = (img.width(), img.height());
        let fit = crate::display::calculate_display_size(iw, ih, params.max_cols, params.max_rows);

        self.fit_cache.put(id, params.max_cols, params.max_rows, fit);
        Some(fit)
    }

    /// Access the stable fit cache.
    pub fn fit_cache(&self) -> &FitCache {
        &self.fit_cache
    }

    /// Access the stable fit cache (mutable).
    pub fn fit_cache_mut(&mut self) -> &mut FitCache {
        &mut self.fit_cache
    }

    /// Number of registered images.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache has no registered images.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterator over all registered image entries.
    pub fn entries(&self) -> impl Iterator<Item = (&ImageId, &ImageEntry)> {
        self.entries.iter()
    }

    /// Clear all entries and cached fits.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.fit_cache.clear();
    }
}

impl Default for ImageCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_cache_get_put() {
        let mut cache = FitCache::default();
        assert!(cache.is_empty());
        cache.put(1, 80, 24, ImageFit { cols: 40, rows: 12 });
        assert_eq!(cache.get(1, 80, 24), Some(ImageFit { cols: 40, rows: 12 }));
        assert_eq!(cache.get(1, 100, 24), None);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn image_cache_register_unregister() {
        let mut cache = ImageCache::new();
        cache.register(
            1,
            ImageSource::Bytes { data: vec![], format_hint: None },
            10,
        );
        assert_eq!(cache.len(), 1);
        assert!(cache.get(1).is_some());
        cache.unregister(1);
        assert!(cache.is_empty());
    }
}
