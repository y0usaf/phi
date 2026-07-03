//! WS7.1 — MCP client as a builtin extension, the stress test for
//! `register_tool` (The Rule: it's built through the same public extension
//! API user extensions use, and touches zero core crates).
//!
//! Servers come from the `mcp` table in `config.lua`
//! ([`phi_session::config::McpServerConfig`] documents the shape; project
//! entries are trust-gated by `Config::load_mcp`). Each connected server's
//! tools register as `ToolSpec { dispatch: Handler }`, so everything
//! downstream is the WS9.3a machinery: REPL stubs (`{server}_{tool}` Python
//! callables), optional native exposure per server (`expose = "native" |
//! "both"`), the host's 60s tool watchdog, and ToolCall/ToolResult
//! interception. A `BeforeAgentStart` hook appends an `# MCP tools` roster
//! to the system prompt so the model knows the REPL functions exist.

mod client;
mod expand;

pub use client::{McpClient, McpTool, TransportConfig};
pub use expand::shell_expand;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use phi_ext::{
    EventHandlerRegistration, EventKind, EventReturn, Extension, ExtensionHost, ExtensionManifest,
    ToolDispatch, ToolExposure, ToolInvocation, ToolOutput, ToolParam, ToolSpec, python_param_name,
    python_param_names,
};
use phi_session::config::McpServerConfig;
use serde_json::Value;

/// Seconds allowed per server for connect + handshake + tools/list when the
/// config doesn't say (crush's default).
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 15;

/// Per-call budget, kept under the host's 60s tool watchdog so the inner
/// timeout fires first and the blocking thread is released instead of
/// leaking behind a detached watchdog.
const CALL_TIMEOUT: Duration = Duration::from_secs(55);

/// The builtin MCP extension. [`McpExtension::connect`] dials every enabled
/// server in parallel (bounded by each server's `timeout`); failures become
/// stderr warnings, never build errors — a dead server costs its tools, not
/// the session. Server connections live inside the tool-handler closures,
/// so a stdio child process dies when the runtime (and thus the last
/// handler) is dropped.
pub struct McpExtension {
    tools: Vec<ToolSpec>,
    prompt_block: Option<String>,
}

impl McpExtension {
    pub async fn connect(servers: BTreeMap<String, McpServerConfig>) -> Self {
        let handle = tokio::runtime::Handle::current();
        let mut joins = Vec::new();
        for (name, cfg) in servers {
            if cfg.disabled.unwrap_or(false) {
                continue;
            }
            let handle = handle.clone();
            joins.push(tokio::spawn(async move {
                match connect_server(&name, &cfg, handle).await {
                    Ok(tools) => tools,
                    Err(e) => {
                        eprintln!("warning: mcp server '{name}': {e:#}");
                        Vec::new()
                    }
                }
            }));
        }
        let mut tools = Vec::new();
        for join in joins {
            if let Ok(specs) = join.await {
                tools.extend(specs);
            }
        }
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        let prompt_block = prompt_block(&tools);
        Self {
            tools,
            prompt_block,
        }
    }

    /// Registered tool count (all servers), mostly for tests/diagnostics.
    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }
}

impl Extension for McpExtension {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: "phi:mcp".to_owned(),
            name: "Phi MCP".to_owned(),
            version: "0.1.0".to_owned(),
            description: "MCP client: config-declared servers' tools via register_tool".to_owned(),
        }
    }

    fn register(&self, host: &mut dyn ExtensionHost) -> Result<()> {
        for tool in &self.tools {
            // A name collision (two servers, or a clash with another
            // extension's tool) costs that tool, not the session.
            if let Err(e) = host.register_tool(tool.clone()) {
                eprintln!("warning: mcp: {e}");
            }
        }
        if let Some(block) = &self.prompt_block {
            let block = block.clone();
            host.subscribe(EventHandlerRegistration {
                timeout: None,
                event: EventKind::BeforeAgentStart,
                label: "phi:mcp".to_owned(),
                handler: Arc::new(move |_event| {
                    Ok(Some(EventReturn::SystemPromptMod {
                        append: Some(block.clone()),
                        prepend: None,
                        replace: None,
                    }))
                }),
            })?;
        }
        Ok(())
    }
}

async fn connect_server(
    server: &str,
    cfg: &McpServerConfig,
    handle: tokio::runtime::Handle,
) -> Result<Vec<ToolSpec>> {
    let expose = match cfg.expose.as_deref() {
        None | Some("repl") => ToolExposure::Repl,
        Some("native") => ToolExposure::Native,
        Some("both") => ToolExposure::Both,
        Some(other) => {
            eprintln!("warning: mcp server '{server}': unknown expose '{other}', using 'repl'");
            ToolExposure::Repl
        }
    };
    // Shell expansion runs commands ($(...)); keep it off the async threads.
    let transport = {
        let server = server.to_owned();
        let cfg = cfg.clone();
        tokio::task::spawn_blocking(move || transport_config(&server, &cfg))
            .await
            .context("transport resolution")??
    };
    let timeout = Duration::from_secs(cfg.timeout.unwrap_or(DEFAULT_CONNECT_TIMEOUT_SECS));
    let (client, mcp_tools) = McpClient::connect(server, transport, timeout).await?;

    let enabled = cfg.enabled_tools.clone().unwrap_or_default();
    let disabled = cfg.disabled_tools.clone().unwrap_or_default();
    Ok(mcp_tools
        .into_iter()
        .filter(|t| enabled.is_empty() || enabled.contains(&t.name))
        .filter(|t| !disabled.contains(&t.name))
        .map(|t| tool_spec(server, t, expose, client.clone(), handle.clone()))
        .collect())
}

