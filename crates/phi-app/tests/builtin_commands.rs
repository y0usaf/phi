//! WS3.3 acceptance (headless half): the built-in slash commands are
//! embedded Lua registered through the public `phi.register_command` door,
//! and their results follow the frontend contract — `{ message }` to
//! print, `{ action }` string keys to dispatch on.

use phi_host::Host;

const BUILTINS: &str = include_str!("../src/builtins/commands.lua");

fn host_with_builtins() -> Host {
    let host = Host::new().expect("host");
    host.load_source("<builtin:commands>", BUILTINS)
        .expect("load builtins");
    host
}

#[test]
fn builtins_register_through_the_public_door() {
    let host = host_with_builtins();
    let names: Vec<String> = host.commands().into_iter().map(|c| c.name).collect();
    assert_eq!(
        names,
        [
            "help", "quit", "clear", "model", "tree", "fork", "login", "logout"
        ]
    );
    assert!(
        host.commands()
            .iter()
            .all(|c| c.source == "<builtin:commands>")
    );
}

#[test]
fn quit_and_clear_return_bare_actions() {
    let host = host_with_builtins();
    let quit = host.invoke_command("quit", "").expect("invoke").unwrap();
    assert_eq!(quit["action"], "quit");
    let clear = host.invoke_command("clear", "").expect("invoke").unwrap();
    assert_eq!(clear["action"], "clear");
}

#[test]
fn model_and_login_carry_their_arguments() {
    let host = host_with_builtins();
    let model = host
        .invoke_command("model", "claude-opus-4-7")
        .expect("invoke")
        .unwrap();
    assert_eq!(model["action"], "set_model");
    assert_eq!(model["query"], "claude-opus-4-7");
    // Empty args still produce the action; the frontend decides what an
    // empty query means (show the current model).
    let bare = host.invoke_command("model", "").expect("invoke").unwrap();
    assert_eq!(bare["query"], "");

    let login = host
        .invoke_command("login", "openai device")
        .expect("invoke")
        .unwrap();
    assert_eq!(login["action"], "login");
    assert_eq!(login["args"], "openai device");
    let logout = host
        .invoke_command("logout", "anthropic")
        .expect("invoke")
        .unwrap();
    assert_eq!(logout["action"], "logout");
    assert_eq!(logout["args"], "anthropic");
}

#[test]
fn help_lists_builtins_and_later_registrations() {
    let host = host_with_builtins();
    host.load_source(
        "hello.lua",
        r#"phi.register_command({ name = "hello", description = "greet",
            handler = function() return "hi" end })"#,
    )
    .expect("load hello");
    let help = host.invoke_command("help", "").expect("invoke").unwrap();
    let message = help["message"].as_str().expect("message");
    for needle in [
        "/help — list available commands",
        "/quit — quit phi",
        "/model — show or switch the model: /model [id]",
        "/login — provider login: /login anthropic|openai [device]",
        "/hello — greet",
    ] {
        assert!(message.contains(needle), "missing {needle:?} in {message}");
    }
}
