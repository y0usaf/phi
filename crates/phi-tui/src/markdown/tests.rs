use super::constants::{MarkdownPalette, RESET, BOLD, CYAN, DIM, UNDERLINE};
use super::highlight::*;

use crate::highlight_code;
use crate::highlight_diff_line;
use crate::highlight_output;
use crate::highlight_snippet;
use crate::render_markdown;
use crate::terminal::visible_width;

fn palette() -> MarkdownPalette {
    MarkdownPalette::default_const()
}

fn plain(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|l| crate::terminal::strip_ansi(l))
        .collect()
}

#[test]
fn renders_paragraphs_with_wrapping() {
    let lines = render_markdown("one two three four five six seven", 20, &palette());
    assert!(lines.len() > 1);
    for line in &lines {
        assert!(visible_width(line) <= 20, "too wide: {line:?}");
    }
    assert_eq!(plain(&lines).join(" "), "one two three four five six seven");
}

#[test]
fn renders_heading_bold() {
    let lines = render_markdown("# Title\n\nbody", 80, &palette());
    assert!(lines[0].contains(BOLD));
    assert!(lines[0].contains(CYAN));
    let p = plain(&lines);
    assert_eq!(p[0], "Title");
    assert!(!p[0].contains('#'), "literal markdown marker leaked: {p:?}");
    // H1 carries an underline rule sized to the title.
    assert_eq!(p[1], "─".repeat(5));
    assert_eq!(p.last().unwrap(), "body");
}

#[test]
fn renders_code_block_verbatim() {
    let lines = render_markdown("```rust\nlet x = 1;\nlet y = 2;\n```", 80, &palette());
    let p = plain(&lines);
    assert!(p.iter().any(|l| l.contains("let x = 1;")));
    assert!(p.iter().any(|l| l.contains("let y = 2;")));
}

#[test]
fn renders_lists_with_bullets_and_numbers() {
    let p = plain(&render_markdown("- a\n- b\n\n1. x\n2. y", 80, &palette()));
    assert!(p.iter().any(|l| l.trim_start().starts_with("• a")), "{p:?}");
    assert!(
        p.iter().any(|l| l.trim_start().starts_with("1. x")),
        "{p:?}"
    );
    assert!(
        p.iter().any(|l| l.trim_start().starts_with("2. y")),
        "{p:?}"
    );
}

#[test]
fn wrapped_list_items_hang_under_text() {
    let p = plain(&render_markdown(
        "- alpha beta gamma delta epsilon\n- short",
        20,
        &palette(),
    ));
    assert!(p[0].starts_with("• alpha"), "{p:?}");
    // Continuations align under the item text, past the marker.
    assert!(p[1].starts_with("  ") && !p[1].starts_with("   "), "{p:?}");
    assert!(p.iter().any(|l| l.starts_with("• short")), "{p:?}");

    let p = plain(&render_markdown("1. alpha beta gamma delta epsilon", 20, &palette()));
    assert!(p[0].starts_with("1. alpha"), "{p:?}");
    assert!(
        p[1].starts_with("   ") && !p[1].starts_with("    "),
        "{p:?}"
    );
}

#[test]
fn inline_code_styled() {
    let lines = render_markdown("use `foo()` here", 80, &palette());
    assert!(lines[0].contains(CYAN));
    assert_eq!(plain(&lines)[0], "use foo() here");
}

#[test]
fn links_show_styled_text_and_url() {
    let lines = render_markdown("see [docs](https://example.com) now", 80, &palette());
    assert!(lines[0].contains(UNDERLINE));
    assert_eq!(plain(&lines)[0], "see docs (https://example.com) now");
    // Autolinks don't duplicate the URL.
    let lines = render_markdown("go to <https://example.com>", 80, &palette());
    assert_eq!(plain(&lines)[0], "go to https://example.com");
}

#[test]
fn fenced_code_is_highlighted_and_closed() {
    let lines = render_markdown("```rust\nlet x = 1;\n```", 80, &palette());
    let p = plain(&lines);
    assert_eq!(p.first().unwrap(), "\u{258d} rust");
    assert!(p.last().unwrap().starts_with('\u{258f}'), "{p:?}");
    // Highlighted lines carry truecolor escapes, not plain DIM.
    let code = lines
        .iter()
        .find(|l| crate::terminal::strip_ansi(l).contains("let x"))
        .unwrap();
    assert!(code.contains("\x1b[38;2;"), "no highlight: {code:?}");
}

#[test]
fn unknown_language_falls_back_to_dim() {
    let lines = render_markdown("```nosuchlang\nfoo bar\n```", 80, &palette());
    let code = lines.iter().find(|l| l.contains("foo bar")).unwrap();
    assert!(code.contains(DIM));
}

