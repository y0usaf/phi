pub use clap;
use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(
    name = "phi",
    version,
    about = "Recursive Language Model (RLM) coding agent"
)]
pub struct Args {
    /// One-shot prompt; without it (on a TTY) the interactive TUI starts.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub prompt: Vec<String>,

    /// Long context handed to the REPL: a file path, or '-' for stdin.
    /// Without it, the prompt itself becomes the context.
    #[arg(long, value_name = "PATH")]
    pub context: Option<String>,

    /// Root model id, optionally qualified as provider/model-id
    /// [default: claude-opus-4-7].
    #[arg(long)]
    pub model: Option<String>,

    /// Model for llm_query sub-calls (defaults to the root model).
    #[arg(long, value_name = "MODEL")]
    pub sub_model: Option<String>,

    /// Thinking level for the root model: off|minimal|low|medium|high|xhigh
    /// [default: off].
    #[arg(long)]
    pub thinking: Option<String>,

    /// Thinking level for the llm_query/subagent sub-model.
    /// Defaults to --thinking when omitted.
    #[arg(long, value_name = "LEVEL")]
    pub sub_model_thinking: Option<String>,

    /// Maximum turns for each recursive subagent child [default: 30].
    /// The top-level RLM is not capped by this setting.
    #[arg(long)]
    pub max_iterations: Option<usize>,

    /// Maximum subagent recursion depth [default: 3 in the TUI, 2 one-shot]
    #[arg(long)]
    pub max_rlm_depth: Option<usize>,

    /// Maximum spend in USD across the run.
    #[arg(long, value_name = "USD")]
    pub budget: Option<f64>,

    /// Maximum total tokens (input + output) across the run.
    #[arg(long, value_name = "N")]
    pub max_tokens: Option<u64>,

    /// Wall-clock limit in seconds.
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<u64>,

    /// Stop after N consecutive REPL errors.
    #[arg(long, value_name = "N")]
    pub max_errors: Option<usize>,

    /// [default: 4]
    #[arg(long)]
    pub max_concurrent_subcalls: Option<usize>,

    /// Python interpreter for the REPL environment [default: python3].
    #[arg(long)]
    pub python: Option<String>,

    /// OAuth login and exit: anthropic | openai.
    #[arg(long, value_name = "PROVIDER")]
    pub login: Option<String>,

    /// Use the device-code flow for --login (openai only).
    #[arg(long)]
    pub device_code: bool,

    /// Remove stored credentials and exit: anthropic | openai.
    #[arg(long, value_name = "PROVIDER")]
    pub logout: Option<String>,

    /// List known models (optionally filtered) and exit.
    #[arg(long, value_name = "PATTERN", num_args = 0..=1, default_missing_value = "")]
    pub list_models: Option<String>,

    /// Run as a session multiplexer (one process, many sessions).
    #[arg(long, conflicts_with = "no_mux")]
    pub mux: bool,

    /// Disable mux for this run, even if enabled in Lua config.
    #[arg(long, conflicts_with = "mux")]
    pub no_mux: bool,

    /// Additional project directory for the multiplexer (repeatable).
    #[arg(long, value_name = "DIR")]
    pub project: Vec<String>,

    /// Max output lines to show per tool card. 0 = header only.
    #[arg(long, value_name = "N")]
    pub tools_tool_max_lines: Option<usize>,

    /// Max code lines to show per REPL block. 0 = header only.
    #[arg(long, value_name = "N")]
    pub tools_repl_max_lines: Option<usize>,

    /// Box padding around tool/subcall/status cards (0 or 1).
    #[arg(long, value_name = "N")]
    pub tools_tool_padding: Option<usize>,

    /// Box padding around REPL blocks (0 or 1).
    #[arg(long, value_name = "N")]
    pub tools_repl_padding: Option<usize>,

    /// Box padding around user message cards (0 or 1).
    #[arg(long, value_name = "N")]
    pub tools_user_padding: Option<usize>,

    /// Gap between tool blocks (0 or 1).
    #[arg(long, value_name = "N")]
    pub tools_gap: Option<usize>,

    /// Consolidate tool uses into a summary card instead of individual cards.
    #[arg(long)]
    pub tools_tool_summary: Option<bool>,

    /// Print full REPL code and output instead of trimmed previews.
    #[arg(short, long)]
    pub verbose: bool,

    /// Suppress progress output; print only the final answer.
    #[arg(short, long)]
    pub quiet: bool,

    /// Output format for one-shot mode: pretty (default) or ndjson.
    #[arg(long, value_name = "FORMAT", value_parser = ["pretty", "ndjson"])]
    pub output: Option<String>,
}

impl Args {
    pub fn parse() -> Self {
        Parser::parse()
    }

    pub fn prompt_text(&self) -> Option<String> {
        if self.prompt.is_empty() {
            None
        } else {
            Some(self.prompt.join(" "))
        }
    }

    pub fn to_session_args(&self) -> phi_session::config::CliArgs {
        phi_session::config::CliArgs {
            model: self.model.clone(),
            sub_model: self.sub_model.clone(),
            thinking: self.thinking.clone(),
            sub_model_thinking: self.sub_model_thinking.clone(),
            python: self.python.clone(),
            max_iterations: self.max_iterations,
            max_rlm_depth: self.max_rlm_depth,
            budget: self.budget,
            max_tokens: self.max_tokens,
            timeout: self.timeout,
            max_errors: self.max_errors,
            max_concurrent_subcalls: self.max_concurrent_subcalls,
            tools_tool_max_lines: self.tools_tool_max_lines,
            tools_repl_max_lines: self.tools_repl_max_lines,
            tools_tool_padding: self.tools_tool_padding,
            tools_repl_padding: self.tools_repl_padding,
            tools_user_padding: self.tools_user_padding,
            tools_gap: self.tools_gap,
            tools_tool_summary: self.tools_tool_summary,
        }
    }
}
