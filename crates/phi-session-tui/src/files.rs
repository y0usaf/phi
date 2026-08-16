//! File-path completion (pi's autocomplete.ts behaviors): `@token` fuzzy
//! search over the worktree (gitignore-aware, like pi's fd subprocess) and
//! readdir prefix completion for Tab.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const CACHE_TTL: Duration = Duration::from_secs(3);
const WALK_CAP: usize = 20_000;
const MAX_RESULTS: usize = 20;

pub struct FileCompletion {
    /// Text inserted into the editor (e.g. `@src/main.rs` or `@"a b/c"`).
    pub value: String,
    /// Short display label (basename, `/`-suffixed for directories).
    pub label: String,
    /// Full relative path shown as the description.
    pub desc: String,
}

/// Relative `/`-separated path plus an is-directory flag.
type Entry = (String, bool);

/// Worktree walker with a short-lived cache so per-keystroke queries don't
/// rescan the tree (pi debounces fd instead).
pub struct FileSearcher {
    cwd: PathBuf,
    cache: Option<(PathBuf, Instant, Vec<Entry>)>,
}

impl FileSearcher {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd, cache: None }
    }

    /// Fuzzy file search for an `@` token (without the `@`). A `dir/` prefix
    /// in the query scopes the walk to that directory, pi-style.
    pub fn search(&mut self, raw_query: &str) -> Vec<FileCompletion> {
        let (display_base, query) = match raw_query.rfind('/') {
            Some(i) => (&raw_query[..=i], &raw_query[i + 1..]),
            None => ("", raw_query),
        };
        let base_dir = if display_base.is_empty() {
            self.cwd.clone()
        } else {
            let expanded = expand_home(display_base);
            if Path::new(&expanded).is_absolute() {
                PathBuf::from(expanded)
            } else {
                self.cwd.join(expanded)
            }
        };
        if !base_dir.is_dir() {
            return Vec::new();
        }

        let entries = self.entries(&base_dir);
        let mut scored: Vec<(&(String, bool), i32)> = entries
            .iter()
            .filter_map(|entry| {
                let score = if query.is_empty() {
                    1
                } else {
                    score_entry(&entry.0, query, entry.1)
                };
                (score > 0).then_some((entry, score))
            })
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.0.cmp(&b.0.0)));
        scored
            .into_iter()
            .take(MAX_RESULTS)
            .map(|((path, is_dir), _)| {
                let display = format!("{display_base}{path}");
                let name = path.rsplit('/').next().unwrap_or(path);
                FileCompletion {
                    value: completion_value(&display, *is_dir),
                    label: format!("{name}{}", if *is_dir { "/" } else { "" }),
                    desc: display,
                }
            })
            .collect()
    }

    fn entries(&mut self, base: &Path) -> &[(String, bool)] {
        let stale = match &self.cache {
            Some((dir, at, _)) => dir != base || at.elapsed() > CACHE_TTL,
            None => true,
        };
        if stale {
            self.cache = Some((base.to_owned(), Instant::now(), walk(base)));
        }
        &self.cache.as_ref().unwrap().2
    }
}

/// Walk `base` respecting .gitignore, including hidden files but never
/// descending into `.git` (pi's fd flags). Paths are `/`-separated and
/// relative to `base`.
fn walk(base: &Path) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(base)
        .hidden(false)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build();
    for entry in walker.flatten() {
        if out.len() >= WALK_CAP {
            break;
        }
        let Ok(rel) = entry.path().strip_prefix(base) else {
            continue;
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
        let path = rel.to_string_lossy().replace('\\', "/");
        out.push((path, is_dir));
    }
    out
}

/// pi's scoreEntry: exact basename 100, basename prefix 80, basename
/// substring 50, path substring 30; directories get +10.
fn score_entry(path: &str, query: &str, is_dir: bool) -> i32 {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    let query = query.to_lowercase();
    let mut score = if name == query {
        100
    } else if name.starts_with(&query) {
        80
    } else if name.contains(&query) {
        50
    } else if path.to_lowercase().contains(&query) {
        30
    } else {
        0
    };
    if is_dir && score > 0 {
        score += 10;
    }
    score
}

fn completion_value(path: &str, is_dir: bool) -> String {
    let full = format!("{path}{}", if is_dir { "/" } else { "" });
    // Quote (and escape) whenever the path carries a space, a double-quote,
    // a backslash, or any control character — not just spaces.
    let needs_quote = full
        .chars()
        .any(|c| c == ' ' || c == '"' || c == '\\' || c.is_control());
    if needs_quote {
        let escaped = full.replace('\\', "\\\\").replace('\"', "\\\"");
        format!("@\"{escaped}\"")
    } else {
        format!("@{full}")
    }
}

fn expand_home(path: &str) -> String {
    if (path == "~" || path.starts_with("~/"))
        && let Some(home) = std::env::var_os("HOME")
    {
        return format!("{}{}", home.to_string_lossy(), &path[1..]);
    }
    path.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        std::fs::create_dir_all(base.join("src/nested")).unwrap();
        std::fs::create_dir_all(base.join("docs")).unwrap();
        std::fs::write(base.join("src/main.rs"), "").unwrap();
        std::fs::write(base.join("src/nested/util.rs"), "").unwrap();
        std::fs::write(base.join("docs/guide.md"), "").unwrap();
        std::fs::write(base.join("README.md"), "").unwrap();
        std::fs::write(base.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(base.join("ignored.txt"), "").unwrap();
        dir
    }

    #[test]
    fn fuzzy_search_finds_files_and_respects_gitignore() {
        let dir = fixture();
        let mut searcher = FileSearcher::new(dir.path().to_owned());
        let hits = searcher.search("main");
        assert!(
            hits.iter().any(|h| h.value == "@src/main.rs"),
            "missing main.rs"
        );
        let all = searcher.search("");
        assert!(!all.iter().any(|h| h.desc.contains("ignored.txt")));
    }

    #[test]
    fn scoped_query_searches_subdirectory() {
        let dir = fixture();
        let mut searcher = FileSearcher::new(dir.path().to_owned());
        let hits = searcher.search("src/util");
        assert!(
            hits.iter().any(|h| h.value == "@src/nested/util.rs"),
            "scoped search failed"
        );
    }

    #[test]
    fn directories_score_above_files() {
        assert!(score_entry("src", "src", true) > score_entry("src.rs", "src", false));
        assert_eq!(score_entry("zzz", "src", false), 0);
    }

    #[test]
    fn values_with_spaces_are_quoted() {
        assert_eq!(completion_value("a b/c.txt", false), "@\"a b/c.txt\"");
        assert_eq!(completion_value("src/main.rs", false), "@src/main.rs");
    }

    #[test]
    fn values_with_quotes_or_control_chars_are_quoted_and_escaped() {
        // A double-quote inside a path must be escaped inside the @"...".
        assert_eq!(completion_value("a\"b/c.txt", false), "@\"a\\\"b/c.txt\"");
        // A control character (newline) also forces quoting.
        assert_eq!(completion_value("a\nb", false), "@\"a\nb\"");
        // A plain single-word path stays unquoted.
        assert_eq!(completion_value("src/main.rs", false), "@src/main.rs");
    }
}
