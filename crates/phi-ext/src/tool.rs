use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Where a registered tool surfaces (WS9.3a).
///
/// - `Repl` (default): a Python callable in the REPL namespace — the model
///   reaches it by writing code through the `repl` tool.
/// - `Native`: a provider-level tool definition — the model calls it directly
///   as a structured tool call, no Python in between.
/// - `Both`: one implementation, two exposures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolExposure {
    #[default]
    Repl,
    Native,
    Both,
}

impl ToolExposure {
    pub fn includes_repl(self) -> bool {
        matches!(self, Self::Repl | Self::Both)
    }

    pub fn includes_native(self) -> bool {
        matches!(self, Self::Native | Self::Both)
    }
}

/// How a tool call is executed.
#[derive(Clone)]
pub enum ToolDispatch {
    /// Invoke the registered host callback (Rust closure, Lua bridge, …).
    /// The runtime bounds the call with a watchdog timeout.
    Handler(ToolHandler),
    /// The tool is implemented by a same-named Python function already
    /// defined in the REPL environment (e.g. by the prelude). Native calls
    /// are dispatched into the env via `__phi_native_call__`, so hashline
    /// cache, read-before-edit, and spill are shared regardless of surface;
    /// REPL exposure needs no generated stub — the function is already there.
    Python,
}

#[derive(Clone, Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub params: Vec<ToolParam>,
    /// Which surfaces this tool appears on. Defaults to `Repl`.
    pub expose: ToolExposure,
    /// JSON Schema for the native tool definition. When `None`, a schema is
    /// derived from `params` (see [`ToolSpec::native_input_schema`]).
    pub input_schema: Option<serde_json::Value>,
    #[serde(skip_serializing)]
    pub dispatch: ToolDispatch,
}

impl ToolSpec {
    /// A REPL-exposed tool with a host handler — the shape every pre-WS9.3
    /// registration had.
    pub fn repl(
        name: impl Into<String>,
        description: impl Into<String>,
        params: Vec<ToolParam>,
        handler: ToolHandler,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            params,
            expose: ToolExposure::Repl,
            input_schema: None,
            dispatch: ToolDispatch::Handler(handler),
        }
    }

    /// The JSON Schema for this tool's native definition: the explicit
    /// `input_schema` when given, else derived from `params`.
    pub fn native_input_schema(&self) -> serde_json::Value {
        self.input_schema
            .clone()
            .unwrap_or_else(|| schema_from_params(&self.params))
    }
}

/// Derive a JSON Schema object from the declared params. Param `ty` strings
/// are the loose Python-ish vocabulary Lua extensions already use; anything
/// unrecognized degrades to `string`. Params without a default are required.
pub fn schema_from_params(params: &[ToolParam]) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for p in params {
        let ty = match p.ty.to_ascii_lowercase().as_str() {
            "int" | "integer" => "integer",
            "float" | "number" => "number",
            "bool" | "boolean" => "boolean",
            "list" | "array" => "array",
            "dict" | "object" => "object",
            _ => "string",
        };
        properties.insert(p.name.clone(), serde_json::json!({ "type": ty }));
        if p.default.is_none() {
            required.push(serde_json::Value::String(p.name.clone()));
        }
    }
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolParam {
    pub name: String,
    pub ty: String,
    pub default: Option<String>,
}

/// Make a wire-level parameter name safe to use as a Python parameter in a
/// generated REPL stub: invalid characters become `_`, a leading digit gets
/// a `_` prefix, and Python keywords get a trailing `_` (PEP 8 convention,
/// e.g. `from` → `from_`). The wire name stays the dict key in the stub
/// body, so the tool handler still receives the original name.
pub fn python_param_name(name: &str) -> String {
    const KEYWORDS: &[&str] = &[
        "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
        "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
        "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return",
        "try", "while", "with", "yield",
    ];
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() || out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    if KEYWORDS.contains(&out.as_str()) {
        out.push('_');
    }
    out
}

/// [`python_param_name`] for a whole signature, with collisions after
/// sanitization (`a-b` vs `a_b`) deduplicated by trailing underscores so the
/// generated `def` never repeats a parameter name.
pub fn python_param_names(params: &[ToolParam]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    params
        .iter()
        .map(|p| {
            let mut py = python_param_name(&p.name);
            while !seen.insert(py.clone()) {
                py.push('_');
            }
            py
        })
        .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolInvocation {
    pub input: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: String,
}

pub type ToolHandler = Arc<dyn Fn(ToolInvocation) -> Result<ToolOutput> + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_derivation_maps_types_and_required() {
        let params = vec![
            ToolParam {
                name: "path".into(),
                ty: "str".into(),
                default: None,
            },
            ToolParam {
                name: "limit".into(),
                ty: "int".into(),
                default: Some("10".into()),
            },
            ToolParam {
                name: "flag".into(),
                ty: "bool".into(),
                default: None,
            },
        ];
        let schema = schema_from_params(&params);
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["path"]["type"], "string");
        assert_eq!(schema["properties"]["limit"]["type"], "integer");
        assert_eq!(schema["properties"]["flag"]["type"], "boolean");
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 2);
        assert!(required.contains(&serde_json::json!("path")));
        assert!(required.contains(&serde_json::json!("flag")));
    }

    #[test]
    fn python_param_name_sanitizes_keywords_digits_and_chars() {
        assert_eq!(python_param_name("path"), "path");
        assert_eq!(python_param_name("from"), "from_");
        assert_eq!(python_param_name("async"), "async_");
        assert_eq!(python_param_name("my-param"), "my_param");
        assert_eq!(python_param_name("2fast"), "_2fast");
        assert_eq!(python_param_name(""), "_");
    }

    #[test]
    fn python_param_names_dedupes_collisions() {
        let params: Vec<ToolParam> = ["a-b", "a_b", "a.b"]
            .iter()
            .map(|n| ToolParam {
                name: (*n).to_owned(),
                ty: "str".into(),
                default: None,
            })
            .collect();
        assert_eq!(python_param_names(&params), vec!["a_b", "a_b_", "a_b__"]);
    }

    #[test]
    fn explicit_input_schema_wins_over_derivation() {
        let explicit = serde_json::json!({
            "type": "object",
            "properties": { "q": { "type": "string", "description": "query" } },
            "required": ["q"],
        });
        let spec = ToolSpec {
            name: "search".into(),
            description: "".into(),
            params: vec![ToolParam {
                name: "ignored".into(),
                ty: "str".into(),
                default: None,
            }],
            expose: ToolExposure::Native,
            input_schema: Some(explicit.clone()),
            dispatch: ToolDispatch::Python,
        };
        assert_eq!(spec.native_input_schema(), explicit);
    }
}
