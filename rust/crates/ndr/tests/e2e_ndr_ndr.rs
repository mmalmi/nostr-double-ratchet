//! E2E test: Two ndr CLI instances establishing a session and messaging
//!
//! This test verifies that two ndr instances (with different data directories)
//! can establish a session via invite/accept flow and exchange messages through
//! a relay.

mod common;

use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// Run ndr CLI command and return JSON output
async fn run_ndr(data_dir: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let output = Command::new("cargo")
        .args(["run", "-q", "-p", "ndr", "--"])
        .arg("--json")
        .arg("--data-dir")
        .arg(data_dir)
        .args(args)
        .output()
        .await
        .expect("Failed to run ndr");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        panic!("ndr failed: stdout={} stderr={}", stdout, stderr);
    }

    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("Failed to parse ndr output: {}\nOutput: {}", e, stdout)
    })
}

/// Start ndr listen in background
async fn start_ndr_listen(data_dir: &std::path::Path, chat_id: Option<&str>) -> (Child, BufReader<tokio::process::ChildStdout>) {
    let mut cmd = Command::new("cargo");
    cmd.args(["run", "-q", "-p", "ndr", "--"])
        .arg("--json")
        .arg("--data-dir")
        .arg(data_dir)
        .arg("listen");

    if let Some(id) = chat_id {
        cmd.arg("--chat").arg(id);
    }

    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("Failed to start ndr listen");

    let stdout = BufReader::new(child.stdout.take().expect("Failed to capture stdout"));
    (child, stdout)
}

/// Setup ndr data directory with config for test relay
fn setup_ndr_dir(relay_url: &str, secret_key: &str) -> TempDir {
    let dir = TempDir::new().unwrap();

    // Write config with relay
    let config_content = serde_json::json!({
        "relays": [relay_url]
    });
    std::fs::write(
        dir.path().join("config.json"),
        serde_json::to_string(&config_content).unwrap()
    ).unwrap();

    // Login synchronously would be ideal but we'll do it in the test
    dir
}

