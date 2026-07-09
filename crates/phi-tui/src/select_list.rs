//! Select-list component mechanism matching pi's keyboard, filtering, and viewport rules.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SelectList {
    items: Vec<SelectItem>,
    filtered: Vec<usize>,
    selected: usize,
    max_visible: usize,
}

impl SelectList {
    pub fn new(items: Vec<SelectItem>, max_visible: usize) -> Self {
        let filtered = (0..items.len()).collect();
        Self {
            items,
            filtered,
            selected: 0,
            max_visible: max_visible.max(1),
        }
    }
    pub fn set_filter(&mut self, filter: &str) {
        let query = filter.to_lowercase();
        self.filtered = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.value.to_lowercase().starts_with(&query))
            .map(|(i, _)| i)
            .collect();
        self.selected = 0;
    }
    pub fn selected(&self) -> Option<&SelectItem> {
        self.filtered
            .get(self.selected)
            .and_then(|i| self.items.get(*i))
    }
    pub fn selected_index(&self) -> usize {
        self.selected
    }
    pub fn handle(&mut self, input: &str) -> &'static str {
        if self.filtered.is_empty() {
            return "none";
        }
        match crate::editor::decode_key(input).as_deref() {
            Some("up") => {
                self.selected = if self.selected == 0 {
                    self.filtered.len() - 1
                } else {
                    self.selected - 1
                };
                "changed"
            }
            Some("down") => {
                self.selected = (self.selected + 1) % self.filtered.len();
                "changed"
            }
            Some("enter") => "confirm",
            Some("escape") | Some("ctrl+c") => "cancel",
            _ => "none",
        }
    }
    pub fn render(&self, width: usize) -> Vec<String> {
        if self.filtered.is_empty() {
            return vec!["  No matching commands".to_owned()];
        }
        let start = self
            .selected
            .saturating_sub(self.max_visible / 2)
            .min(self.filtered.len().saturating_sub(self.max_visible));
        let end = (start + self.max_visible).min(self.filtered.len());
        let mut lines = Vec::with_capacity(end - start + 1);
        for pos in start..end {
            if let Some(item) = self.filtered.get(pos).and_then(|i| self.items.get(*i)) {
                let prefix = if pos == self.selected { "→ " } else { "  " };
                let mut line = format!("{prefix}{}", item.label);
                if width > 40
                    && let Some(description) = &item.description
                {
                    let description = description.replace(['\r', '\n'], " ");
                    line.push_str("  ");
                    line.push_str(&description);
                }
                line = line.chars().take(width.max(1)).collect();
                lines.push(line);
            }
        }
        if start > 0 || end < self.filtered.len() {
            lines.push(format!("  ({}/{})", self.selected + 1, self.filtered.len()));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn list() -> SelectList {
        SelectList::new(
            vec![
                SelectItem {
                    value: "alpha".into(),
                    label: "alpha".into(),
                    description: Some("first".into()),
                },
                SelectItem {
                    value: "beta".into(),
                    label: "beta".into(),
                    description: None,
                },
            ],
            1,
        )
    }
    #[test]
    fn filter_resets_and_wraps() {
        let mut list = list();
        list.set_filter("b");
        assert_eq!(list.selected().map(|x| x.value.as_str()), Some("beta"));
        assert_eq!(list.handle("\x1b[A"), "changed");
        assert_eq!(list.selected_index(), 0);
    }
    #[test]
    fn render_marks_selection_and_scroll() {
        let lines = list().render(20);
        assert_eq!(lines, vec!["→ alpha", "  (1/2)"]);
    }
}
