use super::constants::*;

/// Per-turn user prompt. `iteration` is 0-based.
pub fn build_user_prompt(
    iteration: usize,
    max_iterations: Option<usize>,
    context_count: usize,
    history_count: usize,
    warm: bool,
) -> String {
    let body = match max_iterations {
        Some(max_iterations) => format!("Turn {}/{}:", iteration + 1, max_iterations),
        None => format!("Turn {}:", iteration + 1),
    };
    let mut prompt = if iteration == 0 && history_count == 0 && !warm {
        format!(
            "You have not interacted with the REPL environment or seen your prompt / context yet. Look at the context first; do not provide a final answer yet.\n\n{body}"
        )
    } else {
        body
    };
    if context_count > 1 {
        prompt.push_str(&format!(
            "\n\nNote: You have {} contexts available (context_0 through context_{}).",
            context_count,
            context_count - 1
        ));
    }
    if history_count == 1 {
        prompt.push_str(
            "\n\nNote: You have 1 prior conversation history available in the `history` variable.",
        );
    } else if history_count > 1 {
        prompt.push_str(&format!(
            "\n\nNote: You have {} prior conversation histories available (history_0 through history_{}).",
            history_count,
            history_count - 1
        ));
    }
    if warm && iteration == 0 {
        prompt.push_str(
            "\n\nNote: You are continuing this session. Your previous conversation context is included above; your REPL environment and variables persist across turns. Do not re-probe `context` from scratch — build on what you already know.",
        );
    }
    prompt
}

/// A file loaded from the working directory (e.g. AGENTS.md, CLAUDE.md) to
/// inject as a `<project_context>` block in the system prompt.
#[derive(Clone, Debug, Default)]
pub struct ContextFile {
    pub name: String,
    pub content: String,
}

/// A skill discovered via SKILL.md frontmatter, injected as an
/// `<available_skills>` block in the system prompt.
#[derive(Clone, Debug, Default)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: Option<String>,
}

/// Pi-style composable system-prompt options. Each field overrides a default
/// section; `build_system_prompt` composes them in a fixed order.
///
/// Identity-neutral sections (`cyber_risk`) fall back to their `DEFAULT_*`
/// const when `None` — they are on unless overridden/empty. `env_info` is
/// caller-supplied data (like `cwd`) with no const fallback.
#[derive(Clone, Debug, Default)]
pub struct SystemPromptOptions<'a> {
    /// Opening identity line. Defaults to `DEFAULT_IDENTITY`.
    pub identity: Option<&'a str>,
    /// Dual-use security boundary + URL-guessing restriction. Defaults to
    /// `DEFAULT_CYBER_RISK`. Identity-neutral; default-on.
    pub cyber_risk: Option<&'a str>,
    /// Core REPL tool listing. Defaults to `DEFAULT_REPL_TOOLS`.
    pub core_tools: Option<&'a str>,
    /// Pre-formatted custom-tools listing (e.g. coding helpers). Wrapped in a
    /// "Custom tools and data available in the REPL:" header.
    pub custom_tools: Option<&'a str>,
    /// REPL mechanics / strategy / turn protocol. Defaults to
    /// `DEFAULT_MECHANICS`.
    pub mechanics: Option<&'a str>,
    /// Additional behavioral guidelines, rendered as a bulleted list.
    pub extra_guidelines: Vec<String>,
    /// Append the orchestrator addendum (act as coordinator, delegate).
    pub orchestrator: bool,
    /// Turn cap for this run: `Some(n)` for capped children, `None` for the
    /// uncapped root. Controls the turn-scarcity nudge.
    pub turn_cap: Option<usize>,
    /// Project-context files (AGENTS.md / CLAUDE.md) -> `<project_context>`.
    pub context_files: Vec<ContextFile>,
    /// Skills -> `<available_skills>`.
    pub skills: Vec<Skill>,
    /// `# Environment` block (cwd, is-git, platform, shell, OS version).
    /// Caller-supplied data, like `cwd`; no const fallback. Emitted before
    /// `cwd`.
    pub env_info: Option<&'a str>,
    /// Working directory, rendered as a trailing line.
    pub cwd: Option<&'a str>,
    /// Extension-hook append (from `filter_system_prompt`).
    pub append: Option<&'a str>,
}

