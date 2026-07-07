//! WS1 acceptance (DESIGN.md): hello.lua registers a command and an event
//! handler; a hung handler is killed by the watchdog; project-local
//! extensions are gated on trust; the VM survives every failure mode.

use std::path::PathBuf;

use phi_host::{Host, HostConfig, TrustDecision, TrustStore};
use serde_json::json;

fn hello_lua() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/hello.lua")
}

fn fast_host() -> Host {
    Host::with_config(HostConfig {
        dispatch_timeout_ms: 250,
    })
    .expect("host")
}

#[test]
fn hello_example_registers_command_and_handler() {
    let host = Host::new().expect("host");
    host.load_file(&hello_lua()).expect("load hello.lua");

    let commands = host.commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].name, "hello");
    assert_eq!(
        commands[0].description.as_deref(),
        Some("reply with a greeting")
    );
    assert!(commands[0].source.ends_with("hello.lua"));

    let result = host.invoke_command("hello", "world").expect("invoke");
    assert_eq!(result, Some(json!({ "message": "hello, world!" })));

    assert_eq!(host.subscriber_count("session_start"), 1);
    let outcomes = host
        .emit(
            "session_start",
            &json!({ "reason": "startup", "cwd": "/tmp" }),
        )
        .expect("emit");
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].result, Ok(None));
}

#[test]
fn unknown_command_is_an_error() {
    let host = Host::new().expect("host");
    let err = host.invoke_command("nope", "").unwrap_err();
    assert!(err.to_string().contains("unknown command 'nope'"), "{err}");
}

#[test]
fn duplicate_command_registration_fails_and_names_prior_source() {
    let host = Host::new().expect("host");
    host.load_source(
        "first.lua",
        "phi.register_command({ name = 'x', handler = function() end })",
    )
    .expect("first");
    let err = host
        .load_source(
            "second.lua",
            "phi.register_command({ name = 'x', handler = function() end })",
        )
        .unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("already registered"), "{text}");
    assert!(text.contains("first.lua"), "{text}");
    // Only the first registration survives.
    assert_eq!(host.commands().len(), 1);
}

#[test]
fn hung_handler_is_killed_and_later_handlers_still_run() {
    let host = fast_host();
    host.load_source(
        "spin.lua",
        r#"
        phi.on("spin", function() while true do end end)
        phi.on("spin", function() return { ok = true } end)
        "#,
    )
    .expect("load");

    let outcomes = host.emit("spin", &json!({})).expect("emit");
    assert_eq!(outcomes.len(), 2);
    let err = outcomes[0].result.as_ref().unwrap_err();
    assert!(err.contains("timed out"), "{err}");
    assert_eq!(outcomes[1].result, Ok(Some(json!({ "ok": true }))));

    // The VM survives the kill.
    let again = host.emit("spin", &json!({})).expect("emit again");
    assert_eq!(again.len(), 2);
}

#[test]
fn hung_top_level_chunk_is_killed() {
    let host = fast_host();
    let err = host
        .load_source("busy.lua", "while true do end")
        .unwrap_err();
    assert!(format!("{err:#}").contains("timed out"), "{err:#}");
    // Host still usable afterwards.
    host.load_source("ok.lua", "phi.on('e', function() end)")
        .expect("load after kill");
    assert_eq!(host.subscriber_count("e"), 1);
}

#[test]
fn handler_errors_are_isolated_per_handler() {
    let host = Host::new().expect("host");
    host.load_source(
        "mixed.lua",
        r#"
        phi.on("e", function() error("boom") end)
        phi.on("e", function(ev) return { echo = ev.n + 1 } end)
        "#,
    )
    .expect("load");

    let outcomes = host.emit("e", &json!({ "n": 41 })).expect("emit");
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes[0].result.as_ref().unwrap_err().contains("boom"));
    assert_eq!(outcomes[1].result, Ok(Some(json!({ "echo": 42 }))));
}

