use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(name = "phi", version, about = "phi — bare-core coding harness")]
pub struct Args {
    /// One-shot prompt; '-' or piped stdin also works.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub prompt: Vec<String>,

    /// Model id, optionally qualified as provider/model-id
    /// [default: claude-opus-4-7].
    #[arg(long)]
    pub model: Option<String>,

    /// Thinking level: off|minimal|low|medium|high|xhigh [default: off].
    #[arg(long)]
    pub thinking: Option<String>,

    /// System prompt for the one-shot completion.
    #[arg(long, value_name = "TEXT")]
    pub system: Option<String>,

    /// Show tool output in the transcript (hidden unless it failed).
    #[arg(long, short)]
    pub verbose: bool,

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
