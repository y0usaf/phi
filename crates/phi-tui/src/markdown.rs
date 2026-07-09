//! Deterministic markdown-to-cell rendering mechanism (pi `components/markdown.ts`).

use crate::component::Component;

/// Render the useful, terminal-safe markdown subset used by the interactive frontend.
pub fn render_markdown(
    text: &str,
    width: usize,
    padding_x: usize,
    padding_y: usize,
) -> Vec<String> {
    Markdown::new(text, padding_x, padding_y).render(width)
}

pub struct Markdown {
    text: String,
    padding_x: usize,
    padding_y: usize,
}
impl Markdown {
    pub fn new(text: impl Into<String>, padding_x: usize, padding_y: usize) -> Self {
        Self {
            text: text.into(),
            padding_x,
            padding_y,
        }
    }
}

fn inline(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '`' {
            let mut value = String::new();
            while let Some(&next) = chars.peek() {
                chars.next();
                if next == '`' {
                    break;
                }
                value.push(next);
            }
            out.push_str(&value);
        } else if c == '[' {
            let mut label = String::new();
            while let Some(&next) = chars.peek() {
                chars.next();
                if next == ']' {
                    break;
                }
                label.push(next);
            }
            if chars.next() == Some('(') {
                for next in chars.by_ref() {
                    if next == ')' {
                        break;
                    }
                }
                out.push_str(&label);
            } else {
                out.push('[');
                out.push_str(&label);
            }
        } else if (c == '*' || c == '_' || c == '~') && chars.peek() == Some(&c) {
            chars.next();
            while let Some(next) = chars.next() {
                if next == c && chars.peek() == Some(&c) {
                    chars.next();
                    break;
                }
                out.push(next);
            }
        } else if c == '*' || c == '_' {
            for next in chars.by_ref() {
                if next == c {
                    break;
                }
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}
fn wrap(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    let words: Vec<&str> = line.split_whitespace().collect();
    if words.is_empty() {
        return vec![String::new()];
    }
    let mut result = Vec::new();
    let mut current = String::new();
    for word in words {
        if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > width {
            result.push(current);
            current = String::new();
        }
        if !current.is_empty() {
            current.push(' ')
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}
impl Component for Markdown {
    fn render(&self, width: usize) -> Vec<String> {
        let width = width.max(1);
        let content = width.saturating_sub(self.padding_x * 2).max(1);
        if self.text.trim().is_empty() {
            return Vec::new();
        }
        let mut lines = Vec::new();
        let mut fenced = false;
        for raw in self.text.replace('\t', "   ").lines() {
            let trimmed = raw.trim();
            if trimmed.starts_with("```") {
                fenced = !fenced;
                continue;
            }
            let mut line = if fenced {
                raw.to_owned()
            } else {
                inline(trimmed)
            };
            if !fenced && line.starts_with("### ") {
                line = line[4..].to_owned()
            } else if !fenced && line.starts_with("## ") {
                line = line[3..].to_owned()
            } else if !fenced && line.starts_with("# ") {
                line = line[2..].to_owned()
            } else if !fenced && line.starts_with("> ") {
                line = format!("│ {}", &line[2..])
            } else if !fenced && (line.starts_with("- ") || line.starts_with("* ")) {
                line = format!("• {}", &line[2..])
            }
            lines.extend(wrap(&line, content));
        }
        let blank = " ".repeat(width);
        let left = " ".repeat(self.padding_x);
        let mut out = vec![blank.clone(); self.padding_y];
        for line in lines {
            let mut value = format!("{}{}", left, line);
            value.truncate(width);
            value.push_str(&" ".repeat(width.saturating_sub(value.chars().count())));
            out.push(value);
        }
        out.extend(std::iter::repeat_n(blank, self.padding_y));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::render_markdown;
    #[test]
    fn fixture_matches_plain_terminal_cells() {
        assert_eq!(
            render_markdown("# Hi\n\n- one\n- two", 12, 0, 0),
            [
                "Hi          ",
                "            ",
                "• one       ",
                "• two       "
            ]
        );
    }
    #[test]
    fn wraps_and_strips_inline_markup() {
        assert_eq!(
            render_markdown("**hello** `world`", 8, 0, 0),
            ["hello   ", "world   "]
        );
    }
}