/// Resolve one server's config into transport parameters: pick the
/// transport (explicit `type` wins, else inferred from which of
/// `command`/`url` is set), shell-expand `env`/`headers`, and drop headers
/// that expanded to nothing.
fn transport_config(server: &str, cfg: &McpServerConfig) -> Result<TransportConfig> {
    let inferred = if cfg.command.is_some() {
        "stdio"
    } else {
        "http"
    };
    match cfg.transport.as_deref().unwrap_or(inferred) {
        "stdio" => {
            let command = cfg
                .command
                .clone()
                .ok_or_else(|| anyhow!("server '{server}': stdio transport needs `command`"))?;
            let env = cfg
                .env
                .iter()
                .flatten()
                .map(|(k, v)| (k.clone(), shell_expand(v)))
                .collect();
            Ok(TransportConfig::Stdio {
                command,
                args: cfg.args.clone().unwrap_or_default(),
                env,
            })
        }
        "http" => {
            let url = cfg
                .url
                .clone()
                .ok_or_else(|| anyhow!("server '{server}': http transport needs `url`"))?;
            let headers = cfg
                .headers
                .iter()
                .flatten()
                .filter_map(|(k, v)| {
                    let v = shell_expand(v);
                    // Empty after expansion (unset $VAR, failed $(cmd), or
                    // literal "") → omit rather than send a blank header.
                    (!v.is_empty()).then(|| (k.clone(), v))
                })
                .collect();
            Ok(TransportConfig::Http { url, headers })
        }
        "sse" => bail!(
            "server '{server}': the legacy `sse` transport is not supported yet — \
             modern servers speak `http` (streamable HTTP)"
        ),
        other => bail!("server '{server}': unknown transport '{other}' (stdio|http|sse)"),
    }
}

/// Build the `ToolSpec` for one MCP tool: `{server}_{tool}` sanitized into a
/// Python identifier, params derived from the server's input schema (for the
/// REPL stub signature), the schema itself passed through for native
/// exposure, and a handler that blocks on the async client from the
/// blocking-pool thread the host runs handlers on.
fn tool_spec(
    server: &str,
    tool: McpTool,
    expose: ToolExposure,
    client: Arc<McpClient>,
    handle: tokio::runtime::Handle,
) -> ToolSpec {
    let name = format!(
        "{}_{}",
        python_param_name(server),
        python_param_name(&tool.name)
    );
    let description = if tool.description.trim().is_empty() {
        format!("MCP tool '{}' from server '{server}'", tool.name)
    } else {
        format!("[{server}] {}", tool.description.trim())
    };
    let params = params_from_schema(&tool.input_schema);
    let remote = tool.name;
    let handler = Arc::new(move |inv: ToolInvocation| {
        // Both dispatch paths (REPL stub subcall, native tool call) deliver
        // a JSON object of args. Optional params the caller left at None
        // arrive as nulls — strip them; MCP servers validate against their
        // schema and null is not "absent".
        let args = match serde_json::from_str::<Value>(&inv.input) {
            Ok(Value::Object(map)) => map.into_iter().filter(|(_, v)| !v.is_null()).collect(),
            _ => serde_json::Map::new(),
        };
        let client = client.clone();
        let remote = remote.clone();
        // Handlers run on the blocking pool (never an async worker), so
        // driving the async client with block_on is safe.
        handle
            .block_on(async move { client.call_tool(&remote, args, CALL_TIMEOUT).await })
            .map(|content| ToolOutput { content })
    });
    ToolSpec {
        name,
        description,
        params,
        expose,
        input_schema: Some(tool.input_schema),
        dispatch: ToolDispatch::Handler(handler),
    }
}

/// Derive REPL-stub params from a JSON Schema: property names keep their
/// wire form (`python_param_names` sanitizes them at stub-generation time),
/// JSON types map to the loose Python-ish vocabulary `ToolParam` uses, and
/// non-required params default to `None` (the handler strips the nulls).
/// Required params come first — Python forbids non-default after default.
fn params_from_schema(schema: &Value) -> Vec<ToolParam> {
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut required_params = Vec::new();
    let mut optional_params = Vec::new();
    for (name, prop) in props {
        let ty = match prop.get("type").and_then(Value::as_str) {
            Some("integer") => "int",
            Some("number") => "float",
            Some("boolean") => "bool",
            Some("array") => "list",
            Some("object") => "dict",
            _ => "str",
        };
        if required.contains(&name.as_str()) {
            required_params.push(ToolParam {
                name: name.clone(),
                ty: ty.to_owned(),
                default: None,
            });
        } else {
            optional_params.push(ToolParam {
                name: name.clone(),
                ty: ty.to_owned(),
                default: Some("None".to_owned()),
            });
        }
    }
    required_params.extend(optional_params);
    required_params
}

