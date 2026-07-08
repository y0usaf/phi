//! Port of `core/http-dispatcher.ts` — the HTTP idle-timeout setting
//! vocabulary.
//!
//! WS3.1 subset (recorded): the pure parts only. The spec's
//! `configureHttpDispatcher` installs a process-global undici dispatcher;
//! phi's transport (`phi-ai`) owns request timeouts, and wiring this
//! setting into it lands with its consumer (WS4 session wiring).

use serde_json::Value;

/// Spec: `DEFAULT_HTTP_IDLE_TIMEOUT_MS = 300_000`.
pub const DEFAULT_HTTP_IDLE_TIMEOUT_MS: u64 = 300_000;

/// Spec: `HTTP_IDLE_TIMEOUT_CHOICES` — `(label, timeoutMs)`.
pub const HTTP_IDLE_TIMEOUT_CHOICES: &[(&str, u64)] = &[
    ("30 sec", 30_000),
    ("1 min", 60_000),
    ("2 min", 120_000),
    ("5 min", 300_000),
    ("disabled", 0),
];

fn parse_number(value: f64) -> Option<u64> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    Some(value.floor() as u64)
}

/// Spec: `parseHttpIdleTimeoutMs(value)` — `"disabled"` → 0, numeric
/// strings via `Number()`, non-negative finite numbers floored;
/// everything else `undefined`.
pub fn parse_http_idle_timeout_ms(value: &Value) -> Option<u64> {
    match value {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.eq_ignore_ascii_case("disabled") {
                return Some(0);
            }
            if trimmed.is_empty() {
                return None;
            }
            parse_number(trimmed.parse::<f64>().ok()?)
        }
        Value::Number(n) => parse_number(n.as_f64()?),
        _ => None,
    }
}

/// Spec: `formatHttpIdleTimeoutMs(timeoutMs)`.
pub fn format_http_idle_timeout_ms(timeout_ms: u64) -> String {
    if let Some((label, _)) = HTTP_IDLE_TIMEOUT_CHOICES
        .iter()
        .find(|(_, ms)| *ms == timeout_ms)
    {
        return (*label).to_owned();
    }
    // Spec: `${timeoutMs / 1000} sec` — JS number formatting.
    let seconds = timeout_ms as f64 / 1000.0;
    if seconds.fract() == 0.0 {
        format!("{} sec", seconds as u64)
    } else {
        format!("{seconds} sec")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_spec_shapes() {
        assert_eq!(parse_http_idle_timeout_ms(&json!("disabled")), Some(0));
        assert_eq!(parse_http_idle_timeout_ms(&json!(" DISABLED ")), Some(0));
        assert_eq!(parse_http_idle_timeout_ms(&json!("")), None);
        assert_eq!(parse_http_idle_timeout_ms(&json!("2000")), Some(2000));
        assert_eq!(parse_http_idle_timeout_ms(&json!("1e3")), Some(1000));
        assert_eq!(parse_http_idle_timeout_ms(&json!(1500.9)), Some(1500));
        assert_eq!(parse_http_idle_timeout_ms(&json!(0)), Some(0));
        assert_eq!(parse_http_idle_timeout_ms(&json!(-1)), None);
        assert_eq!(parse_http_idle_timeout_ms(&json!("nope")), None);
        assert_eq!(parse_http_idle_timeout_ms(&json!(true)), None);
        assert_eq!(parse_http_idle_timeout_ms(&json!(null)), None);
    }

    #[test]
    fn formats_choices_and_fallback() {
        assert_eq!(format_http_idle_timeout_ms(60_000), "1 min");
        assert_eq!(format_http_idle_timeout_ms(0), "disabled");
        assert_eq!(format_http_idle_timeout_ms(45_000), "45 sec");
        assert_eq!(format_http_idle_timeout_ms(45_500), "45.5 sec");
    }
}
