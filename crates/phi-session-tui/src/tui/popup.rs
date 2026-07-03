//! Slash-command popup autocomplete.

use super::*;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum PopupKind {
    /// Completing a command name; enter applies and submits.
    Command,
    /// Completing a command argument; enter only applies.
    Argument,
    /// `@token` fuzzy file search; directories keep completing.
    File,
}

/// What a confirmed selector value means (pi-style editor-swap selectors).
pub(crate) enum SelectorKind {
    /// Value is a `provider/id` model name; true switches the root model.
    Model { root: bool },
    /// Read-only usage browser; confirm just closes it.
    Usage,
    /// Value is a session file path to resume.
    Resume,
    /// Value is the entry id of a user message to fork from.
    Fork,
    /// Value is the entry id to navigate the active branch to.
    Tree,
    /// An extension's ask-the-user prompt (`UiPromptRequest`); confirm sends
    /// the selected option index back. Dropping the sender unanswered (close,
    /// replacement, run end) reads as "dismissed" on the asking side.
    Prompt {
        reply: std::sync::mpsc::SyncSender<Option<usize>>,
    },
}

pub(crate) struct Suggestion {
    /// Text inserted on completion (command name without the slash, or the
    /// argument value).
    pub(crate) value: String,
    pub(crate) label: String,
    pub(crate) desc: String,
}

pub(crate) struct Popup {
    pub(crate) items: Vec<Suggestion>,
    pub(crate) selected: usize,
    /// The text ending at the cursor that a completion replaces.
    pub(crate) prefix: String,
    pub(crate) kind: PopupKind,
}

#[derive(Eq, PartialEq)]
pub(crate) enum PopupOutcome {
    /// The key was consumed by the popup.
    Handled,
    /// Let the regular input path process the key.
    Pass,
}

pub(crate) const POPUP_MAX_VISIBLE: usize = 6;

pub(crate) fn render_popup(popup: &Popup, p: &InlinePalette) -> Vec<String> {
    let total = popup.items.len();
    let start = popup
        .selected
        .saturating_sub(POPUP_MAX_VISIBLE - 1)
        .min(total.saturating_sub(POPUP_MAX_VISIBLE));
    let width = popup
        .items
        .iter()
        .map(|item| item.label.len())
        .max()
        .unwrap_or(0);
    let mut lines: Vec<String> = popup
        .items
        .iter()
        .enumerate()
        .skip(start)
        .take(POPUP_MAX_VISIBLE)
        .map(|(i, item)| {
            if i == popup.selected {
                format!(
                    "{}\u{258C}{RESET} {:<width$}  {}{}{RESET}",
                    &p.accent_fg, item.label, &p.text_tertiary, item.desc
                )
            } else {
                // Labels stay at the terminal's primary foreground (they're
                // the candidates being scanned); only the desc annotation
                // recedes to the tertiary level.
                format!(
                    "  {:<width$}  {}{}{RESET}",
                    item.label, &p.text_tertiary, item.desc
                )
            }
        })
        .collect();
    if total > start + POPUP_MAX_VISIBLE {
        lines.push(format!(
            "{}  … {} more{RESET}",
            &p.text_ghost,
            total - start - POPUP_MAX_VISIBLE
        ));
    }
    lines
}

/// Suggestions for the text before the cursor, pi-style: `/prefix` fuzzy
/// filters command names; `/command arg-prefix` filters that command's
/// arguments. Both phases are registry-driven: `commands` is the runtime's
/// command listing and `arg_candidates` queries a command's declared
/// completion source.
pub(crate) fn complete(
    before: &str,
    commands: &[phi_ext::CommandInfo],
    arg_candidates: impl Fn(&str) -> Option<Vec<String>>,
) -> Option<Popup> {
    let rest = before.strip_prefix('/')?;
    let Some((command, arg)) = rest.split_once(' ') else {
        let items: Vec<Suggestion> =
            fuzzy_filter(commands.iter().collect(), rest, |c| c.name.clone())
                .into_iter()
                .map(|c| Suggestion {
                    value: c.name.clone(),
                    label: format!("/{} {}", c.name, c.args_hint).trim_end().to_owned(),
                    desc: c.description.clone(),
                })
                .collect();
        return (!items.is_empty()).then(|| Popup {
            items,
            selected: 0,
            prefix: before.to_owned(),
            kind: PopupKind::Command,
        });
    };
    if arg.contains(' ') {
        return None;
    }
    let candidates = arg_candidates(command)?;
    let items: Vec<Suggestion> = fuzzy_filter(candidates, arg, |c| c.clone())
        .into_iter()
        .map(|c| Suggestion {
            value: c.clone(),
            label: c,
            desc: String::new(),
        })
        .collect();
    (!items.is_empty()).then(|| Popup {
        items,
        selected: 0,
        prefix: arg.to_owned(),
        kind: PopupKind::Argument,
    })
}

/// The `@`-prefixed token ending at the cursor, if any. Tokens break at
/// spaces/tabs/quotes/`=`, except inside an unclosed `@"…` quote.
pub(crate) fn extract_at_token(before: &str) -> Option<String> {
    // Unclosed `@"…` quote: the token runs from the `@` to the cursor.
    if let Some(qstart) = find_unclosed_quote(before)
        && qstart > 0
        && before[..qstart].ends_with('@')
        && is_token_start(before, qstart - 1)
    {
        return Some(before[qstart - 1..].to_owned());
    }
    let start = before
        .rfind([' ', '\t', '"', '\'', '='])
        .map_or(0, |i| i + 1);
    let token = &before[start..];
    (token.starts_with('@') && is_token_start(before, start)).then(|| token.to_owned())
}

pub(crate) fn find_unclosed_quote(text: &str) -> Option<usize> {
    let mut open = None;
    for (i, c) in text.char_indices() {
        if c == '"' {
            open = match open {
                Some(_) => None,
                None => Some(i),
            };
        }
    }
    open
}

pub(crate) fn is_token_start(text: &str, index: usize) -> bool {
    index == 0
        || matches!(
            text.as_bytes()[index - 1],
            b' ' | b'\t' | b'"' | b'\'' | b'='
        )
}

/// Collapse a message to one display line for selector rows.
pub(crate) fn single_line(text: &str) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.is_empty() {
        "(empty)".to_owned()
    } else {
        joined
    }
}
