//! Tool categorization and summary rendering, shared by the CLI (`phi-app`)
//! and the inline TUI (`phi-session-tui`).

use crate::session::ToolCall;

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
    pub fn label(self) -> &'static str {
        match self {
            Self::Reads => "reads",
            Self::Edits => "edits",
            Self::Commands => "commands",
            Self::Searches => "searches",
            Self::Other => "other",
        }
    }

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

/// Per-category tallies for a set of tool calls.
#[derive(Clone, Debug, Default)]
pub struct CategoryTally {
    pub ok: usize,
    pub failed: usize,
}

/// Build a per-category tally from a slice of tool calls.
pub fn tally(tools: &[ToolCall]) -> [(ToolCategory, CategoryTally); 5] {
    let mut out = [
        (ToolCategory::Reads, CategoryTally::default()),
        (ToolCategory::Edits, CategoryTally::default()),
        (ToolCategory::Commands, CategoryTally::default()),
        (ToolCategory::Searches, CategoryTally::default()),
        (ToolCategory::Other, CategoryTally::default()),
    ];
    for tool in tools {
        let cat = categorize(&tool.name);
        let idx = ToolCategory::order()
            .iter()
            .position(|c| *c == cat)
            .unwrap();
        if tool.error.is_some() {
            out[idx].1.failed += 1;
        } else {
            out[idx].1.ok += 1;
        }
    }
    out
}
