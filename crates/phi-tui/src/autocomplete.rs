//! Autocomplete mechanism matching `packages/tui/src/autocomplete.ts`.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutocompleteItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: String,
    pub description: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestions {
    pub items: Vec<AutocompleteItem>,
    pub prefix: String,
}

fn delimiter(c: char) -> bool {
    matches!(c, ' ' | '\t' | '"' | '\'' | '=')
}
fn prefix(text: &str, force: bool) -> Option<String> {
    if let Some(q) = text.rfind('"')
        && text[q + 1..].find('"').is_none()
    {
        return Some(text[q..].to_owned());
    }
    let start = text
        .char_indices()
        .rev()
        .find(|(_, c)| delimiter(*c))
        .map_or(0, |(i, c)| i + c.len_utf8());
    let p = &text[start..];
    if force
        || p.contains('/')
        || p.starts_with('.')
        || p.starts_with("~/")
        || (p.is_empty() && text.ends_with(' '))
    {
        Some(p.to_owned())
    } else {
        None
    }
}
fn parse(p: &str) -> (&str, bool, bool) {
    if let Some(x) = p.strip_prefix("@\"") {
        (x, true, true)
    } else if let Some(x) = p.strip_prefix('"') {
        (x, false, true)
    } else if let Some(x) = p.strip_prefix('@') {
        (x, true, false)
    } else {
        (p, false, false)
    }
}
fn display_path(p: &str, at: bool, quoted: bool, dir: bool) -> String {
    let mut s = p.replace('\\', "/");
    if dir && !s.ends_with('/') {
        s.push('/');
    }
    if at {
        s.insert(0, '@');
    }
    if quoted || s.contains(' ') {
        if at {
            format!("@\"{}\"", s.trim_start_matches('@'))
        } else {
            format!("\"{s}\"")
        }
    } else {
        s
    }
}

pub fn suggestions(
    commands: &[SlashCommand],
    base: &Path,
    line: &str,
    force: bool,
) -> Option<Suggestions> {
    let before = line;
    if before.starts_with('/') && !before.contains(' ') {
        let query = &before[1..];
        let items = commands
            .iter()
            .filter(|c| c.name.to_lowercase().contains(&query.to_lowercase()))
            .map(|c| AutocompleteItem {
                value: c.name.clone(),
                label: c.name.clone(),
                description: c.description.clone(),
            })
            .collect::<Vec<_>>();
        return (!items.is_empty()).then_some(Suggestions {
            items,
            prefix: before.to_owned(),
        });
    }
    let p = prefix(before, force)?;
    let (raw, at, quoted) = parse(&p);
    let expanded = if let Some(x) = raw.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(x)
    } else {
        PathBuf::from(raw)
    };
    let full = if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    };
    let (dir, query) = if raw.ends_with('/') || raw.is_empty() {
        (full, "".to_owned())
    } else {
        (
            full.parent().unwrap_or(base).to_path_buf(),
            full.file_name()
                .and_then(|x| x.to_str())
                .unwrap_or("")
                .to_owned(),
        )
    };
    let mut items = Vec::new();
    for entry in fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.to_lowercase().starts_with(&query.to_lowercase()) {
            continue;
        }
        let is_dir = entry.file_type().ok()?.is_dir();
        let value = if raw.ends_with('/') {
            format!("{}{}", raw, name)
        } else if let Some((parent, _)) = raw.rsplit_once('/') {
            format!("{parent}/{name}")
        } else {
            name.clone()
        };
        items.push(AutocompleteItem {
            value: display_path(&value, at, quoted, is_dir),
            label: format!("{}{}", name, if is_dir { "/" } else { "" }),
            description: None,
        });
    }
    items.sort_by_key(|i| (!i.label.ends_with('/'), i.label.to_lowercase()));
    (!items.is_empty()).then_some(Suggestions { items, prefix: p })
}

pub fn apply(line: &str, cursor: usize, item: &AutocompleteItem, p: &str) -> (String, usize) {
    let before = &line[..cursor.saturating_sub(p.len())];
    let after = &line[cursor..];
    if p.starts_with('/') && !p[1..].contains('/') {
        let s = format!("{before}/{} {after}", item.value);
        let c = before.len() + item.value.len() + 2;
        return (s, c);
    }
    let s = format!("{before}{}{after}", item.value);
    (s, before.len() + item.value.len())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    #[test]
    fn command_completion_and_apply() {
        let cs = vec![SlashCommand {
            name: "model".into(),
            description: None,
        }];
        let x = suggestions(&cs, Path::new("."), "/mo", false).unwrap();
        assert_eq!(x.items[0].value, "model");
        assert_eq!(
            apply("/mo", 3, &x.items[0], &x.prefix),
            ("/model ".into(), 7)
        );
    }
    #[test]
    fn directory_completion() {
        let d = std::env::temp_dir().join(format!("phi-autocomplete-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        let x = suggestions(&[], &d, "./s", true).unwrap();
        let _ = fs::remove_dir_all(&d);
        assert_eq!(x.items[0].label, "src/");
    }
}
