use std::collections::HashMap;
use std::io::{IsTerminal, Read};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use phi_ai::{
    AuthStorage, LoginMethod, Model, OAuthProviderId, PrintCallbacks, Registry, ThinkingLevel,
    Usage,
};
use phi_builtins::prelude::{CODING_PRELUDE, coding_tools_section};
use phi_cli::Args;
use phi_mux as mux;
use phi_rlm::engine::PythonEnvFactory;
use phi_rlm::{ContextPayload, LimitKind, ProviderLm, Rlm, RlmConfig, RlmEvent, StopCause};
use phi_session::{config, session_builder, tui};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let storage = Arc::new(AuthStorage::new(auth_path()?));

    if let Some(provider) = &args.logout {
        let provider = parse_oauth_provider(provider)?;
        storage.logout(provider.storage_key())?;
        println!("Logged out of {}", provider.storage_key());
        return Ok(());
    }

    if let Some(provider) = &args.login {
        let provider = parse_oauth_provider(provider)?;
        let method = if args.device_code {
            LoginMethod::DeviceCode
        } else {
            LoginMethod::Browser
        };
        storage
            .login(provider, method, &PrintCallbacks, &CancellationToken::new())
            .await?;
        println!("Logged in to {}", provider.storage_key());
        return Ok(());
    }

    let registry = Arc::new(Registry::builtin());

    if let Some(pattern) = &args.list_models {
        for model in registry.supported_models() {
            let qualified = format!("{}/{}", model.provider, model.id);
            if pattern.is_empty() || qualified.contains(pattern.as_str()) {
                println!(
                    "{qualified:<40} {:<28} {}",
                    model.name,
                    format_model_pricing(model)
                );
            }
        }
        return Ok(());
    }

    run(args, storage, registry).await
}

fn format_tokens(count: u64) -> String {
    match count {
        0..=999 => count.to_string(),
        1_000..=9_999 => format!("{:.1}k", count as f64 / 1_000.0),
        10_000..=999_999 => format!("{}k", count / 1_000),
        _ => format!("{:.1}M", count as f64 / 1_000_000.0),
    }
}

fn format_rate(rate: f64) -> String {
    if rate == 0.0 {
        "$0/M".to_owned()
    } else if rate >= 10.0 {
        format!("${rate:.0}/M")
    } else if rate >= 1.0 {
        format!("${rate:.2}/M")
    } else {
        format!("${rate:.4}/M")
    }
}

fn format_model_pricing(model: &Model) -> String {
    let mut parts = vec![
        format!("in {}", format_rate(model.cost.input)),
        format!("out {}", format_rate(model.cost.output)),
    ];
    if model.cost.cache_write > 0.0 {
        parts.push(format!("cache+ {}", format_rate(model.cost.cache_write)));
    }
    if model.cost.cache_read > 0.0 {
        parts.push(format!("cache↺ {}", format_rate(model.cost.cache_read)));
    }
    parts.join(" · ")
}

fn format_usage_summary(usage: &Usage) -> String {
    format!(
        "usage: in {} · out {} · cache+ {} · cache↺ {} · total {} · ${:.4}",
        format_tokens(usage.input),
        format_tokens(usage.output),
        format_tokens(usage.cache_write),
        format_tokens(usage.cache_read),
        format_tokens(usage.total_tokens),
        usage.cost.total
    )
}