#[tokio::test]
async fn test_ndr_to_ndr_session() {
    // Start WebSocket relay
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);
    println!("Relay started at: {}", relay_url);

    // Setup Alice (creates invite)
    let alice_dir = setup_ndr_dir(&relay_url, "");
    let alice_sk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    // Setup Bob (joins via invite)
    let bob_dir = setup_ndr_dir(&relay_url, "");
    let bob_sk = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    // Login both
    let result = run_ndr(alice_dir.path(), &["login", alice_sk]).await;
    assert_eq!(result["status"], "ok", "Alice login failed");

    let result = run_ndr(bob_dir.path(), &["login", bob_sk]).await;
    assert_eq!(result["status"], "ok", "Bob login failed");

    // Alice creates an invite
    let result = run_ndr(alice_dir.path(), &["invite", "create", "-l", "alice-invite"]).await;
    assert_eq!(result["status"], "ok", "Alice invite create failed");
    let invite_url = result["data"]["url"].as_str().unwrap().to_string();
    let invite_id = result["data"]["id"].as_str().unwrap().to_string();
    println!("Alice created invite: {} (id: {})", invite_url, invite_id);

    // Start Alice's ndr listen to receive the invite response
    let (mut alice_listen_child, mut alice_listen_reader) = start_ndr_listen(alice_dir.path(), None).await;

    // Give Alice's listener time to connect
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Bob joins via the invite URL
    let result = run_ndr(bob_dir.path(), &["chat", "join", &invite_url]).await;
    assert_eq!(result["status"], "ok", "Bob join failed");
    let bob_chat_id = result["data"]["id"].as_str().unwrap().to_string();
    let response_event = result["data"]["response_event"].as_str().unwrap().to_string();
    println!("Bob joined chat: {}", bob_chat_id);

    // Bob needs to publish the response event to the relay
    let (mut ws, _) = connect_async(&relay_url).await.expect("Failed to connect to relay");
    let event_msg = format!(r#"["EVENT",{}]"#, response_event);
    ws.send(Message::Text(event_msg)).await.expect("Failed to send response event");

    // Wait for relay OK
    if let Some(Ok(Message::Text(response))) = ws.next().await {
        println!("Relay response to Bob's response event: {}", response);
    }

    // Wait for Alice's listener to receive the session_created event
    let mut alice_chat_id = None;
    let timeout_instant = std::time::Instant::now();
    while timeout_instant.elapsed() < Duration::from_secs(10) {
        let mut line = String::new();
        let read_result = tokio::time::timeout(
            Duration::from_millis(100),
            alice_listen_reader.read_line(&mut line)
        ).await;

        if let Ok(Ok(n)) = read_result {
            if n > 0 {
                let trimmed = line.trim();
                println!("[Alice listen] {}", trimmed);
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if json["event"] == "session_created" {
                        alice_chat_id = json["chat_id"].as_str().map(|s| s.to_string());
                        println!("Alice received invite response! Chat ID: {:?}", alice_chat_id);
                        break;
                    }
                }
            }
        }
    }

    // Kill Alice's first listener
    let _ = alice_listen_child.kill().await;

    assert!(alice_chat_id.is_some(), "Alice did not receive Bob's invite response");
    let alice_chat_id = alice_chat_id.unwrap();

    // Now test bidirectional messaging
    println!("\n--- Testing Bob -> Alice message ---");

    // Start Alice's listener for messages
    let (mut alice_listen_child, mut alice_listen_reader) = start_ndr_listen(alice_dir.path(), Some(&alice_chat_id)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Bob sends a message
    let result = run_ndr(bob_dir.path(), &["send", &bob_chat_id, "Hello from Bob!"]).await;
    assert_eq!(result["status"], "ok", "Bob send failed");
    let bob_msg_event = result["data"]["event"].as_str().unwrap().to_string();

    // Publish Bob's message to relay
    let event_msg = format!(r#"["EVENT",{}]"#, bob_msg_event);
    ws.send(Message::Text(event_msg)).await.expect("Failed to send message event");

    // Wait for Alice to receive the message
    let mut alice_received = false;
    let timeout_instant = std::time::Instant::now();
    while timeout_instant.elapsed() < Duration::from_secs(10) {
        let mut line = String::new();
        let read_result = tokio::time::timeout(
            Duration::from_millis(100),
            alice_listen_reader.read_line(&mut line)
        ).await;

        if let Ok(Ok(n)) = read_result {
            if n > 0 {
                let trimmed = line.trim();
                println!("[Alice listen] {}", trimmed);
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if json["event"] == "message" && json["content"] == "Hello from Bob!" {
                        println!("Alice received Bob's message!");
                        alice_received = true;
                        break;
                    }
                }
            }
        }
    }

    let _ = alice_listen_child.kill().await;
    assert!(alice_received, "Alice did not receive Bob's message");

    println!("\n--- Testing Alice -> Bob message ---");

    // Start Bob's listener
    let (mut bob_listen_child, mut bob_listen_reader) = start_ndr_listen(bob_dir.path(), Some(&bob_chat_id)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Alice sends a message
    let result = run_ndr(alice_dir.path(), &["send", &alice_chat_id, "Hello from Alice!"]).await;
    assert_eq!(result["status"], "ok", "Alice send failed");
    let alice_msg_event = result["data"]["event"].as_str().unwrap().to_string();

    // Publish Alice's message to relay
    let event_msg = format!(r#"["EVENT",{}]"#, alice_msg_event);
    ws.send(Message::Text(event_msg)).await.expect("Failed to send message event");

    // Wait for Bob to receive the message
    let mut bob_received = false;
    let timeout_instant = std::time::Instant::now();
    while timeout_instant.elapsed() < Duration::from_secs(10) {
        let mut line = String::new();
        let read_result = tokio::time::timeout(
            Duration::from_millis(100),
            bob_listen_reader.read_line(&mut line)
        ).await;

        if let Ok(Ok(n)) = read_result {
            if n > 0 {
                let trimmed = line.trim();
                println!("[Bob listen] {}", trimmed);
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if json["event"] == "message" && json["content"] == "Hello from Alice!" {
                        println!("Bob received Alice's message!");
                        bob_received = true;
                        break;
                    }
                }
            }
        }
    }

    // Cleanup
    let _ = bob_listen_child.kill().await;
    ws.close(None).await.ok();
    relay.stop().await;

    assert!(bob_received, "Bob did not receive Alice's message");

    println!("\n=== E2E test passed: ndr <-> ndr bidirectional messaging works! ===");
}
