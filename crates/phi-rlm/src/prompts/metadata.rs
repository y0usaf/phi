use serde_json::Value;

use crate::ContextPayload;

#[derive(Clone, Copy, Debug)]
pub struct QueryMetadata {
    pub context_type: &'static str,
    pub context_total_length: usize,
}

impl QueryMetadata {
    pub fn from_payload(payload: &ContextPayload) -> Self {
        fn value_len(value: &Value) -> usize {
            match value {
                Value::String(s) => s.chars().count(),
                other => other.to_string().chars().count(),
            }
        }
        match payload {
            ContextPayload::Str(s) => Self {
                context_type: "str",
                context_total_length: s.chars().count(),
            },
            ContextPayload::Json(Value::String(s)) => Self {
                context_type: "str",
                context_total_length: s.chars().count(),
            },
            ContextPayload::Json(Value::Object(map)) => Self {
                context_type: "dict",
                context_total_length: map.values().map(value_len).sum(),
            },
            ContextPayload::Json(Value::Array(items)) => {
                let total = items
                    .iter()
                    .map(|item| match item {
                        Value::Object(map) => match map.get("content") {
                            Some(content) => value_len(content),
                            None => item.to_string().chars().count(),
                        },
                        other => value_len(other),
                    })
                    .sum();
                Self {
                    context_type: "list",
                    context_total_length: total,
                }
            }
            ContextPayload::Json(other) => Self {
                context_type: "str",
                context_total_length: other.to_string().chars().count(),
            },
        }
    }
}

/// The metadata user message that follows the system prompt.
pub fn metadata_user_prompt(metadata: QueryMetadata, root_prompt: Option<&str>) -> String {
    let body = format!(
        "Your context is a {} of {} total characters. Each sub-LLM call can handle roughly ~100k tokens at once.",
        metadata.context_type, metadata.context_total_length
    );
    match root_prompt {
        Some(root_prompt) => format!("Answer the following: {root_prompt}\n\n{body}"),
        None => body,
    }
}
