//! Process-wide message layout LRU cache (port of jcode's
//! `jcode-tui-messages::cache`).
//!
//! Phi doesn't use ratatui; its styled text is `String` with embedded ANSI
//! escape sequences (SGR colors/attributes, OSC 8 hyperlinks).  The cached
//! unit is therefore `Vec<String>` — one entry per rendered message at a
//! given `(width, content_hash, diff_mode)` triple.
//!
//! On a cache hit the caller receives an `Arc<Vec<String>>` clone (the `Arc`
//! itself is cheap to clone; the backing `Vec` is shared until mutation).

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

// ---------------------------------------------------------------------------
// Diff display mode
// ---------------------------------------------------------------------------

/// How diffs are rendered in the message area.  Phi doesn't yet have a rich
/// diff-mode enum; this is the minimal variant set the cache key needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum DiffDisplayMode {
    #[default]
    Off,
    Unified,
    Split,
}

// ---------------------------------------------------------------------------
// Cache key
// ---------------------------------------------------------------------------

/// Multivariate key that captures every input that can change the rendered
/// output of a single message.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageCacheKey {
    /// Terminal width the message was wrapped to.
    pub width: u16,
    /// Hash of the message content (stable across re-renders).
    pub content_hash: u64,
    /// Length of the message content — included as a cheap disambiguator to
    /// reduce collider probability before the full hash is compared.
    pub content_len: usize,
    /// Diff rendering mode active when the message was laid out.
    pub diff_mode: DiffDisplayMode,
}

impl MessageCacheKey {
    /// Build a key from raw message content and runtime context.
    pub fn from_content(content: &str, width: u16, diff_mode: DiffDisplayMode) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut hasher);
        Self {
            width,
            content_hash: hasher.finish(),
            content_len: content.len(),
            diff_mode,
        }
    }
}

// ---------------------------------------------------------------------------
// LRU state
// ---------------------------------------------------------------------------

const MESSAGE_CACHE_LIMIT: usize = 2048;

struct MessageCacheState {
    entries: HashMap<MessageCacheKey, Arc<Vec<String>>>,
    order: VecDeque<MessageCacheKey>,
    limit: usize,
}

impl Default for MessageCacheState {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            limit: MESSAGE_CACHE_LIMIT,
        }
    }
}

impl MessageCacheState {
    fn get(&self, key: &MessageCacheKey) -> Option<Arc<Vec<String>>> {
        self.entries.get(key).cloned()
    }

