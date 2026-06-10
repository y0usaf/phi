//! Generic full-width selector (port of pi's selector components): an
//! optional fuzzy-search input above an arrow-navigable list. The host swaps
//! it in place of the editor until the user confirms or cancels.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthChar;

use crate::fuzzy::fuzzy_filter;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";

const MAX_VISIBLE: usize = 10;

/// One selectable row. `label` and `detail` are plain text; the selector
/// applies its own styling.
pub struct SelectorItem {
    /// Returned to the host on confirm.
    pub value: String,
    pub label: String,
    /// Dim annotation rendered after the label.
    pub detail: String,
    /// Marks the active value with a `✓` (e.g. the current model).
    pub current: bool,
}

impl SelectorItem {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            detail: String::new(),
            current: false,
        }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    pub fn current(mut self, current: bool) -> Self {
        self.current = current;
        self
    }
}

#[derive(Eq, PartialEq, Debug)]
pub enum SelectorOutcome {
    /// Index into `items()` of the confirmed row.
    Confirmed(usize),
    Cancelled,
    /// Key consumed (selection moved / query edited).
    Pending,
}

pub struct Selector {
    title: String,
    items: Vec<SelectorItem>,
    query: String,
    searchable: bool,
    /// Indices into `items`, filtered by `query`, best match first.
    filtered: Vec<usize>,
    /// Index into `filtered`.
    selected: usize,
}

impl Selector {
    pub fn new(title: impl Into<String>, items: Vec<SelectorItem>, searchable: bool) -> Self {
        let filtered = (0..items.len()).collect();
        Self {
            title: title.into(),
            items,
            query: String::new(),
            searchable,
            filtered,
            selected: 0,
        }
    }

    pub fn items(&self) -> &[SelectorItem] {
        &self.items
    }

    pub fn searchable(&self) -> bool {
        self.searchable
    }

    /// Move the selection to the item with `value`, if present.
    pub fn preselect(&mut self, value: &str) {
        if let Some(pos) = self
            .filtered
            .iter()
            .position(|&i| self.items[i].value == value)
        {
            self.selected = pos;
        }
    }

    fn refilter(&mut self) {
        self.filtered = fuzzy_filter((0..self.items.len()).collect(), &self.query, |&i| {
            let item = &self.items[i];
            format!("{} {} {}", item.label, item.detail, item.value)
        });
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> SelectorOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return SelectorOutcome::Cancelled,
            KeyCode::Char('c') if ctrl => return SelectorOutcome::Cancelled,
            KeyCode::Up | KeyCode::BackTab if !self.filtered.is_empty() => {
                self.selected = self
                    .selected
                    .checked_sub(1)
                    .unwrap_or(self.filtered.len() - 1);
            }
            KeyCode::Down if !self.filtered.is_empty() => {
                self.selected = (self.selected + 1) % self.filtered.len();
            }
            KeyCode::Enter => {
                if let Some(&index) = self.filtered.get(self.selected) {
                    return SelectorOutcome::Confirmed(index);
                }
            }
            KeyCode::Backspace if self.searchable => {
                self.query.pop();
                self.refilter();
            }
            KeyCode::Char('u') if ctrl && self.searchable => {
                self.query.clear();
                self.refilter();
            }
            KeyCode::Char(c)
                if self.searchable && !ctrl && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.query.push(c);
                self.refilter();
            }
            _ => {}
        }
        SelectorOutcome::Pending
    }

    /// Lines for the live region plus, for searchable selectors, the cursor
    /// position as (line offset, column) within those lines.
    pub fn render(&self, width: usize) -> (Vec<String>, Option<(usize, usize)>) {
        let mut lines = vec![format!("{BOLD}{}{RESET}", self.title)];
        let cursor = self.searchable.then(|| {
            lines.push(format!("{BOLD}›{RESET} {}", self.query));
            (
                1,
                2 + self
                    .query
                    .chars()
                    .map(|c| c.width().unwrap_or(0))
                    .sum::<usize>(),
            )
        });

        let total = self.filtered.len();
        if total == 0 {
            lines.push(format!("{DIM}  no matches{RESET}"));
            return (lines, cursor);
        }

        // Window centered on the selection, pi-style.
        let start = self
            .selected
            .saturating_sub(MAX_VISIBLE / 2)
            .min(total.saturating_sub(MAX_VISIBLE));
        let end = (start + MAX_VISIBLE).min(total);
        for (pos, &index) in self.filtered[start..end].iter().enumerate() {
            let item = &self.items[index];
            let selected = start + pos == self.selected;
            let mark = if item.current { " ✓" } else { "" };
            let label = truncate(&item.label, width.saturating_sub(4));
            let mut line = if selected {
                format!("{CYAN}→ {label}{RESET}")
            } else {
                format!("  {label}")
            };
            if !item.detail.is_empty() {
                line.push_str(&format!(" {DIM}{}{RESET}", item.detail));
            }
            if !mark.is_empty() {
                line.push_str(&format!("{GREEN}{mark}{RESET}"));
            }
            lines.push(line);
        }
        if total > MAX_VISIBLE {
            lines.push(format!("{DIM}  ({}/{total}){RESET}", self.selected + 1));
        }
        (lines, cursor)
    }
}

/// Truncate plain text to `width` display columns, appending `…` on cut.
pub fn truncate(text: &str, width: usize) -> String {
    let total: usize = text.chars().map(|c| c.width().unwrap_or(0)).sum();
    if total <= width {
        return text.to_owned();
    }
    let mut used = 0;
    let mut out = String::new();
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > width.saturating_sub(1) {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn selector() -> Selector {
        Selector::new(
            "Pick",
            vec![
                SelectorItem::new("a/one", "one"),
                SelectorItem::new("b/two", "two").current(true),
                SelectorItem::new("c/three", "three"),
            ],
            true,
        )
    }

    #[test]
    fn arrows_wrap_and_enter_confirms() {
        let mut s = selector();
        assert_eq!(s.handle_key(key(KeyCode::Up)), SelectorOutcome::Pending);
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            SelectorOutcome::Confirmed(2)
        );
        assert_eq!(s.handle_key(key(KeyCode::Down)), SelectorOutcome::Pending);
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            SelectorOutcome::Confirmed(0)
        );
    }

    #[test]
    fn typing_filters_and_esc_cancels() {
        let mut s = selector();
        s.handle_key(key(KeyCode::Char('t')));
        s.handle_key(key(KeyCode::Char('w')));
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            SelectorOutcome::Confirmed(1)
        );
        assert_eq!(s.handle_key(key(KeyCode::Esc)), SelectorOutcome::Cancelled);
    }

    #[test]
    fn preselect_moves_to_value() {
        let mut s = selector();
        s.preselect("c/three");
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            SelectorOutcome::Confirmed(2)
        );
    }

    #[test]
    fn no_match_renders_placeholder() {
        let mut s = selector();
        s.handle_key(key(KeyCode::Char('z')));
        s.handle_key(key(KeyCode::Char('z')));
        let (lines, cursor) = s.render(80);
        assert!(lines.iter().any(|l| l.contains("no matches")));
        assert_eq!(cursor, Some((1, 4)));
        assert_eq!(s.handle_key(key(KeyCode::Enter)), SelectorOutcome::Pending);
    }
}
