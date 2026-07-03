//! End-to-end WS7.1 test: a fake MCP server speaking JSON-RPC over stdio
//! (python3, same interpreter the REPL tests already require), connected by
//! `McpExtension`, registered through the public extension API, and invoked
//! through the host's watchdogged tool path — exactly what a session does.

#![cfg(feature = "builtins")]

use std::collections::BTreeMap;

use phi_builtins::McpExtension;
use phi_ext::RuntimeBuilder;
use phi_session::config::McpServerConfig;
use serde_json::json;

/// A minimal but conformant stdio MCP server: initialize handshake,
/// paginated tools/list (two pages, exercising the cursor loop), tools/call
/// with success / isError / env-reading tools.
const FAKE_SERVER: &str = r#"
import json, os, sys

def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()

PAGE1 = [{"name": "echo", "description": "Echo text back",
          "inputSchema": {"type": "object",
                          "properties": {"text": {"type": "string"},
                                         "repeat": {"type": "integer"}},
                          "required": ["text"]}}]
PAGE2 = [{"name": "fail", "description": "Always errors",
          "inputSchema": {"type": "object", "properties": {}}},
         {"name": "denied", "description": "Should be filtered out",
          "inputSchema": {"type": "object", "properties": {}}},
         {"name": "getenv", "description": "Read FAKE_SECRET from the environment",
          "inputSchema": {"type": "object", "properties": {}}}]

for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get("method")
    mid = msg.get("id")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": mid, "result": {
            "protocolVersion": msg["params"]["protocolVersion"],
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fake", "version": "0"}}})
    elif method == "tools/list":
        cursor = (msg.get("params") or {}).get("cursor")
        if cursor is None:
            send({"jsonrpc": "2.0", "id": mid,
                  "result": {"tools": PAGE1, "nextCursor": "page2"}})
        else:
            send({"jsonrpc": "2.0", "id": mid, "result": {"tools": PAGE2}})
    elif method == "tools/call":
        name = msg["params"]["name"]
        args = msg["params"].get("arguments", {})
        if name == "echo":
            if "repeat" in args and not isinstance(args["repeat"], int):
                send({"jsonrpc": "2.0", "id": mid,
                      "error": {"code": -32602, "message": "repeat must be an int"}})
                continue
            text = args.get("text", "") * args.get("repeat", 1)
            send({"jsonrpc": "2.0", "id": mid,
                  "result": {"content": [{"type": "text", "text": text}]}})
        elif name == "fail":
            send({"jsonrpc": "2.0", "id": mid,
                  "result": {"isError": True,
                             "content": [{"type": "text", "text": "boom"}]}})
        elif name == "getenv":
            send({"jsonrpc": "2.0", "id": mid,
                  "result": {"content": [{"type": "text",
                                          "text": os.environ.get("FAKE_SECRET", "")}]}})
        else:
            send({"jsonrpc": "2.0", "id": mid,
                  "error": {"code": -32602, "message": "no such tool"}})
    elif mid is not None:
        send({"jsonrpc": "2.0", "id": mid,
              "error": {"code": -32601, "message": "method not supported"}})
"#;

fn fake_server_config(script: &std::path::Path) -> McpServerConfig {
    McpServerConfig {
        command: Some("python3".to_owned()),
        args: Some(vec![script.display().to_string()]),
        env: Some([("FAKE_SECRET".to_owned(), "s3cret".to_owned())].into()),
        disabled_tools: Some(vec!["denied".to_owned()]),
        expose: Some("both".to_owned()),
        timeout: Some(10),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn stdio_server_tools_register_and_invoke() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("fake_mcp.py");
    std::fs::write(&script, FAKE_SERVER).unwrap();

    let mut servers = BTreeMap::new();
    servers.insert("fake".to_owned(), fake_server_config(&script));
    let ext = McpExtension::connect(servers).await;
    // Both pages listed, `denied` filtered by disabled_tools.
    assert_eq!(ext.tool_count(), 3);

    let rt = RuntimeBuilder::new()
        .with_cwd(dir.path())
        .register_extension(ext)
        .build()
        .unwrap();

    // REPL surface: stubs with schema-derived, keyword-safe signatures.
    let stubs = rt.extension_prelude_stubs();
    assert!(
        stubs.contains("def fake_echo(text: str, repeat: int = None)"),
        "{stubs}"
    );

    // Native surface: expose = "both" puts every tool on the provider list.
    let native: Vec<&str> = rt.native_tool_specs().map(|t| t.name.as_str()).collect();
    assert_eq!(native, vec!["fake_echo", "fake_fail", "fake_getenv"]);
    let echo_spec = rt
        .native_tool_specs()
        .find(|t| t.name == "fake_echo")
        .unwrap();
    // The server's schema passes through verbatim for native exposure.
    assert_eq!(echo_spec.native_input_schema()["required"], json!(["text"]));

    // Invoke through the host's watchdogged handler path (what both native
    // dispatch and the REPL stub subcall use).
    let out = rt
        .invoke_tool("fake_echo", json!({"text": "hi", "repeat": 2}))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out.content, "hihi");

    // Nulls (optional params left at None by the stub) are stripped before
    // the wire — the server sees "repeat" as absent, not null.
    let out = rt
        .invoke_tool("fake_echo", json!({"text": "hi", "repeat": null}))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out.content, "hi");

    // isError results surface as errors (stub → RuntimeError, native →
    // failed tool call), and the error text is the content.
    let err = rt.invoke_tool("fake_fail", json!({})).await.unwrap_err();
    assert!(err.to_string().contains("boom"), "{err}");

    // Shell-expanded env reached the child process.
    let out = rt
        .invoke_tool("fake_getenv", json!({}))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out.content, "s3cret");

    // The system prompt documents the repl-exposed roster (stubs are
    // injected silently; without this the model wouldn't know them).
    let prompt = rt
        .filter_system_prompt("q", "BASE".to_owned())
        .await
        .unwrap();
    assert!(prompt.contains("# MCP tools"), "{prompt}");
    assert!(
        prompt.contains("fake_echo(text: str, repeat: int = None)"),
        "{prompt}"
    );
    assert!(prompt.contains("[fake] Echo text back"), "{prompt}");
}

#[tokio::test(flavor = "multi_thread")]
async fn enabled_tools_allowlist_wins() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("fake_mcp.py");
    std::fs::write(&script, FAKE_SERVER).unwrap();

    let mut cfg = fake_server_config(&script);
    cfg.enabled_tools = Some(vec!["echo".to_owned()]);
    let mut servers = BTreeMap::new();
    servers.insert("fake".to_owned(), cfg);
    let ext = McpExtension::connect(servers).await;
    assert_eq!(ext.tool_count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn dead_server_degrades_to_no_tools() {
    let mut servers = BTreeMap::new();
    servers.insert(
        "gone".to_owned(),
        McpServerConfig {
            command: Some("/nonexistent/mcp-server".to_owned()),
            timeout: Some(2),
            ..Default::default()
        },
    );
    // A server that can't spawn costs its tools, never the session.
    let ext = McpExtension::connect(servers).await;
    assert_eq!(ext.tool_count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_server_is_skipped() {
    let mut servers = BTreeMap::new();
    servers.insert(
        "off".to_owned(),
        McpServerConfig {
            command: Some("/nonexistent/mcp-server".to_owned()),
            disabled: Some(true),
            ..Default::default()
        },
    );
    let ext = McpExtension::connect(servers).await;
    assert_eq!(ext.tool_count(), 0);
}
