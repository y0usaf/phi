//! Tool categorization for consolidated summary cards, shared by the CLI
//! event printer (`phi-app`) and the inline TUI (`phi-session-tui`).

/// Broad category a tool belongs to, used for the consolidated summary card.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ToolCategory {
    Reads,
    Edits,
    Commands,
    Searches,
    Other,
}

impl ToolCategory {
    /// Display order for the summary card.
    pub fn order() -> &'static [ToolCategory] {
        &[
            Self::Reads,
            Self::Edits,
            Self::Commands,
            Self::Searches,
            Self::Other,
        ]
    }
}

/// Map a tool name to its summary category.
pub fn categorize(name: &str) -> ToolCategory {
    match name {
        "read_file" => ToolCategory::Reads,
        "edit_file" | "write_file" => ToolCategory::Edits,
        "bash" => ToolCategory::Commands,
        "grep" | "find" => ToolCategory::Searches,
        _ => ToolCategory::Other,
    }
}

/// Per-category `(ok, failed)` tallies from `(tool name, failed)` pairs,
/// in [`ToolCategory::order`] order.
pub fn tally<'a>(
    tools: impl IntoIterator<Item = (&'a str, bool)>,
) -> [(ToolCategory, usize, usize); 5] {
    let mut out = [
        (ToolCategory::Reads, 0, 0),
        (ToolCategory::Edits, 0, 0),
        (ToolCategory::Commands, 0, 0),
        (ToolCategory::Searches, 0, 0),
        (ToolCategory::Other, 0, 0),
    ];
    for (name, failed) in tools {
        let cat = categorize(name);
        let idx = ToolCategory::order()
            .iter()
            .position(|c| *c == cat)
            .unwrap();
        if failed {
            out[idx].2 += 1;
        } else {
            out[idx].1 += 1;
        }
    }
    out
}
