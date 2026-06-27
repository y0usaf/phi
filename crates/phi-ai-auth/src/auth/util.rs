//! Authorization-input parsing helpers.

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
