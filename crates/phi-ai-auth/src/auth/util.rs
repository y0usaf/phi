//! Authorization-input parsing helpers.

/// Best-effort: open `url` in the default browser via the platform opener.
/// Login flows always print the URL too, so failure here is non-fatal.
pub(crate) fn open_browser(url: &str) -> std::io::Result<()> {
    use std::process::{Command, Stdio};

    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        // rundll32 handles `&` in URLs without cmd.exe quoting games.
        let mut c = Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler").arg(url);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };

    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
}

/// Accepts a raw code, a full redirect URL, `code#state`, or a query string.
pub(crate) fn parse_authorization_input(input: &str) -> (Option<String>, Option<String>) {
    let value = input.trim();
    if value.is_empty() {
        return (None, None);
    }
    if let Ok(url) = url::Url::parse(value) {
        let mut code = None;
        let mut state = None;
        for (key, val) in url.query_pairs() {
            match key.as_ref() {
                "code" => code = Some(val.into_owned()),
                "state" => state = Some(val.into_owned()),
                _ => {}
            }
        }
        return (code, state);
    }
    if let Some((code, state)) = value.split_once('#') {
        return (Some(code.to_owned()), Some(state.to_owned()));
    }
    if value.contains("code=") {
        let mut code = None;
        let mut state = None;
        for pair in value.split('&') {
            if let Some((key, val)) = pair.split_once('=') {
                match key {
                    "code" => code = Some(val.to_owned()),
                    "state" => state = Some(val.to_owned()),
                    _ => {}
                }
            }
        }
        return (code, state);
    }
    (Some(value.to_owned()), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_authorization_inputs() {
        assert_eq!(
            parse_authorization_input("http://localhost:1455/auth/callback?code=abc&state=xyz"),
            (Some("abc".into()), Some("xyz".into()))
        );
        assert_eq!(
            parse_authorization_input("abc#xyz"),
            (Some("abc".into()), Some("xyz".into()))
        );
        assert_eq!(
            parse_authorization_input("code=abc&state=xyz"),
            (Some("abc".into()), Some("xyz".into()))
        );
        assert_eq!(parse_authorization_input("abc"), (Some("abc".into()), None));
        assert_eq!(parse_authorization_input("  "), (None, None));
    }
}