async fn run(args: Args, storage: Arc<AuthStorage>, registry: Arc<Registry>) -> Result<()> {
    let cwd = std::env::current_dir().context("could not determine working directory")?;
    let prompt = args.prompt_text();
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let tui_mode = prompt.is_none() && args.context.is_none() && tty;

    if args.mux || tui_mode {
        let mux_config = config::Config::load(&cwd)?.mux.unwrap_or_default();
        let mux_enabled =
            args.mux || (!args.no_mux && tui_mode && mux_config.enabled.unwrap_or(false));
        if mux_enabled {
            if !tty {
                bail!("--mux requires an interactive terminal");
            }
            if prompt.is_some() || args.context.is_some() {
                bail!("--mux does not take a prompt or --context");
            }
            let shared = session_builder::SharedResources {
                args,
                storage,
                registry,
            };
            return mux::app::run(shared, mux_config, cwd).await;
        }
    }

    if tui_mode {
        let shared = session_builder::SharedResources {
            args,
            storage,
            registry,
        };
        let setup = session_builder::build_session(&cwd, &shared, None).await?;
        return tui::run(setup, tui::SessionIo::standalone(cwd)).await;
    }

    let settings = config::Settings::resolve(&args, config::Config::load(&cwd)?, false);

    let root_model = session_builder::find_model(&registry, &settings.model)?;
    let sub_model = match &settings.sub_model {
        Some(query) => session_builder::find_model(&registry, query)?,
        None => root_model.clone(),
    };
    let thinking: ThinkingLevel = settings
        .thinking
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;
    let sub_model_thinking: ThinkingLevel = settings
        .sub_model_thinking
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;

    if let Some(key) = &args.api_key {
        storage.set_runtime_api_key(&root_model.provider, key);
        if sub_model.provider != root_model.provider {
            storage.set_runtime_api_key(&sub_model.provider, key);
        }
    }
    for provider in [&root_model.provider, &sub_model.provider] {
        if storage.resolve(provider).await?.is_none() {
            bail!(
                "no credentials for provider '{provider}': run `phi --login {provider}`, \
                 pass --api-key, or set {}_API_KEY",
                provider.to_uppercase()
            );
        }
    }

    let config = RlmConfig {
        max_depth: settings.max_depth,
        max_iterations: settings.max_iterations,
        max_budget: settings.budget,
        max_timeout: settings.timeout.map(Duration::from_secs),
        max_tokens: settings.max_tokens,
        max_errors: settings.max_errors,
        max_concurrent_subcalls: settings.max_concurrent_subcalls,
        setup_code: Some(CODING_PRELUDE.to_owned()),
        tools_section: Some(coding_tools_section()),
        cwd: Some(cwd.display().to_string()),
        user_prologue: Some(format!(
            "You are a coding agent operating directly in the user's working directory: {}.              Use the pre-defined helper functions to explore and modify the project.",
            cwd.display()
        )),
        ..RlmConfig::default()
    };

    let root = Arc::new(ProviderLm::new(
        registry.clone(),
        root_model,
        storage.clone(),
        thinking,
    ));
    let sub = Arc::new(ProviderLm::new(
        registry.clone(),
        sub_model,
        storage.clone(),
        sub_model_thinking,
    ));
    let envs = Arc::new(PythonEnvFactory {
        python: settings.python.clone(),
    });

    let (payload, root_prompt) = match &args.context {
        Some(source) => (ContextPayload::Str(read_context(source)?), prompt.clone()),
        None => match prompt.clone() {
            Some(text) => (ContextPayload::Str(text), None),
            None => {
                bail!("provide a prompt and/or --context (or run on a TTY for the interactive TUI)")
            }
        },
    };

    let mut rlm = Rlm::new(config, root, sub, envs);
    session_builder::register_supported_named_models(
        &mut rlm,
        registry.clone(),
        storage.clone(),
        sub_model_thinking,
    );
    let mut printer = None;
    if !args.quiet {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        rlm = rlm.with_events(tx);
        printer = Some(tokio::spawn(print_events(
            rx,
            args.verbose,
            settings.max_iterations,
        )));
    }

    let cancel = CancellationToken::new();
    let ctrl_c_cancel = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\ninterrupted, finishing up...");
            ctrl_c_cancel.cancel();
        }
    });

    let result = rlm
        .completion(payload, root_prompt.as_deref(), &cancel)
        .await;
    drop(rlm);
    if let Some(printer) = printer {
        let _ = printer.await;
    }

    println!("{}", result.response);
    if !args.quiet {
        eprintln!(
            "\n[{:?}] {} iteration(s), {:.1}s",
            result.stop,
            result.iterations,
            result.duration.as_secs_f64(),
        );
        eprintln!("{}", format_usage_summary(&result.usage));
    }
    if result.stop == StopCause::Failed {
        bail!(result.error.unwrap_or_else(|| "run failed".to_owned()));
    }
    Ok(())
}

