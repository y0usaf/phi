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

/// Deadline for a single `$(cmd)` expansion: a config command like
/// `$(tail -f /dev/null)` must not hang server startup forever.
const RUN_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn run_command(cmd: &str) -> String {
    let mut child = match std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    // Drain stdout on a thread so a chatty command can't fill the pipe and
    // deadlock while we wait; bound the whole expansion with a deadline.
    let stdout_handle = child.stdout.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = std::io::Read::read_to_string(&mut s, &mut buf);
            buf
        })
    });

    let deadline = std::time::Instant::now() + RUN_COMMAND_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                eprintln!(
                    "warning: MCP $() command timed out after {}s: {}",
                    RUN_COMMAND_TIMEOUT.as_secs(),
                    cmd
                );
                break None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(_) => break None,
        }
    };

    let stdout = match stdout_handle {
        Some(h) => h.join().unwrap_or_default(),
        None => String::new(),
    };
    match status {
        Some(s) if s.success() => stdout.trim_end_matches('\n').to_owned(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{RUN_COMMAND_TIMEOUT, shell_expand};

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
    #[test]
    fn command_expansion_times_out() {
        let start = std::time::Instant::now();
        let out = shell_expand("$(tail -f /dev/null)");
        assert_eq!(out, "");
        assert!(start.elapsed() < RUN_COMMAND_TIMEOUT + std::time::Duration::from_secs(5));
    }

}
