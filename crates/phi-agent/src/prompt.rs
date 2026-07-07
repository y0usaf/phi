//! System prompt, pi-shaped and pi-sized (~30 lines): preamble, tool list,
//! guidelines, project context from AGENTS.md/CLAUDE.md, date and cwd
//! last. A custom head replaces the first section but keeps the tail.

use std::path::{Path, PathBuf};

use phi_host::ToolInfo;

const PREAMBLE: &str = "You are an expert coding assistant operating inside \
phi, a coding agent harness. You help users by reading files, executing \
commands, editing code, and writing new files.";

/// Context file names, in per-directory precedence order.
const CONTEXT_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

pub fn build(tools: &[ToolInfo], custom_head: Option<&str>, cwd: &Path) -> String {
    let mut out = String::new();
    match custom_head {
        Some(head) => out.push_str(head.trim_end()),
        None => {
            out.push_str(PREAMBLE);
            if !tools.is_empty() {
                out.push_str("\n\nAvailable tools:\n");
                for tool in tools {
                    let summary = tool.description.split('.').next().unwrap_or("");
                    out.push_str(&format!("- {}: {}\n", tool.name, summary.trim()));
                }
            }
            out.push_str(
                "\nGuidelines:\n\
                 - Use bash for file operations like ls, rg, find\n\
                 - Be concise in your responses\n\
                 - Show file paths clearly when working with files\n",
            );
        }
    }

    let context_files = project_context_files(global_config_dir().as_deref(), cwd);
    if !context_files.is_empty() {
        out.push_str("\n\n<project_context>\n");
        for path in &context_files {
            if let Ok(content) = std::fs::read_to_string(path) {
                out.push_str(&format!(
                    "<project_instructions path=\"{}\">\n{}\n</project_instructions>\n",
                    path.display(),
                    content.trim_end()
                ));
            }
        }
        out.push_str("</project_context>");
    }

    let today = jiff::Zoned::now().strftime("%Y-%m-%d");
    out.push_str(&format!(
        "\n\nCurrent date: {today}\nCurrent working directory: {}",
        cwd.display()
    ));
    out
}

/// Global context file first, then one per directory walking root → cwd
/// (closest to cwd last, so it wins by recency in the prompt).
pub fn project_context_files(global_dir: Option<&Path>, cwd: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(dir) = global_dir
        && let Some(f) = first_context_file(dir)
    {
        files.push(f);
    }
    let mut chain: Vec<&Path> = cwd.ancestors().collect();
    chain.reverse();
    for dir in chain {
        if let Some(f) = first_context_file(dir) {
            files.push(f);
        }
    }
    files
}

fn first_context_file(dir: &Path) -> Option<PathBuf> {
    CONTEXT_FILES
        .iter()
        .map(|name| dir.join(name))
        .find(|p| p.is_file())
}

fn global_config_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "phi").map(|d| d.config_dir().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use phi_host::ToolInfo;

    fn tool(name: &str, description: &str) -> ToolInfo {
        ToolInfo {
            name: name.to_owned(),
            description: description.to_owned(),
            input_schema: serde_json::json!({}),
            source: "<test>".to_owned(),
        }
    }

    #[test]
    fn default_prompt_lists_tools_and_ends_with_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let tools = [tool("read", "Read a file. More detail here.")];
        let prompt = build(&tools, None, dir.path());
        assert!(prompt.starts_with("You are an expert coding assistant"));
        assert!(prompt.contains("- read: Read a file\n"), "{prompt}");
        assert!(prompt.contains("Current date: "));
        assert!(
            prompt
                .trim_end()
                .ends_with(&dir.path().display().to_string())
        );
    }

    #[test]
    fn custom_head_replaces_preamble_but_keeps_tail() {
        let dir = tempfile::tempdir().unwrap();
        let prompt = build(&[], Some("Custom system prompt."), dir.path());
        assert!(prompt.starts_with("Custom system prompt."));
        assert!(!prompt.contains("expert coding assistant"));
        assert!(prompt.contains("Current working directory:"));
    }

    #[test]
    fn context_walk_orders_root_first_cwd_last() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.path().join("AGENTS.md"), "root rules").unwrap();
        std::fs::write(nested.join("CLAUDE.md"), "leaf rules").unwrap();
        let global = tempfile::tempdir().unwrap();
        std::fs::write(global.path().join("AGENTS.md"), "global rules").unwrap();

        let files = project_context_files(Some(global.path()), &nested);
        assert_eq!(files.len(), 3, "{files:?}");
        assert!(files[0].starts_with(global.path()));
        assert!(files[1].ends_with("AGENTS.md"));
        assert!(files[2].ends_with("CLAUDE.md"));
    }

    #[test]
    fn agents_md_wins_over_claude_md_in_one_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "a").unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "c").unwrap();
        let files = project_context_files(None, dir.path());
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("AGENTS.md"));
    }
}
