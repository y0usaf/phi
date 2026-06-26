//! Payload redaction: safe-field filtering for observability payloads.

use serde_json::Value;

/// Safe payload redaction: returns only safe fields. Unsafe content
/// (prompts, completions, tool args/results, file contents, api keys) is
/// never included. When unsure, returns `Value::Null`.
pub fn safe_payload(name: &str, payload: &Value) -> Value {
    let safe_keys: &[&str] = &[
        "provider",
        "model",
        "session_id",
        "entry_type",
        "tool_name",
        "status_code",
        "stop_reason",
        "input_tokens",
        "output_tokens",
        "cache_read",
        "cache_write",
        "total_tokens",
        "cost",
        "duration_ms",
        "iterations",
        "depth",
    ];
    if let Some(obj) = payload.as_object() {
        let filtered: serde_json::Map<String, Value> = obj
            .iter()
            .filter(|(k, _)| safe_keys.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Value::Object(filtered)
    } else {
        // Non-object payloads carry no safe key/value structure to filter, so
        // redact them to null regardless of event name. (Safe by over-redaction:
        // a raw scalar payload could be a prompt or completion, which must never
        // pass through. `name` is retained for future structured allow-listing.)
        let _ = name;
        Value::Null
    }
}
