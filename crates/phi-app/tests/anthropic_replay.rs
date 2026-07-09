#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Headless WS4.6 acceptance: the real Anthropic protocol is replayed through
//! the public Lua agent, including a tool round trip and JSONL persistence.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use phi_app::builtins::{CODING_AGENT_PACK, TOOLS_PACK};
use phi_host::{Host, HostConfig};

fn read_request(stream: &mut TcpStream) {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let count = stream.read(&mut chunk).unwrap();
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                break;
            }
        }
    }
}

fn response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
}

#[test]
fn anthropic_replay_runs_lua_agent_tool_round_trip_and_persists_messages() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(0usize));
    let seen = Arc::clone(&requests);
    let first = include_str!("../../phi-ai/tests/fixtures/anthropic/replay_basic.sse").to_owned();
    let second = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_02\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-opus-4-7\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n"
    );
    let server = thread::spawn(move || {
        for body in [&first, second] {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            *seen.lock().unwrap() += 1;
            stream.write_all(response(body).as_bytes()).unwrap();
        }
    });

    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().to_string_lossy().into_owned();
    let agent_dir = temp.path().join("agent");
    let session =
        phi_session::SessionManager::create(&cwd, None, &agent_dir.to_string_lossy(), None)
            .unwrap();
    let session_path = session.get_session_file().unwrap().to_owned();
    let model = serde_json::json!({
        "id": "claude-opus-4-7", "name": "Claude Opus 4.7",
        "api": "anthropic-messages", "provider": "anthropic",
        "baseUrl": format!("http://{}", address), "reasoning": true,
        "input": ["text"], "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
        "contextWindow": 100000, "maxTokens": 1024
    });
    let host = Host::new(HostConfig {
        cwd: Some(cwd),
        ..HostConfig::default()
    })
    .unwrap();
    let report = host.load_embedded(&[phi_agent::PACK, TOOLS_PACK, CODING_AGENT_PACK]);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let result = host.call_command("phi-run", &serde_json::json!({
        "model": model, "apiKey": "test-key", "prompt": "read the file", "sessionPath": session_path
    }).to_string()).unwrap().unwrap();
    server.join().unwrap();

    assert_eq!(result["text"], "done");
    assert_eq!(*requests.lock().unwrap(), 2);
    let session_text = std::fs::read_to_string(result["sessionPath"].as_str().unwrap()).unwrap();
    let entries: Vec<serde_json::Value> = session_text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let messages: Vec<&serde_json::Value> = entries
        .iter()
        .filter_map(|entry| entry.get("message"))
        .collect();
    assert!(messages.iter().any(|message| message["role"] == "user"));
    assert!(
        messages
            .iter()
            .any(|message| message["role"] == "toolResult")
    );
    assert!(
        messages
            .iter()
            .any(|message| message["role"] == "assistant")
    );
}
