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
fn setup_ndr_dir(relay_url: &str, _secret_key: &str) -> TempDir {
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

    // Bob joins via the invite URL (ndr chat join now publishes response to relay automatically)
    let result = run_ndr(bob_dir.path(), &["chat", "join", &invite_url]).await;
    assert_eq!(result["status"], "ok", "Bob join failed");
    let bob_chat_id = result["data"]["id"].as_str().unwrap().to_string();
    println!("Bob joined chat: {}", bob_chat_id);

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

    // Bob sends a message (ndr send now publishes to relay automatically)
    let result = run_ndr(bob_dir.path(), &["send", &bob_chat_id, "Hello from Bob!"]).await;
    assert_eq!(result["status"], "ok", "Bob send failed");

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

    // Alice sends a message (ndr send now publishes to relay automatically)
    let result = run_ndr(alice_dir.path(), &["send", &alice_chat_id, "Hello from Alice!"]).await;
    assert_eq!(result["status"], "ok", "Alice send failed");

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
    relay.stop().await;

    assert!(bob_received, "Bob did not receive Alice's message");

    println!("\n=== E2E test passed: ndr <-> ndr bidirectional messaging works! ===");
}

/// Helper to wait for a specific message with a running listener
async fn wait_for_message(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    expected_content: &str,
    label: &str,
) -> bool {
    let timeout_instant = std::time::Instant::now();
    while timeout_instant.elapsed() < Duration::from_secs(10) {
        let mut line = String::new();
        let read_result = tokio::time::timeout(
            Duration::from_millis(100),
            reader.read_line(&mut line)
        ).await;

        if let Ok(Ok(n)) = read_result {
            if n > 0 {
                let trimmed = line.trim();
                println!("[{} listen] {}", label, trimmed);
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if json["event"] == "message" {
                        let content = json["content"].as_str().unwrap_or("");
                        if content == expected_content {
                            println!("{} received message: {}", label, expected_content);
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// Test that listener keeps working across multiple message exchanges
/// This tests the key rotation issue - when messages are exchanged, ephemeral keys rotate
/// and the listener must update its subscriptions to receive messages on new keys.
#[tokio::test]
async fn test_ndr_long_conversation() {
    // Start WebSocket relay
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);
    println!("Relay started at: {}", relay_url);

    // Setup Alice and Bob
    let alice_dir = setup_ndr_dir(&relay_url, "");
    let bob_dir = setup_ndr_dir(&relay_url, "");
    let alice_sk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let bob_sk = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    // Login both
    run_ndr(alice_dir.path(), &["login", alice_sk]).await;
    run_ndr(bob_dir.path(), &["login", bob_sk]).await;

    // Alice creates invite, Bob joins
    let result = run_ndr(alice_dir.path(), &["invite", "create", "-l", "alice-invite"]).await;
    let invite_url = result["data"]["url"].as_str().unwrap().to_string();

    // Start Alice's listener for invite response
    let (mut alice_listen_child, mut alice_listen_reader) = start_ndr_listen(alice_dir.path(), None).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Bob joins
    let result = run_ndr(bob_dir.path(), &["chat", "join", &invite_url]).await;
    let bob_chat_id = result["data"]["id"].as_str().unwrap().to_string();

    // Wait for Alice to receive session_created
    let mut alice_chat_id = None;
    let timeout_instant = std::time::Instant::now();
    while timeout_instant.elapsed() < Duration::from_secs(10) {
        let mut line = String::new();
        if let Ok(Ok(n)) = tokio::time::timeout(Duration::from_millis(100), alice_listen_reader.read_line(&mut line)).await {
            if n > 0 {
                let trimmed = line.trim();
                println!("[Alice listen] {}", trimmed);
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if json["event"] == "session_created" {
                        alice_chat_id = json["chat_id"].as_str().map(|s| s.to_string());
                        break;
                    }
                }
            }
        }
    }
    let _ = alice_listen_child.kill().await;
    let alice_chat_id = alice_chat_id.expect("Alice did not receive invite response");

    println!("\n=== Starting long conversation test (listener stays running) ===\n");

    // Start PERSISTENT listeners for both Alice and Bob
    let (mut alice_listen_child, mut alice_listen_reader) = start_ndr_listen(alice_dir.path(), Some(&alice_chat_id)).await;
    let (mut bob_listen_child, mut bob_listen_reader) = start_ndr_listen(bob_dir.path(), Some(&bob_chat_id)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Round 1: Bob -> Alice
    println!("\n--- Round 1: Bob -> Alice ---");
    let result = run_ndr(bob_dir.path(), &["send", &bob_chat_id, "Message 1 from Bob"]).await;
    assert_eq!(result["status"], "ok", "Bob send 1 failed");
    assert!(wait_for_message(&mut alice_listen_reader, "Message 1 from Bob", "Alice").await,
        "Alice did not receive message 1 from Bob");

    // Round 2: Alice -> Bob
    println!("\n--- Round 2: Alice -> Bob ---");
    let result = run_ndr(alice_dir.path(), &["send", &alice_chat_id, "Message 2 from Alice"]).await;
    assert_eq!(result["status"], "ok", "Alice send 2 failed");
    assert!(wait_for_message(&mut bob_listen_reader, "Message 2 from Alice", "Bob").await,
        "Bob did not receive message 2 from Alice");

    // Round 3: Bob -> Alice (keys should have rotated by now)
    println!("\n--- Round 3: Bob -> Alice (after key rotation) ---");
    let result = run_ndr(bob_dir.path(), &["send", &bob_chat_id, "Message 3 from Bob"]).await;
    assert_eq!(result["status"], "ok", "Bob send 3 failed");
    assert!(wait_for_message(&mut alice_listen_reader, "Message 3 from Bob", "Alice").await,
        "Alice did not receive message 3 from Bob (KEY ROTATION BUG?)");

    // Round 4: Alice -> Bob
    println!("\n--- Round 4: Alice -> Bob ---");
    let result = run_ndr(alice_dir.path(), &["send", &alice_chat_id, "Message 4 from Alice"]).await;
    assert_eq!(result["status"], "ok", "Alice send 4 failed");
    assert!(wait_for_message(&mut bob_listen_reader, "Message 4 from Alice", "Bob").await,
        "Bob did not receive message 4 from Alice (KEY ROTATION BUG?)");

    // Round 5: Bob -> Alice
    println!("\n--- Round 5: Bob -> Alice ---");
    let result = run_ndr(bob_dir.path(), &["send", &bob_chat_id, "Message 5 from Bob"]).await;
    assert_eq!(result["status"], "ok", "Bob send 5 failed");
    assert!(wait_for_message(&mut alice_listen_reader, "Message 5 from Bob", "Alice").await,
        "Alice did not receive message 5 from Bob (KEY ROTATION BUG?)");

    // Round 6: Alice -> Bob
    println!("\n--- Round 6: Alice -> Bob ---");
    let result = run_ndr(alice_dir.path(), &["send", &alice_chat_id, "Message 6 from Alice"]).await;
    assert_eq!(result["status"], "ok", "Alice send 6 failed");
    assert!(wait_for_message(&mut bob_listen_reader, "Message 6 from Alice", "Bob").await,
        "Bob did not receive message 6 from Alice (KEY ROTATION BUG?)");

    // Cleanup
    let _ = alice_listen_child.kill().await;
    let _ = bob_listen_child.kill().await;
    relay.stop().await;

    println!("\n=== Long conversation test PASSED - listener handles key rotation correctly! ===");
}
