//! Shell expansion for MCP `env`/`headers` values (crush's semantics): a
//! config value like `"Bearer $TOKEN"` or `"$(op read op://vault/key)"` is
//! resolved when the server starts, so secrets come from the environment or
//! a command instead of living in the config file.
//!
//! Supported forms: `$VAR`, `${VAR}` (environment lookup; unset → empty) and
//! `$(command)` (run through `sh -c`, stdout captured, trailing newline
//! trimmed; failure → empty). Anything else passes through literally.

/// Expand `$VAR`, `${VAR}`, and `$(command)` in `value`.
pub fn shell_expand(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' || i + 1 >= chars.len() {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        match chars[i + 1] {
            '{' => {
                if let Some(close) = chars[i + 2..].iter().position(|&c| c == '}') {
                    let name: String = chars[i + 2..i + 2 + close].iter().collect();
                    out.push_str(&std::env::var(name).unwrap_or_default());
                    i += close + 3;
                } else {
                    out.push('$');
                    i += 1;
                }
            }
            '(' => {
                // Find the matching close paren (nesting-aware, so
                // `$(echo $(date))` resolves as one command).
                let mut depth = 0usize;
                let mut end = None;
                for (j, &c) in chars.iter().enumerate().skip(i + 1) {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = Some(j);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                match end {
                    Some(j) => {
                        let cmd: String = chars[i + 2..j].iter().collect();
                        out.push_str(&run_command(&cmd));
                        i = j + 1;
                    }
                    None => {
                        out.push('$');
                        i += 1;
                    }
                }
            }
            c if c == '_' || c.is_ascii_alphabetic() => {
                let mut j = i + 1;
                while j < chars.len() && (chars[j] == '_' || chars[j].is_ascii_alphanumeric()) {
                    j += 1;
                }
                let name: String = chars[i + 1..j].iter().collect();
                out.push_str(&std::env::var(name).unwrap_or_default());
                i = j;
            }
            _ => {
                out.push('$');
                i += 1;
            }
        }
    }
    out
}

fn run_command(cmd: &str) -> String {
    match std::process::Command::new("sh").arg("-c").arg(cmd).output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .trim_end_matches('\n')
            .to_owned(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::shell_expand;

    // Tests run in parallel; each uses its own uniquely-named variable.
    fn with_var(name: &str, value: &str, f: impl FnOnce()) {
        // SAFETY: test-only; unique names avoid cross-test races.
        unsafe { std::env::set_var(name, value) };
        f();
        unsafe { std::env::remove_var(name) };
    }

    #[test]
    fn plain_values_pass_through() {
        assert_eq!(shell_expand("no dollars here"), "no dollars here");
        assert_eq!(shell_expand("50$ bill"), "50$ bill");
        assert_eq!(shell_expand("trailing $"), "trailing $");
    }

    #[test]
    fn bare_and_braced_vars_expand() {
        with_var("PHI_MCP_T1", "secret", || {
            assert_eq!(shell_expand("Bearer $PHI_MCP_T1"), "Bearer secret");
            assert_eq!(shell_expand("Bearer ${PHI_MCP_T1}!"), "Bearer secret!");
        });
    }

    #[test]
    fn unset_var_expands_to_empty() {
        assert_eq!(shell_expand("$PHI_MCP_DEFINITELY_UNSET_VAR"), "");
    }

    #[test]
    fn command_substitution_captures_stdout() {
        assert_eq!(shell_expand("$(printf hi)"), "hi");
        assert_eq!(
            shell_expand("v=$(echo nested $(echo deep))"),
            "v=nested deep"
        );
    }

    #[test]
    fn failing_command_expands_to_empty() {
        assert_eq!(shell_expand("$(false)"), "");
    }

    #[test]
    fn unclosed_forms_stay_literal() {
        assert_eq!(shell_expand("${OOPS"), "${OOPS");
        assert_eq!(shell_expand("$(oops"), "$(oops");
    }
}