/// Compose a system prompt from `SystemPromptOptions`, mirroring pi's
/// `buildSystemPrompt`. Sections are joined with blank lines in a fixed order:
///
/// identity → cyber_risk → core_tools → custom_tools → mechanics →
/// extra_guidelines → orchestrator → turn_scarcity → project_context →
/// available_skills → env_info → cwd → append
///
/// Identity-neutral sections fall back to their `DEFAULT_*` const (on unless
/// overridden/empty); `env_info` is caller-supplied data with no fallback.
pub fn build_system_prompt(opts: &SystemPromptOptions) -> String {
    let mut sections: Vec<String> = Vec::new();

    // identity
    sections.push(opts.identity.unwrap_or(DEFAULT_IDENTITY).to_owned());

    // cyber_risk (identity-neutral; default-on via const fallback)
    push_non_empty_or_default(&mut sections, opts.cyber_risk, DEFAULT_CYBER_RISK);

    // core_tools
    sections.push(opts.core_tools.unwrap_or(DEFAULT_REPL_TOOLS).to_owned());

    // custom_tools (optional, wrapped in a header)
    if let Some(tools) = opts.custom_tools
        && !tools.is_empty()
    {
        sections.push(format!("Custom tools and data available in the REPL:\n{tools}"));
    }

    // mechanics
    let mechanics = opts.mechanics.unwrap_or(DEFAULT_MECHANICS).to_owned();
    sections.push(mechanics);

    // extra_guidelines (optional, bulleted)
    if !opts.extra_guidelines.is_empty() {
        let bullets: Vec<String> = opts
            .extra_guidelines
            .iter()
            .map(|g| format!("- {g}"))
            .collect();
        sections.push(format!("Guidelines:\n{}", bullets.join("\n")));
    }

    // orchestrator addendum
    if opts.orchestrator {
        sections.push(ORCHESTRATOR_ADDENDUM.to_owned());
    }

    // turn scarcity nudge (capped children only)
    if opts.turn_cap.is_some() {
        sections.push(TURN_SCARCITY_NUDGE.to_owned());
    }

    // project_context
    if !opts.context_files.is_empty() {
        let blocks: Vec<String> = opts
            .context_files
            .iter()
            .map(|f| format!("<file name=\"{}\">\n{}\n</file>", f.name, f.content))
            .collect();
        sections.push(format!("<project_context>\n{}\n</project_context>", blocks.join("\n")));
    }

    // available_skills
    if !opts.skills.is_empty() {
        let entries: Vec<String> = opts
            .skills
            .iter()
            .map(|s| {
                let path_attr = s
                    .path
                    .as_deref()
                    .map(|p| format!(" path=\"{}\"", p))
                    .unwrap_or_default();
                format!(
                    "<skill name=\"{}\" description=\"{}\"{} />",
                    s.name, s.description, path_attr
                )
            })
            .collect();
        sections.push(format!("<available_skills>\n{}\n</available_skills>", entries.join("\n")));
    }

    // env_info (caller-supplied data; no const fallback) — before cwd
    push_if_non_empty(&mut sections, opts.env_info);

    // cwd
    if let Some(cwd) = opts.cwd {
        sections.push(format!("Working directory: {cwd}"));
    }

    // append (extension hook)
    if let Some(append) = opts.append
        && !append.is_empty()
    {
        sections.push(append.to_owned());
    }

    sections.join("\n\n")
}

/// Push `value` if it is `Some` and non-empty. Used for gated sections that
/// emit only when the caller opts in (no const fallback).
fn push_if_non_empty(sections: &mut Vec<String>, value: Option<&str>) {
    if let Some(text) = value
        && !text.is_empty()
    {
        sections.push(text.to_owned());
    }
}

/// Push `value`, or `default` if `value` is `None`. An explicit `Some("")` skips
/// the section entirely (override-to-off). Used for identity-neutral sections
/// that are on unless overridden/empty.
fn push_non_empty_or_default(sections: &mut Vec<String>, value: Option<&str>, default: &str) {
    match value {
        Some(text) if !text.is_empty() => sections.push(text.to_owned()),
        Some(_) => {} // explicit empty → suppress
        None => sections.push(default.to_owned()),
    }
}