fn auth_path() -> Result<std::path::PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "phi")
        .context("could not determine a home directory")?;
    Ok(dirs.data_dir().join("auth.json"))
}

fn parse_oauth_provider(name: &str) -> Result<OAuthProviderId> {
    OAuthProviderId::parse(name)
        .with_context(|| format!("unknown provider '{name}' (expected: anthropic, openai)"))
}

fn read_context(source: &str) -> Result<String> {
    if source == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .context("failed to read context from stdin")?;
        return Ok(buffer);
    }
    std::fs::read_to_string(source).with_context(|| format!("failed to read context: {source}"))
}

async fn print_events(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<RlmEvent>,
    verbose: bool,
    max_iterations: usize,
) {
    let color = std::io::stderr().is_terminal();
    let mut open_tool_args: HashMap<u64, serde_json::Value> = HashMap::new();
    while let Some(event) = rx.recv().await {
        match event {
            RlmEvent::IterationStart { depth, iteration } => {
                eprintln!(
                    "\n[turn {}/{max_iterations}]{}",
                    iteration + 1,
                    depth_tag(depth)
                );
            }
            RlmEvent::CodeExecStart { code, .. } => {
                eprintln!("{} exec", spin(color));
                eprintln!("{}", indent(&preview(&code, 16, verbose)));
            }
            RlmEvent::CodeExecEnd {
                stdout,
                stderr,
                duration,
                ..
            } => {
                let glyph = if stderr.trim().is_empty() {
                    check(color)
                } else {
                    cross(color)
                };
                if (duration.as_secs_f64() * 10.0).round() <= 0.0 {
                    eprintln!("  {glyph} exec");
                } else {
                    eprintln!("  {glyph} exec • {:.1}s", duration.as_secs_f64());
                }
                if !stdout.trim().is_empty() {
                    eprintln!("{}", indent(&preview(&stdout, 12, verbose)));
                }
                if !stderr.trim().is_empty() {
                    eprintln!("{}", indent(&preview(&stderr, 12, verbose)));
                }
            }
            RlmEvent::SubcallStart {
                depth,
                model,
                prompt_preview,
            } => {
                eprintln!(
                    "{} subcall {model}{} {prompt_preview}",
                    spin(color),
                    depth_tag(depth)
                );
            }
            RlmEvent::SubcallEnd {
                model,
                duration,
                error,
                ..
            } => match error {
                Some(error) => {
                    eprintln!("  {} subcall {model} failed: {error}", cross(color))
                }
                None => eprintln!(
                    "  {} subcall {model} {:.1}s",
                    check(color),
                    duration.as_secs_f64()
                ),
            },
            RlmEvent::ToolStart {
                call_id,
                depth,
                name,
                args,
            } => {
                open_tool_args.insert(call_id, args.clone());
                eprintln!(
                    "{} {name}{} {}",
                    spin(color),
                    depth_tag(depth),
                    tool_args_preview(&args)
                );
            }
            RlmEvent::ToolEnd {
                call_id,
                name,
                result,
                error,
                duration: _duration,
                ..
            } => {
                let args = open_tool_args
                    .remove(&call_id)
                    .unwrap_or(serde_json::Value::Null);
                eprintln!(
                    "{}",
                    render_tool_panel(
                        &name,
                        &args,
                        error.as_deref().unwrap_or(&result),
                        error.is_some(),
                        verbose,
                        color
                    )
                );
            }
            RlmEvent::AnswerSubmitted { .. } => {
                eprintln!("{} answer submitted", check(color))
            }
            RlmEvent::LimitHit { limit, detail, .. } => {
                eprintln!("{} limit {} {detail}", cross(color), limit_name(limit));
            }
            RlmEvent::RunStart { .. }
            | RlmEvent::RunEnd { .. }
            | RlmEvent::IterationEnd { .. }
            | RlmEvent::RootDelta { .. }
            | RlmEvent::RootThinkingDelta { .. }
            | RlmEvent::RootText { .. } => {}
        }
    }
}

