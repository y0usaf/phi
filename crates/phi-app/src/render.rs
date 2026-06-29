use crate::tool_view::*;

fn terminal_preview_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(120)
        .clamp(60, 180)
}

pub(crate) fn plain_width(text: &str) -> usize {
    text.chars().count()
}

pub(crate) fn truncate_plain(text: &str, width: usize) -> String {
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

pub(crate) fn pad_plain(text: &str, width: usize) -> String {
    let text = truncate_plain(text, width);
    let pad = width.saturating_sub(plain_width(&text));
    format!("{text}{}", " ".repeat(pad))
}

pub(crate) fn preview_tail_lines(lines: &[String], max_lines: usize, verbose: bool) -> Vec<String> {
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

pub(crate) fn panel_border(
    label: &str,
    label_plain: &str,
    left: char,
    right: char,
    width: usize,
) -> String {
    let fill = width.saturating_sub(plain_width(label_plain) + 7).max(1);
    format!(" {left}─── {label} {}{right}", "─".repeat(fill))
}

pub(crate) fn panel_row(text: &str, width: usize) -> String {
    let inner = width.saturating_sub(4).max(1);
    format!(" │ {} │", pad_plain(text, inner))
}

pub(crate) fn panel_bottom(width: usize) -> String {
    format!(" └{}┘", "─".repeat(width.saturating_sub(2).max(1)))
}

pub(crate) fn render_tool_panel(
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
        let (_, ok_color, err_color) = glyph_colors();
        if ok {
            format!("{ok_color}{title_plain}{GLYPH_RESET}")
        } else {
            format!("{err_color}{title_plain}{GLYPH_RESET}")
        }
    } else {
        title_plain.clone()
    };

    let mut out = Vec::new();
    out.push(panel_border(&title, &title_plain, '┌', '┐', width));

    let input_lines = tool_input_lines(name, args);
    let show_input = tool_shows_input_section(name);
    if show_input {
        let input_label = tool_input_label(name);
        out.push(panel_border(input_label, input_label, '├', '┤', width));
        if input_lines.is_empty() {
            out.push(panel_row("(no input)", width));
        } else {
            for line in input_lines {
                out.push(panel_row(&line, width));
            }
        }
    }

    if show_input {
        out.push(panel_border("Output", "Output", '├', '┤', width));
    }
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
