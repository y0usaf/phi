//! Integration tests for the phi server/client protocol over a real Unix socket.
//!
//! Mirrors ref/jcode's `server/tests.rs` socket round-trip tests. Each test
//! binds a server on a unique temporary socket, spawns the accept loop, and
//! exercises the Client API.

use std::path::PathBuf;

use phi_protocol::{cleanup_socket_pair, socket_has_live_listener};
use phi_server::{Client, Server};

/// Unique socket paths for each test (avoids parallel-run collisions).
fn test_socket(name: &str) -> (PathBuf, PathBuf) {
    let mut path = std::env::temp_dir();
    path.push(format!("phi-test-{name}-{}.sock", std::process::id()));
    cleanup_socket_pair(&path);
    let debug = path.with_file_name(
        path.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .replace(".sock", "-debug.sock"),
    );
    (path, debug)
}

/// Bind a server on a temporary socket and spawn its accept loop.
async fn spawn_server(name: &str) -> (PathBuf, PathBuf) {
    let (sock, debug) = test_socket(name);
    let server = Server::new_with_paths(sock.clone(), debug.clone());
    let sock_clone = sock.clone();
    tokio::spawn(async move {
        let _ = server.run().await;
    });
    // Wait until the socket has a live listener.
    for _ in 0..100 {
        if socket_has_live_listener(&sock_clone).await {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    (sock, debug)
}

#[tokio::test]
async fn ping_pong_round_trip() {
    let (sock, _debug) = spawn_server("ping").await;
    let mut client = Client::connect_with_path(sock).await.unwrap();
    assert!(client.ping().await.unwrap());
}

#[tokio::test]
async fn get_state_returns_snapshot() {
    let (sock, _debug) = spawn_server("state").await;
    let mut client = Client::connect_with_path(sock).await.unwrap();
    let event = client.get_state().await.unwrap();
    match event {
        phi_protocol::ServerEvent::State {
            is_processing,
            session_id,
            client_count,
        } => {
            assert!(!is_processing);
            assert!(session_id.is_empty());
            assert!(client_count >= 1);
        }
        _ => panic!("expected State event, got {event:?}"),
    }
}

#[tokio::test]
async fn subscribe_returns_ack() {
    let (sock, _debug) = spawn_server("subscribe").await;
    let mut client = Client::connect_with_path(sock).await.unwrap();
    let id = client.subscribe(Some("/tmp".to_string()), None).await.unwrap();
    let event = client.read_event().await.unwrap();
    assert_eq!(event, phi_protocol::ServerEvent::Ack { id });
}

#[tokio::test]
async fn clear_returns_no_error() {
    let (sock, _debug) = spawn_server("clear").await;
    let mut client = Client::connect_with_path(sock).await.unwrap();
    client.clear().await.unwrap();
}

#[tokio::test]
async fn malformed_request_returns_error_with_peeked_id() {
    let (sock, _debug) = spawn_server("malformed").await;
    let mut client = Client::connect_with_path(sock).await.unwrap();

    // Send a request with an unknown type but a valid id.
    // The server should peek the id and return it in the error.
    let line = r#"{"type":"bogus","id":77}"#;
    client.send_raw(line).await.unwrap();

    let event = client.read_event().await.unwrap();
    match event {
        phi_protocol::ServerEvent::Error { id, .. } => {
            assert_eq!(id, 77, "error should carry the peeked request id");
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_lock_prevents_double_bind() {
    use phi_protocol::{DaemonLock, daemon_lock_path_for};
    use std::path::PathBuf;

    let lock_path: PathBuf = std::env::temp_dir().join(format!(
        "phi-test-lock-{}.lock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&lock_path);

    // First acquisition succeeds.
    let _lock1 = DaemonLock::acquire(&lock_path).unwrap();

    // Second acquisition on the same path must fail.
    let result = DaemonLock::acquire(&lock_path);
    assert!(result.is_err(), "second daemon lock should be rejected");

    let _ = std::fs::remove_file(&lock_path);
}