/// Compact one-line `key=value` summary of a tool call's arguments.
fn tool_args_preview(args: &serde_json::Value) -> String {
    let Some(map) = args.as_object() else {
        return String::new();
    };
    map.iter()
        .map(|(key, value)| {
            let text = match value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let flat = text.replace('\n', "\\n");
            let short: String = flat.chars().take(60).collect();
            let ellipsis = if short.len() < flat.len() { "…" } else { "" };
            format!("{key}={short}{ellipsis}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// ANSI styling for scrollback event glyphs (gated on a tty in `print_events`).
const GLYPH_RESET: &str = "\x1b[0m";
const GLYPH_ACTIVE: &str = "\x1b[38;2;130;160;230m";
const GLYPH_OK: &str = "\x1b[38;2;130;200;140m";
const GLYPH_ERR: &str = "\x1b[38;2;240;120;120m";

/// Leading braille marker for the start of an in-flight top-level action.
fn spin(color: bool) -> String {
    if color {
        format!("{GLYPH_ACTIVE}\u{283f}{GLYPH_RESET}")
    } else {
        "\u{283f}".to_owned()
    }
}

/// Check mark settling a successfully completed action.
fn check(color: bool) -> String {
    if color {
        format!("{GLYPH_OK}\u{2713}{GLYPH_RESET}")
    } else {
        "\u{2713}".to_owned()
    }
}

/// Cross mark settling a failed action.
fn cross(color: bool) -> String {
    if color {
        format!("{GLYPH_ERR}\u{2717}{GLYPH_RESET}")
    } else {
        "\u{2717}".to_owned()
    }
}

fn depth_tag(depth: usize) -> String {
    if depth == 0 {
        String::new()
    } else {
        format!(" d{depth}")
    }
}

fn limit_name(limit: LimitKind) -> &'static str {
    match limit {
        LimitKind::Budget => "budget",
        LimitKind::Timeout => "timeout",
        LimitKind::Tokens => "tokens",
        LimitKind::Errors => "errors",
    }
}

fn preview(text: &str, max_lines: usize, verbose: bool) -> String {
    let text = text.trim_end();
    if verbose {
        return text.to_owned();
    }
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines {
        return text.to_owned();
    }
    let mut out = lines[..max_lines].join("\n");
    out.push_str(&format!("\n... ({} more lines)", lines.len() - max_lines));
    out
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("  | {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn bash_timeout_arg(args: &serde_json::Value) -> Option<u64> {
    args.get("timeout").and_then(|value| match value {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    })
}

fn parse_bash_result(text: &str) -> (Vec<String>, Option<i64>, Option<u64>) {
    let mut lines = Vec::new();
    let mut exit_code = None;
    let mut timed_out = None;
    for line in text.trim_end().lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("[exit code ")
            .and_then(|rest| rest.strip_suffix(']'))
            && let Ok(code) = rest.parse::<i64>()
        {
            exit_code = Some(code);
            continue;
        }
        if let Some(rest) = trimmed
            .strip_prefix("Error: command timed out after ")
            .and_then(|rest| rest.strip_suffix('s'))
            && let Ok(secs) = rest.parse::<u64>()
        {
            timed_out = Some(secs);
            continue;
        }
        if trimmed != "(no output)" {
            lines.push(line.to_owned());
        }
    }
    (lines, exit_code, timed_out)
}

fn tool_display_name(name: &str) -> String {
    name.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn tool_arg<'a>(args: &'a serde_json::Value, key: &str) -> &'a str {
    args.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

fn tool_title_suffix(name: &str, args: &serde_json::Value) -> Option<String> {
    match name {
        "read_file" | "read_hashline" | "apply_hashline" => {
            let path = tool_arg(args, "path");
            (!path.is_empty()).then(|| format!(" — {path}"))
        }
        "grep" | "find" | "grep_hashline" => {
            let pattern = tool_arg(args, "pattern");
            let path = tool_arg(args, "path");
            if pattern.is_empty() {
                None
            } else if path.is_empty() || path == "." {
                Some(format!(" — {pattern}"))
            } else {
                Some(format!(" — {pattern} in {path}"))
            }
        }
        "ls" => {
            let path = tool_arg(args, "path");
            Some(format!(" — {}", if path.is_empty() { "." } else { path }))
        }
        _ => None,
    }
}

fn tool_shows_args_section(name: &str) -> bool {
    !matches!(
        name,
        "read_file" | "read_hashline" | "grep" | "grep_hashline" | "find" | "ls"
    )
}

fn tool_input_lines(name: &str, args: &serde_json::Value) -> Vec<String> {
    if name == "bash" {
        let cmd = args
            .get("cmd")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if cmd.trim().is_empty() {
            vec!["$ ".to_owned()]
        } else {
            cmd.lines().map(|line| format!("$ {line}")).collect()
        }
    } else if !tool_shows_args_section(name) || args.is_null() {
        Vec::new()
    } else if let Some(map) = args.as_object() {
        if map.is_empty() {
            Vec::new()
        } else {
            serde_json::to_string_pretty(args)
                .unwrap_or_else(|_| args.to_string())
                .lines()
                .map(str::to_owned)
                .collect()
        }
    } else {
        let text = match args {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if text.trim().is_empty() {
            Vec::new()
        } else {
            text.lines().map(str::to_owned).collect()
        }
    }
}

fn tool_output_lines(
    name: &str,
    args: &serde_json::Value,
    text: &str,
) -> (Vec<String>, Vec<String>, bool) {
    if name == "bash" {
        let timeout = bash_timeout_arg(args);
        let (lines, exit_code, timed_out) = parse_bash_result(text);
        let ok = exit_code.unwrap_or(0) == 0 && timed_out.is_none();
        let mut footer = Vec::new();
        if let Some(code) = exit_code.filter(|code| *code != 0) {
            footer.push(format!("Command exited with code {code}"));
        }
        if let Some(secs) = timed_out {
            footer.push(format!("Command timed out after {secs}s"));
        }
        if let Some(secs) = timeout.or(timed_out) {
            footer.push(format!("⟦Timeout: {secs}s⟧"));
        }
        (lines, footer, ok)
    } else {
        let lines: Vec<String> = if text.trim().is_empty() {
            Vec::new()
        } else {
            text.trim_end().lines().map(str::to_owned).collect()
        };
        (lines, Vec::new(), true)
    }
}

fn terminal_preview_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(120)
        .clamp(60, 180)
}

fn plain_width(text: &str) -> usize {
    text.chars().count()
}

fn truncate_plain(text: &str, width: usize) -> String {
    if plain_width(text) <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let keep = width.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    out.push('…');
    out
}

fn pad_plain(text: &str, width: usize) -> String {
    let text = truncate_plain(text, width);
    let pad = width.saturating_sub(plain_width(&text));
    format!("{text}{}", " ".repeat(pad))
}

fn preview_tail_lines(lines: &[String], max_lines: usize, verbose: bool) -> Vec<String> {
    if verbose || lines.len() <= max_lines {
        return lines.to_vec();
    }
    let hidden = lines.len() - max_lines;
    let mut out = vec![format!(
        "… ({} earlier lines, showing {} of {})",
        hidden,
        max_lines,
        lines.len()
    )];
    out.extend(lines[lines.len() - max_lines..].iter().cloned());
    out
}

fn panel_border(label: &str, label_plain: &str, left: char, right: char, width: usize) -> String {
    let fill = width.saturating_sub(plain_width(label_plain) + 7).max(1);
    format!(" {left}─── {label} {}{right}", "─".repeat(fill))
}

fn panel_row(text: &str, width: usize) -> String {
    let inner = width.saturating_sub(4).max(1);
    format!(" │ {} │", pad_plain(text, inner))
}

fn panel_bottom(width: usize) -> String {
    format!(" └{}┘", "─".repeat(width.saturating_sub(2).max(1)))
}

fn render_tool_panel(
    name: &str,
    args: &serde_json::Value,
    text: &str,
    failed: bool,
    verbose: bool,
    color: bool,
) -> String {
    let width = terminal_preview_width().saturating_sub(1).max(40);
    let display = tool_display_name(name);
    let display = format!(
        "{display}{}",
        tool_title_suffix(name, args).unwrap_or_default()
    );
    let (parsed_lines, footer_lines, parsed_ok) = tool_output_lines(name, args, text);
    let ok = !failed && parsed_ok;
    let title_plain = format!("{} {display}", if ok { "✔" } else { "✘" });
    let title = if color {
        if ok {
            format!("{GLYPH_OK}{title_plain}{GLYPH_RESET}")
        } else {
            format!("{GLYPH_ERR}{title_plain}{GLYPH_RESET}")
        }
    } else {
        title_plain.clone()
    };

    let mut out = Vec::new();
    out.push(panel_border(&title, &title_plain, '┌', '┐', width));

    let input_lines = tool_input_lines(name, args);
    if tool_shows_args_section(name) || name == "bash" {
        let input_label = if name == "bash" { "Command" } else { "Args" };
        out.push(panel_border(input_label, input_label, '├', '┤', width));
        if input_lines.is_empty() {
            out.push(panel_row("(no input)", width));
        } else {
            for line in input_lines {
                out.push(panel_row(&line, width));
            }
        }
    }

    out.push(panel_border("Output", "Output", '├', '┤', width));
    let output_lines = preview_tail_lines(&parsed_lines, 10, verbose);
    if output_lines.is_empty() {
        out.push(panel_row("(no output)", width));
    } else {
        for line in output_lines {
            out.push(panel_row(&line, width));
        }
    }
    for line in footer_lines {
        out.push(panel_row(&line, width));
    }
    out.push(panel_bottom(width));
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use phi_ai::{Api, Cost, CostRates};
    use std::collections::BTreeMap;

    fn test_model() -> Model {
        Model {
            id: "m".into(),
            name: "Model".into(),
            api: Api::AnthropicMessages,
            provider: "anthropic".into(),
            base_url: "https://example.test".into(),
            reasoning: false,
            input: vec![],
            cost: CostRates {
                input: 3.0,
                output: 15.0,
                cache_read: 0.3,
                cache_write: 3.75,
            },
            context_window: 200_000,
            max_tokens: 8_192,
            headers: BTreeMap::new(),
            thinking_levels: None,
        }
    }

    #[test]
    fn format_model_pricing_includes_cache_rates_when_present() {
        assert_eq!(
            format_model_pricing(&test_model()),
            "in $3.00/M · out $15/M · cache+ $3.75/M · cache↺ $0.3000/M"
        );
    }

    #[test]
    fn format_usage_summary_shows_breakdown_and_total() {
        let usage = Usage {
            input: 1_200,
            output: 34_000,
            cache_read: 5_600,
            cache_write: 7_800,
            total_tokens: 48_600,
            cost: Cost {
                total: 0.1234,
                ..Cost::default()
            },
        };
        assert_eq!(
            format_usage_summary(&usage),
            "usage: in 1.2k · out 34k · cache+ 7.8k · cache↺ 5.6k · total 48k · $0.1234"
        );
    }

    #[test]
    fn observational_tools_hide_args_and_prettify_names() {
        assert_eq!(tool_display_name("read_file"), "Read File");
        assert_eq!(tool_display_name("read_hashline"), "Read Hashline");
        assert!(!tool_shows_args_section("read_file"));
        assert!(!tool_shows_args_section("read_hashline"));
        assert!(
            tool_input_lines("read_file", &serde_json::json!({"path": "src/main.rs"})).is_empty()
        );
        assert!(
            tool_input_lines("read_hashline", &serde_json::json!({"path": "src/main.rs"}))
                .is_empty()
        );
        assert!(
            !tool_input_lines("write_file", &serde_json::json!({"path": "src/main.rs"})).is_empty()
        );
        assert_eq!(
            tool_title_suffix("read_file", &serde_json::json!({"path": "src/main.rs"})),
            Some(" — src/main.rs".to_owned())
        );
        assert_eq!(
            tool_title_suffix("read_hashline", &serde_json::json!({"path": "src/main.rs"})),
            Some(" — src/main.rs".to_owned())
        );
        assert!(
            !tool_shows_args_section("grep_hashline"),
            "grep_hashline should render as observational"
        );
    }
}
