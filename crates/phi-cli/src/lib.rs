pub use clap;
use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(name = "phi", about = "Recursive Language Model (RLM) coding agent")]
pub struct Args {
    /// One-shot prompt; without it (on a TTY) the interactive TUI starts.
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

    /// Thinking level for the llm_query/rlm_query sub-model.
    /// Defaults to --thinking when omitted.
    #[arg(long, value_name = "LEVEL")]
    pub sub_model_thinking: Option<String>,

    /// Maximum turns for each recursive rlm_query child [default: 30].
    /// The top-level RLM is not capped by this setting.
    #[arg(long)]
    pub max_iterations: Option<usize>,

    /// Maximum rlm_query recursion depth [default: 3 in the TUI, 2 one-shot]
    #[arg(long)]
    pub max_depth: Option<usize>,

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

    /// API key for the root model's provider (this run only, not persisted).
    #[arg(long, value_name = "KEY")]
    pub api_key: Option<String>,

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

    /// Print full REPL code and output instead of trimmed previews.
    #[arg(short, long)]
    pub verbose: bool,

    /// Suppress progress output; print only the final answer.
    #[arg(short, long)]
    pub quiet: bool,
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
}
