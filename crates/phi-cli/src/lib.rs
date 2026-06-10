use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(name = "phi", about = "Recursive Language Model (RLM) coding agent")]
pub struct Args {
    /// One-shot prompt; without it (on a TTY) an interactive chat starts.
    #[arg()]
    pub prompt: Vec<String>,

    /// Long context handed to the REPL: a file path, or '-' for stdin.
    /// Without it, the prompt itself becomes the context.
    #[arg(long, value_name = "PATH")]
    pub context: Option<String>,

    /// Root model id, optionally qualified as provider/model-id.
    #[arg(long, default_value = "claude-opus-4-7")]
    pub model: String,

    /// Model for llm_query sub-calls (defaults to the root model).
    #[arg(long, value_name = "MODEL")]
    pub sub_model: Option<String>,

    /// Thinking level for the root model: off|minimal|low|medium|high|xhigh.
    #[arg(long, default_value = "off")]
    pub thinking: String,

    #[arg(long, default_value_t = 30)]
    pub max_iterations: usize,

    #[arg(long, default_value_t = 1)]
    pub max_depth: usize,

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

    #[arg(long, default_value_t = 4)]
    pub max_concurrent_subcalls: usize,

    /// Python interpreter for the REPL environment.
    #[arg(long, default_value = "python3")]
    pub python: String,

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

    /// Print full REPL code and output instead of trimmed previews.
    #[arg(short, long)]
    pub verbose: bool,

    /// Suppress progress output; print only the final answer.
    #[arg(short, long)]
    pub quiet: bool,
}

impl Args {
    pub fn parse() -> Self {
        <Self as Parser>::parse()
    }

    pub fn prompt_text(&self) -> Option<String> {
        if self.prompt.is_empty() {
            None
        } else {
            Some(self.prompt.join(" "))
        }
    }
}
