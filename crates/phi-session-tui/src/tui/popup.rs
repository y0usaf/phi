//! Slash-command popup autocomplete.

use super::*;

pub(crate) struct CommandSpec {
    pub(crate) name: &'static str,
    pub(crate) args: &'static str,
    pub(crate) desc: &'static str,
}

pub(crate) const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "login",
        args: "[provider]",
        desc: "authenticate via OAuth (anthropic | openai)",
    },
    CommandSpec {
        name: "logout",
        args: "[provider]",
        desc: "remove stored credentials",
    },
    CommandSpec {
        name: "model",
        args: "[model]",
        desc: "switch the root model (no arg: open the selector)",
    },
    CommandSpec {
        name: "submodel",
        args: "[model]",
        desc: "switch the sub model used by llm_query",
    },
    CommandSpec {
        name: "resume",
        args: "",
        desc: "pick a past session for this directory to resume",
    },
    CommandSpec {
        name: "tree",
        args: "",
        desc: "navigate the session tree (also esc esc)",
    },
    CommandSpec {
        name: "fork",
        args: "",
        desc: "fork a new session from a prior user message",
    },
    CommandSpec {
        name: "new",
        args: "",
        desc: "reset the session (fresh REPL, history cleared)",
    },
    CommandSpec {
        name: "usage",
        args: "",
        desc: "show usage totals",
    },
    CommandSpec {
        name: "help",
        args: "",
        desc: "show commands and keys",
    },
    CommandSpec {
        name: "quit",
        args: "",
        desc: "exit (also ctrl-d on an empty line)",
    },
];

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
/// arguments.
pub(crate) fn complete(
    before: &str,
    model_names: &[String],
    ext_cmds: &[(String, String)],
) -> Option<Popup> {
    let rest = before.strip_prefix('/')?;
    let Some((command, arg)) = rest.split_once(' ') else {
        let mut items: Vec<Suggestion> =
            fuzzy_filter(COMMANDS.iter().collect(), rest, |c| c.name.to_owned())
                .into_iter()
                .map(|c| Suggestion {
                    value: c.name.to_owned(),
                    label: format!("/{} {}", c.name, c.args).trim_end().to_owned(),
                    desc: c.desc.to_owned(),
                })
                .collect();
        // Include extension commands in completion
        let ext_items: Vec<Suggestion> = ext_cmds
            .iter()
            .filter(|(name, _)| {
                rest.is_empty()
                    || name
                        .to_ascii_lowercase()
                        .contains(&rest.to_ascii_lowercase())
            })
            .map(|(name, desc)| Suggestion {
                value: name.clone(),
                label: format!("/{name}"),
                desc: desc.clone(),
            })
            .collect();
        items.extend(ext_items);
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
    let candidates: Vec<&str> = match command {
        "login" | "logout" => vec!["anthropic", "openai"],
        "model" | "submodel" => model_names.iter().map(String::as_str).collect(),
        _ => return None,
    };
    let items: Vec<Suggestion> = fuzzy_filter(candidates, arg, |c| (*c).to_owned())
        .into_iter()
        .map(|c| Suggestion {
            value: c.to_owned(),
            label: c.to_owned(),
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