#[test]
fn common_language_aliases_are_highlighted() {
    let p = palette();
    for lang in ["ts", "tsx", "js", "jsx", "py", "sh", "shell", "console"] {
        let sample = match lang {
            "py" => "print(1)",
            "sh" | "shell" | "console" => "echo hi",
            "ts" | "tsx" => "const x: number = 1;",
            _ => "const x = 1;",
        };
        let lines = render_markdown(&format!("```{lang}\n{sample}\n```"), 80, &p);
        let code = lines
            .iter()
            .find(|l| crate::terminal::strip_ansi(l).contains(sample))
            .unwrap();
        assert!(
            code.contains("\x1b[38;2;"),
            "alias {lang} was not highlighted: {code:?}"
        );
    }
}

#[test]
fn blockquote_prefixed() {
    let p = plain(&render_markdown("> quoted text", 80, &palette()));
    assert!(p.iter().any(|l| l.contains("│ quoted text")), "{p:?}");
}

#[test]
fn highlight_code_treats_repl_as_python() {
    let lines = highlight_code("print(1)", Some("repl"), &palette());
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("\x1b[38;2;"), "no highlight: {lines:?}");
}

#[test]
fn repl_highlighting_marks_known_calls() {
    let p = palette();
    let lines = highlight_code("print(bash('echo hi'))", Some("repl"), &p);
    assert_eq!(lines.len(), 1);
    let stripped = strip_ansi(&lines[0]);
    assert!(
        stripped.contains("bash"),
        "known repl call missing: {lines:?}"
    );
    assert!(
        lines[0].contains(&p.repl_fn),
        "known repl call not highlighted: {lines:?}"
    );
}

#[test]
fn highlight_snippet_plain_without_lang() {
    let lines = highlight_snippet("plain text", None, &palette());
    assert_eq!(lines, vec!["plain text"]);
}

#[test]
fn highlight_snippet_with_lang_uses_syntax_theme() {
    let lines = highlight_snippet("fn main() {}", Some("rust"), &palette());
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("\x1b[38;2;"), "no highlight: {lines:?}");
}

#[test]
fn code_default_tokens_never_render_white() {
    // base16-ocean.dark default fg is #c0c5ce (192;197;206): near-white.
    // It must be softened to gray so only model prose reads as white.
    let lines = highlight_code("fn main() {}", Some("rust"), &palette());
    assert_eq!(lines.len(), 1);
    assert!(
        !lines[0].contains("38;2;192;197;206"),
        "near-white token survived softening: {lines:?}"
    );
    assert!(lines[0].contains("\x1b[38;2;"), "no highlight: {lines:?}");
}

#[test]
fn output_error_keyword_colored() {
    let p = palette();
    let line = highlight_output("compile error: something broke", &p);
    assert!(line.starts_with(&p.out_base), "{line:?}");
    assert!(line.contains(&p.out_err), "error accent missing: {line:?}");
    assert!(line.ends_with(RESET), "{line:?}");
    assert_eq!(strip_ansi(&line), "compile error: something broke");
}

#[test]
fn output_warning_and_success_colored() {
    let p = palette();
    let line = highlight_output("warning: deprecated API", &p);
    assert!(line.contains(&p.out_warn), "{line:?}");
    let line = highlight_output("test result: ok. 12 passed", &p);
    assert!(line.contains(&p.out_ok), "{line:?}");
}

#[test]
fn output_path_colored() {
    let p = palette();
    let line = highlight_output("  --> src/foo.rs:12:5", &p);
    assert!(line.contains(&p.out_path), "path accent missing: {line:?}");
    let line = highlight_output("see /tmp/log.txt for details", &p);
    assert!(line.contains(&p.out_path), "{line:?}");
}

#[test]
fn output_numbers_strings_urls_colored() {
    let p = palette();
    let line = highlight_output("done in 1.23s (45ms, 100%)", &p);
    assert!(line.contains(&p.out_num), "{line:?}");
    let line = highlight_output("read 'config.toml' now", &p);
    assert!(line.contains(&p.out_str), "{line:?}");
    let line = highlight_output("docs at https://example.com/x", &p);
    assert!(line.contains(&p.out_url), "{line:?}");
    assert!(line.contains(UNDERLINE), "{line:?}");
}

#[test]
fn output_with_ansi_passes_through() {
    let pre = "\x1b[31merror\x1b[0m already styled";
    assert_eq!(highlight_output(pre, &palette()), pre);
}

#[test]
fn output_handles_weird_unicode() {
    let p = palette();
    let line = highlight_output("héllo → wörld 日本語 error ✓", &p);
    assert!(line.contains(&p.out_err), "{line:?}");
    assert_eq!(
        crate::terminal::strip_ansi(&line),
        "héllo → wörld 日本語 error ✓"
    );
}

