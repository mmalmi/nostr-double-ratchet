//! E2E test: ndr listen with empty data directory (no chats, no invites)
//!
//! This test verifies the fix for the "not subscribed" error that occurred when
//! `ndr listen` was called before any invites or chats existed. The listener should
//! now gracefully wait for new invites/chats via filesystem watching instead of erroring.

mod common;

use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Start ndr listen in background and return (child, stdout_reader, stderr_reader)
async fn start_ndr_listen_with_stderr(
    data_dir: &std::path::Path,
) -> (
    Child,
    BufReader<tokio::process::ChildStdout>,
    BufReader<tokio::process::ChildStderr>,
) {
    let mut child = Command::new("cargo")
        .args(["run", "-q", "-p", "ndr", "--"])
        .arg("--json")
        .arg("--data-dir")
        .arg(data_dir)
        .arg("listen")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("Failed to start ndr listen");

    let stdout = BufReader::new(child.stdout.take().expect("Failed to capture stdout"));
    let stderr = BufReader::new(child.stderr.take().expect("Failed to capture stderr"));
    (child, stdout, stderr)
}

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

/// Setup ndr data directory with config for test relay
fn setup_ndr_dir(relay_url: &str) -> TempDir {
    let dir = TempDir::new().unwrap();

    // Write config with relay
    let config_content = serde_json::json!({
        "relays": [relay_url]
    });
    std::fs::write(
        dir.path().join("config.json"),
        serde_json::to_string(&config_content).unwrap(),
    )
    .unwrap();

    dir
}

/// Test that `ndr listen` works with an empty data directory (no chats, no invites).
///
/// Previously, this would fail with "not subscribed" error because the nostr client
/// would throw an error when subscribe() was called with empty filters, or when
/// notifications.recv() was called without an active subscription.
///
/// The fix implements filesystem watching: listen() now waits for invites/chats to be
/// created before connecting to relays and subscribing.
#[tokio::test]
async fn test_listen_empty_data_dir() {
    // Start WebSocket relay
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);
    println!("Relay started at: {}", relay_url);

    // Setup fresh directory with no invites or chats
    let dir = setup_ndr_dir(&relay_url);
    let secret_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    // Login
    let result = run_ndr(dir.path(), &["login", secret_key]).await;
    assert_eq!(result["status"], "ok", "Login failed");

    // Verify invites and chats directories are empty or don't exist
    let invites_dir = dir.path().join("invites");
    let chats_dir = dir.path().join("chats");
    assert!(
        !invites_dir.exists() || std::fs::read_dir(&invites_dir).unwrap().count() == 0,
        "invites directory should be empty"
    );
    assert!(
        !chats_dir.exists() || std::fs::read_dir(&chats_dir).unwrap().count() == 0,
        "chats directory should be empty"
    );

    // Start ndr listen - this used to fail with "not subscribed" error
    let (mut child, mut stdout_reader, _stderr_reader) = start_ndr_listen_with_stderr(dir.path()).await;

    // Wait a bit for it to start and output the listening message
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Check if process is still running
    let status = child.try_wait().expect("Failed to check process status");
    assert!(
        status.is_none(),
        "ndr listen should still be running with empty data dir, but it exited: {:?}",
        status
    );

    // Read stdout to find the listening message
    let mut found_listening_msg = false;
    let mut stdout_lines = Vec::new();
    loop {
        let mut line = String::new();
        match tokio::time::timeout(Duration::from_millis(200), stdout_reader.read_line(&mut line)).await {
            Ok(Ok(0)) => break, // EOF
            Ok(Ok(_)) => {
                let trimmed = line.trim();
                println!("[stdout] {}", trimmed);
                stdout_lines.push(trimmed.to_string());
                if trimmed.contains("Listening for messages") {
                    found_listening_msg = true;
                }
            }
            Ok(Err(e)) => {
                println!("[stdout error] {}", e);
                break;
            }
            Err(_) => break, // timeout - no more output
        }
    }

    // Verify the listener output
    assert!(
        found_listening_msg,
        "Should have printed 'Listening for messages...' message. stdout: {:?}",
        stdout_lines
    );

    // Clean up
    let _ = child.kill().await;
    relay.stop().await;

    println!("\n=== Test PASSED: ndr listen works with empty data directory ===");
}