/// System-prompt roster for repl-exposed MCP tools. The stubs land in the
/// REPL env silently (setup code is never shown to the model), so without
/// this block the functions would be reachable but unknown. Native-exposed
/// tools skip it — the provider tool definitions already carry them.
fn prompt_block(tools: &[ToolSpec]) -> Option<String> {
    let mut lines = Vec::new();
    for t in tools.iter().filter(|t| t.expose.includes_repl()) {
        let py_names = python_param_names(&t.params);
        let sig: Vec<String> = t
            .params
            .iter()
            .zip(&py_names)
            .map(|(p, py)| match &p.default {
                Some(def) => format!("{}: {} = {}", py, p.ty, def),
                None => format!("{}: {}", py, p.ty),
            })
            .collect();
        lines.push(format!(
            "- {}({}) -> str — {}",
            t.name,
            sig.join(", "),
            t.description
        ));
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "# MCP tools\n\n\
         These Python functions are provided by connected MCP servers and are \
         available in the REPL alongside the built-in helpers. They block until \
         the server responds and raise RuntimeError on server-side failures:\n\n{}",
        lines.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn params_from_schema_orders_required_first_and_maps_types() {
        let schema = json!({
            "type": "object",
            "properties": {
                "aaa_optional": {"type": "integer"},
                "query": {"type": "string"},
                "deep": {"type": "object"},
                "count": {"type": "number"},
                "flags": {"type": "array"},
                "on": {"type": "boolean"},
            },
            "required": ["query", "on"],
        });
        let params = params_from_schema(&schema);
        let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
        // Required first (schema-property order within each group is the
        // serde_json BTreeMap order, i.e. sorted).
        assert_eq!(
            names,
            vec!["on", "query", "aaa_optional", "count", "deep", "flags"]
        );
        assert!(params[0].default.is_none() && params[1].default.is_none());
        assert!(
            params[2..]
                .iter()
                .all(|p| p.default.as_deref() == Some("None"))
        );
        let ty = |n: &str| {
            params
                .iter()
                .find(|p| p.name == n)
                .map(|p| p.ty.clone())
                .unwrap()
        };
        assert_eq!(ty("query"), "str");
        assert_eq!(ty("aaa_optional"), "int");
        assert_eq!(ty("count"), "float");
        assert_eq!(ty("on"), "bool");
        assert_eq!(ty("flags"), "list");
        assert_eq!(ty("deep"), "dict");
    }

    #[test]
    fn params_from_schema_handles_missing_properties() {
        assert!(params_from_schema(&json!({})).is_empty());
        assert!(params_from_schema(&json!({"type": "object"})).is_empty());
    }

    #[test]
    fn transport_config_infers_and_validates() {
        let stdio = McpServerConfig {
            command: Some("npx".to_owned()),
            args: Some(vec!["-y".to_owned(), "server".to_owned()]),
            ..Default::default()
        };
        assert!(matches!(
            transport_config("s", &stdio),
            Ok(TransportConfig::Stdio { command, args, .. })
                if command == "npx" && args.len() == 2
        ));

        let http = McpServerConfig {
            url: Some("https://x/mcp".to_owned()),
            ..Default::default()
        };
        assert!(matches!(
            transport_config("s", &http),
            Ok(TransportConfig::Http { url, .. }) if url == "https://x/mcp"
        ));

        // Explicit type wins over inference; missing fields are errors.
        let bad = McpServerConfig {
            transport: Some("stdio".to_owned()),
            url: Some("https://x".to_owned()),
            ..Default::default()
        };
        assert!(transport_config("s", &bad).is_err());
        assert!(transport_config("s", &McpServerConfig::default()).is_err());

        let sse = McpServerConfig {
            transport: Some("sse".to_owned()),
            url: Some("https://x/sse".to_owned()),
            ..Default::default()
        };
        assert!(
            transport_config("s", &sse)
                .unwrap_err()
                .to_string()
                .contains("sse")
        );
    }

    #[test]
    fn empty_expanded_headers_are_omitted() {
        let cfg = McpServerConfig {
            url: Some("https://x/mcp".to_owned()),
            headers: Some(
                [
                    (
                        "Authorization".to_owned(),
                        "$PHI_MCP_UNSET_VAR_T".to_owned(),
                    ),
                    ("X-Static".to_owned(), "yes".to_owned()),
                ]
                .into(),
            ),
            ..Default::default()
        };
        let Ok(TransportConfig::Http { headers, .. }) = transport_config("s", &cfg) else {
            panic!("expected http transport");
        };
        assert_eq!(headers, vec![("X-Static".to_owned(), "yes".to_owned())]);
    }
}