#[test]
fn diff_lines_colored() {
    let p = palette();
    assert_eq!(
        highlight_diff_line("+added", &p),
        Some(format!("{}+added{RESET}", p.diff_add))
    );
    assert_eq!(
        highlight_diff_line("-removed", &p),
        Some(format!("{}-removed{RESET}", p.diff_del))
    );
    assert_eq!(
        highlight_diff_line("@@ -1,3 +1,4 @@", &p),
        Some(format!("{}@@ -1,3 +1,4 @@{RESET}", p.repl_fence))
    );
    assert_eq!(highlight_diff_line("+++ b/file.rs", &p), None);
    assert_eq!(highlight_diff_line("--- a/file.rs", &p), None);
    assert_eq!(highlight_diff_line(" context", &p), None);
}

#[test]
fn vivify_boosts_muted_colors_and_softens_near_white() {
    // base16-ocean red #bf616a: muted, should gain saturation.
    let (r, g, b) = soften_rgb(191, 97, 106);
    let before = 191 - 97;
    let after = r.max(g).max(b) as i32 - r.min(g).min(b) as i32;
    assert!(after > before, "not more saturated: {r},{g},{b}");
    assert!(r.min(g).min(b) < 165, "boost produced near-white");
    // Near-white still softened to gray.
    let (r, g, b) = soften_rgb(192, 197, 206);
    assert!(
        r.max(g).max(b) < 165,
        "near-white not softened: {r},{g},{b}"
    );
    // Near-gray untouched.
    assert_eq!(soften_rgb(100, 105, 110), (100, 105, 110));
}

#[test]
fn list_markers_get_accent_color() {
    let p = palette();
    let lines = render_markdown("- alpha\n\n1. beta", 80, &p);
    let bullet = lines.iter().find(|l| l.contains('•')).unwrap();
    assert!(bullet.contains(&p.marker_accent), "{bullet:?}");
    let num = lines
        .iter()
        .find(|l| crate::terminal::strip_ansi(l).contains("1. beta"))
        .unwrap();
    assert!(num.contains(&p.marker_accent), "{num:?}");
    // Item text itself is not accented: accent appears before marker only.
    let after_marker = &bullet[bullet.find("alpha").unwrap()..];
    assert!(!after_marker.contains(&p.marker_accent), "{bullet:?}");
}

#[test]
fn task_list_markers_colored() {
    let p = palette();
    let lines = render_markdown("- [x] done thing\n- [ ] todo thing", 80, &p);
    let done = lines
        .iter()
        .find(|l| crate::terminal::strip_ansi(l).contains("[x]"))
        .unwrap();
    assert!(done.contains(&p.task_done), "{done:?}");
    let todo = lines
        .iter()
        .find(|l| crate::terminal::strip_ansi(l).contains("[ ]"))
        .unwrap();
    assert!(todo.contains(DIM), "{todo:?}");
}

#[test]
fn blockquote_bar_uses_accent() {
    let p = palette();
    let lines = render_markdown("> quoted text", 80, &p);
    let q = lines
        .iter()
        .find(|l| crate::terminal::strip_ansi(l).contains("│ quoted text"))
        .unwrap();
    assert!(q.contains(&p.quote_bar), "{q:?}");
}

#[test]
fn h3_headings_use_subhead_accent() {
    let p = palette();
    let lines = render_markdown("### Sub Title", 80, &p);
    assert!(lines[0].contains(BOLD), "{lines:?}");
    assert!(lines[0].contains(&p.subhead), "{lines:?}");
    let lines = render_markdown("## Big Title", 80, &p);
    assert!(lines[0].contains(CYAN), "{lines:?}");
    assert!(!lines[0].contains(&p.subhead), "{lines:?}");
}

#[test]
fn markdown_renders_repl_fences_literally() {
    let lines = render_markdown("```repl\nprint(1)\n```", 80, &palette());
    let p = plain(&lines);
    assert_eq!(p.first().unwrap(), "```repl");
    assert_eq!(p.last().unwrap(), "```");
}


#[test]
fn over_wide_tokens_are_hard_split() {
    let p = palette();
    // Long URL without spaces — wider than the line width.
    let url = "https://example.com/very/very/very/very/very/long/path";
    let lines = render_markdown(url, 20, &p);
    for line in &lines {
        assert!(visible_width(line) <= 20, "too wide: {line:?}");
    }
    let joined: String = plain(&lines)
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect();
    assert!(joined.contains(url), "URL fragmented: {joined:?}");

    // Long inline code — wider than the line width.
    let lines = render_markdown("`aaaaaaaaaaaaaaaaaaaaaaaaa`", 20, &p);
    for line in &lines {
        assert!(visible_width(line) <= 20, "too wide: {line:?}");
    }
    let joined: String = plain(&lines)
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect();
    assert!(
        joined.contains("aaaaaaaaaaaaaaaaaaaaaaaaa"),
        "code fragmented: {joined:?}"
    );
}