/// Test that ndr listen dynamically picks up new invites via filesystem watching.
///
/// Flow:
/// 1. Start ndr listen with empty data dir
/// 2. Create an invite while listener is running
/// 3. Verify listener starts subscribing to the new invite
#[tokio::test]
async fn test_listen_detects_new_invite() {
    // Start WebSocket relay
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);
    println!("Relay started at: {}", relay_url);

    // Setup fresh directory
    let dir = setup_ndr_dir(&relay_url);
    let secret_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    // Login
    let result = run_ndr(dir.path(), &["login", secret_key]).await;
    assert_eq!(result["status"], "ok", "Login failed");

    // Start ndr listen with empty data dir
    let (mut child, mut stdout_reader, _stderr_reader) = start_ndr_listen_with_stderr(dir.path()).await;

    // Wait for listener to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify it's running
    assert!(
        child.try_wait().expect("check status").is_none(),
        "ndr listen should be running"
    );

    // Wait for initial listening message
    let mut got_listening_msg = false;
    let timeout_start = std::time::Instant::now();
    while timeout_start.elapsed() < Duration::from_secs(5) {
        let mut line = String::new();
        match tokio::time::timeout(Duration::from_millis(100), stdout_reader.read_line(&mut line)).await {
            Ok(Ok(n)) if n > 0 => {
                let trimmed = line.trim();
                println!("[listener] {}", trimmed);
                if trimmed.contains("Listening for messages") {
                    got_listening_msg = true;
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(got_listening_msg, "Should have received listening message");

    // Now create an invite while listener is running
    println!("\n--- Creating invite while listener is running ---");
    let result = run_ndr(dir.path(), &["invite", "create", "-l", "test-invite"]).await;
    assert_eq!(result["status"], "ok", "Invite create failed");
    let _invite_url = result["data"]["url"].as_str().unwrap();
    println!("Created invite: {}", _invite_url);

    // Give filesystem watcher time to detect the new invite
    tokio::time::sleep(Duration::from_secs(1)).await;

    // The listener should still be running
    assert!(
        child.try_wait().expect("check status").is_none(),
        "ndr listen should still be running after invite creation"
    );

    // Note: We can't easily verify that the listener subscribed to the new invite
    // without having another party respond to it. But the key thing is that it
    // didn't crash and is still running.

    // Clean up
    let _ = child.kill().await;
    relay.stop().await;

    println!("\n=== Test PASSED: ndr listen handles new invites while running ===");
}

/// Test the full flow: start with empty dir, create invite, have it accepted,
/// and verify the listener receives the session_created event.
#[tokio::test]
async fn test_listen_receives_invite_response_after_dynamic_subscribe() {
    // Start WebSocket relay
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);
    println!("Relay started at: {}", relay_url);

    // Setup Alice (starts with empty dir, creates invite while listening)
    let alice_dir = setup_ndr_dir(&relay_url);
    let alice_sk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    // Setup Bob (will join Alice's invite)
    let bob_dir = setup_ndr_dir(&relay_url);
    let bob_sk = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    // Login both
    run_ndr(alice_dir.path(), &["login", alice_sk]).await;
    run_ndr(bob_dir.path(), &["login", bob_sk]).await;

    // Alice starts listening with EMPTY data dir
    println!("\n--- Alice starts listening with empty data dir ---");
    let (mut alice_child, mut alice_stdout, _alice_stderr) = start_ndr_listen_with_stderr(alice_dir.path()).await;

    // Wait for Alice to be ready
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(alice_child.try_wait().unwrap().is_none(), "Alice listener should be running");

    // Wait for listening message
    let timeout_start = std::time::Instant::now();
    while timeout_start.elapsed() < Duration::from_secs(5) {
        let mut line = String::new();
        match tokio::time::timeout(Duration::from_millis(100), alice_stdout.read_line(&mut line)).await {
            Ok(Ok(n)) if n > 0 => {
                let trimmed = line.trim();
                println!("[Alice] {}", trimmed);
                if trimmed.contains("Listening") {
                    break;
                }
            }
            _ => {}
        }
    }

    // Alice creates invite WHILE listener is running
    println!("\n--- Alice creates invite while listener is running ---");
    let result = run_ndr(alice_dir.path(), &["invite", "create", "-l", "alice-invite"]).await;
    assert_eq!(result["status"], "ok", "Alice invite create failed");
    let invite_url = result["data"]["url"].as_str().unwrap().to_string();
    println!("Alice created invite: {}", invite_url);

    // Give filesystem watcher time to detect new invite
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Bob joins Alice's invite
    println!("\n--- Bob joins Alice's invite ---");
    let result = run_ndr(bob_dir.path(), &["chat", "join", &invite_url]).await;
    assert_eq!(result["status"], "ok", "Bob join failed");
    let bob_chat_id = result["data"]["id"].as_str().unwrap();
    println!("Bob joined with chat ID: {}", bob_chat_id);

    // Alice should receive the session_created event
    println!("\n--- Waiting for Alice to receive session_created ---");
    let mut alice_received_session = false;
    let timeout_start = std::time::Instant::now();
    while timeout_start.elapsed() < Duration::from_secs(10) {
        let mut line = String::new();
        match tokio::time::timeout(Duration::from_millis(100), alice_stdout.read_line(&mut line)).await {
            Ok(Ok(n)) if n > 0 => {
                let trimmed = line.trim();
                println!("[Alice] {}", trimmed);
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if json["event"] == "session_created" {
                        println!("SUCCESS: Alice received session_created event!");
                        alice_received_session = true;
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    // Clean up
    let _ = alice_child.kill().await;
    relay.stop().await;

    assert!(
        alice_received_session,
        "Alice should have received session_created event via dynamic subscription"
    );

    println!("\n=== Test PASSED: Full flow with dynamic subscription works! ===");
}