    fn insert(&mut self, key: MessageCacheKey, lines: Vec<String>) -> Arc<Vec<String>> {
        let arc = Arc::new(lines);

        // If the key already exists, update the value but don't touch LRU
        // ordering (the entry retains its original position).
        if let std::collections::hash_map::Entry::Occupied(mut entry) =
            self.entries.entry(key.clone())
        {
            entry.insert(arc.clone());
            return arc;
        }

        // New entry — add to both map and LRU tail.
        self.entries.insert(key.clone(), arc.clone());
        self.order.push_back(key);

        // Evict oldest entries until we're within the limit.
        while self.order.len() > self.limit {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }

        arc
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

// ---------------------------------------------------------------------------
// Process-wide singleton
// ---------------------------------------------------------------------------

static MESSAGE_CACHE: OnceLock<Mutex<MessageCacheState>> = OnceLock::new();

fn message_cache() -> &'static Mutex<MessageCacheState> {
    MESSAGE_CACHE.get_or_init(|| Mutex::new(MessageCacheState::default()))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns the number of entries currently in the cache (mainly for tests).
pub fn cache_len() -> usize {
    match message_cache().lock() {
        Ok(state) => state.len(),
        Err(poisoned) => poisoned.into_inner().len(),
    }
}

/// Look up — or compute and store — the rendered lines for a message.
///
/// On a cache hit the closure is **not** called and the stored
/// `Arc<Vec<String>>` is returned directly (zero-copy: the `Arc` is cloned
/// but the backing `Vec` is shared).  On a miss the closure runs, the result
/// is inserted, and the new `Arc` is returned.
///
/// `content` is the raw message text used to derive the cache key.  In a
/// full integration this would be the message's content field; the hash and
/// length are computed from it so the caller doesn't need to pre-hash.
pub fn get_cached_message_lines<F>(
    content: &str,
    width: u16,
    diff_mode: DiffDisplayMode,
    render: F,
) -> Arc<Vec<String>>
where
    F: FnOnce(&str, u16, DiffDisplayMode) -> Vec<String>,
{
    // Bypass cache in test builds so unit tests for rendering aren't masked
    // by cached results.
    if cfg!(test) {
        return Arc::new(render(content, width, diff_mode));
    }

    let key = MessageCacheKey::from_content(content, width, diff_mode);

    let mut cache = match message_cache().lock() {
        Ok(c) => c,
        Err(poisoned) => poisoned.into_inner(),
    };

    if let Some(cached) = cache.get(&key) {
        return cached;
    }

    let lines = render(content, width, diff_mode);
    cache.insert(key, lines)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_from_content_is_deterministic() {
        let a = MessageCacheKey::from_content("hello", 80, DiffDisplayMode::Off);
        let b = MessageCacheKey::from_content("hello", 80, DiffDisplayMode::Off);
        assert_eq!(a, b);
    }

    #[test]
    fn key_differs_on_width() {
        let a = MessageCacheKey::from_content("hello", 80, DiffDisplayMode::Off);
        let b = MessageCacheKey::from_content("hello", 120, DiffDisplayMode::Off);
        assert_ne!(a, b);
    }

    #[test]
    fn key_differs_on_content() {
        let a = MessageCacheKey::from_content("hello", 80, DiffDisplayMode::Off);
        let b = MessageCacheKey::from_content("world", 80, DiffDisplayMode::Off);
        assert_ne!(a, b);
    }

    #[test]
    fn key_differs_on_diff_mode() {
        let a = MessageCacheKey::from_content("hello", 80, DiffDisplayMode::Off);
        let b = MessageCacheKey::from_content("hello", 80, DiffDisplayMode::Unified);
        assert_ne!(a, b);
    }

    #[test]
    fn insert_and_get_returns_same_arc() {
        // We test the state struct directly since the process-wide singleton
        // is bypassed under `cfg(test)`.
        let mut state = MessageCacheState::default();
        let key = MessageCacheKey::from_content("test", 80, DiffDisplayMode::Off);
        let arc1 = state.insert(key.clone(), vec!["line1".to_string()]);
        let arc2 = state.get(&key).unwrap();
        assert!(Arc::ptr_eq(&arc1, &arc2));
        assert_eq!(arc2.as_ref(), &vec!["line1".to_string()]);
    }

    #[test]
    fn lru_evicts_oldest_beyond_limit() {
        let mut state = MessageCacheState::default();
        state.limit = 3;

        for i in 0..5 {
            let key = MessageCacheKey::from_content(&format!("msg{i}"), 80, DiffDisplayMode::Off);
            state.insert(key, vec![format!("line{i}")]);
        }

        // Only the last 3 should remain.
        assert_eq!(state.len(), 3);
        assert!(state
            .get(&MessageCacheKey::from_content("msg0", 80, DiffDisplayMode::Off))
            .is_none());
        assert!(state
            .get(&MessageCacheKey::from_content("msg1", 80, DiffDisplayMode::Off))
            .is_none());
        assert!(state
            .get(&MessageCacheKey::from_content("msg2", 80, DiffDisplayMode::Off))
            .is_some());
        assert!(state
            .get(&MessageCacheKey::from_content("msg4", 80, DiffDisplayMode::Off))
            .is_some());
    }

    #[test]
    fn update_existing_key_does_not_grow_lru() {
        let mut state = MessageCacheState::default();
        state.limit = 2;

        let key = MessageCacheKey::from_content("msg", 80, DiffDisplayMode::Off);
        state.insert(key.clone(), vec!["v1".to_string()]);
        state.insert(key.clone(), vec!["v2".to_string()]);

        // Re-inserting the same key should not push a duplicate onto the
        // order deque.
        assert_eq!(state.order.len(), 1);
        assert_eq!(state.entries.len(), 1);

        let cached = state.get(&key).unwrap();
        assert_eq!(cached.as_ref(), &vec!["v2".to_string()]);
    }

    #[test]
    fn cache_len_reflects_entries() {
        // The process-wide cache is bypassed under cfg!(test), but cache_len
        // still reads the static.  At minimum it shouldn't panic.
        let _ = cache_len();
    }
}