#[test]
fn payload_and_result_round_trip_nested_tables() {
    let host = Host::new().expect("host");
    host.load_source(
        "echo.lua",
        r#"
        phi.on("echo", function(ev)
          return { got = ev, list = { 1, "two", true }, nested = { a = { b = "c" } } }
        end)
        "#,
    )
    .expect("load");

    let payload = json!({ "s": "str", "n": 7, "f": 1.5, "b": false, "arr": [1, 2, 3] });
    let outcomes = host.emit("echo", &payload).expect("emit");
    let value = outcomes[0].result.as_ref().unwrap().clone().unwrap();
    assert_eq!(value["got"], payload);
    assert_eq!(value["list"], json!([1, "two", true]));
    assert_eq!(value["nested"], json!({ "a": { "b": "c" } }));
}

#[test]
fn bare_string_command_result_is_message_shorthand() {
    let host = Host::new().expect("host");
    host.load_source(
        "s.lua",
        "phi.register_command({ name = 's', handler = function() return 'plain' end })",
    )
    .expect("load");
    let result = host.invoke_command("s", "").expect("invoke");
    assert_eq!(result, Some(json!({ "message": "plain" })));
}

#[test]
fn syntax_error_reports_file_and_vm_survives() {
    let host = Host::new().expect("host");
    let err = host
        .load_source("broken.lua", "this is not lua")
        .unwrap_err();
    assert!(format!("{err:#}").contains("broken.lua"), "{err:#}");
    host.load_source("fine.lua", "phi.on('e', function() end)")
        .expect("load after error");
}

#[test]
fn emit_without_subscribers_is_empty() {
    let host = Host::new().expect("host");
    let outcomes = host.emit("nobody", &json!({})).expect("emit");
    assert!(outcomes.is_empty());
}

#[test]
fn discovery_gates_project_extensions_on_trust() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let cwd = tmp.path();
    let global = cwd.join("global-ext");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::write(global.join("b.lua"), "-- global b").unwrap();
    std::fs::write(global.join("a.lua"), "-- global a").unwrap();
    let project = cwd.join(".phi/extensions");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("p.lua"), "-- project").unwrap();
    std::fs::write(project.join("notes.txt"), "not lua").unwrap();

    assert!(phi_host::project_extensions_present(cwd));

    let untrusted = phi_host::discover_extensions(Some(&global), cwd, false);
    assert_eq!(untrusted.len(), 2, "{untrusted:?}");
    // Global dir sorted by filename; no project files.
    assert!(untrusted[0].ends_with("a.lua") && untrusted[1].ends_with("b.lua"));

    let trusted = phi_host::discover_extensions(Some(&global), cwd, true);
    assert_eq!(trusted.len(), 3, "{trusted:?}");
    assert!(trusted[2].ends_with("p.lua"));
}

#[test]
fn trust_store_persists_decisions() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let store_path = tmp.path().join("nested/trust.json");
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();

    let mut store = TrustStore::open(store_path.clone());
    assert_eq!(store.decision(&project), TrustDecision::Undecided);
    store.set(&project, true).expect("persist");
    assert_eq!(store.decision(&project), TrustDecision::Trusted);

    // A fresh handle reads the persisted decision.
    let reopened = TrustStore::open(store_path.clone());
    assert_eq!(reopened.decision(&project), TrustDecision::Trusted);

    let mut store = TrustStore::open(store_path);
    store.set(&project, false).expect("persist deny");
    assert_eq!(store.decision(&project), TrustDecision::Denied);
}

#[test]
fn registration_from_event_handler_attributes_to_extension() {
    let host = Host::new().expect("host");
    host.load_source(
        "late.lua",
        r#"
        phi.on("boot", function()
          phi.register_command({ name = "late", handler = function() return "hi" end })
        end)
        "#,
    )
    .expect("load");
    host.emit("boot", &json!({})).expect("emit");
    let commands = host.commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].source, "late.lua");
}

#[test]
fn commands_snapshot_is_readable_from_lua() {
    let host = Host::new().expect("host");
    host.load_file(&hello_lua()).expect("load hello.lua");
    host.load_file(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/list-commands.lua"),
    )
    .expect("load list-commands.lua");

    let result = host
        .invoke_command("commands", "")
        .expect("invoke")
        .expect("result");
    let message = result["message"].as_str().expect("message string");
    let lines: Vec<&str> = message.lines().collect();
    // Registration order: hello.lua loaded first, then list-commands.lua.
    assert!(lines[0].starts_with("/hello"));
    assert!(lines[0].ends_with("hello.lua)"));
    assert!(lines[1].starts_with("/commands"));
    assert!(lines[1].ends_with("list-commands.lua)"));
}
