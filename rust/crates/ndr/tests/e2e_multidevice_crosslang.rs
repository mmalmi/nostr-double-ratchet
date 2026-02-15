//! Cross-language E2E test: TypeScript multi-device (AppKeys + device invites) <-> ndr CLI
//!
//! Scenario:
//! 1) TS (owner device) publishes AppKeys listing only itself, and creates a chat invite URL
//! 2) ndr joins the chat via URL (invite response is published by the test harness)
//! 3) ndr sends PING1 (TS owner device receives it)
//! 4) TS publishes updated AppKeys adding a second device + that device's Invite event
//! 5) ndr SessionManager discovers the new device and establishes a session
//! 6) ndr sends PING2 and both TS devices must receive/decrypt it

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap() // crates
        .parent()
        .unwrap() // rust
        .parent()
        .unwrap() // repo root
        .to_path_buf()
}

/// Run ndr CLI command and return JSON output
async fn run_ndr(data_dir: &Path, args: &[&str]) -> serde_json::Value {
    let output = Command::new(common::ndr_binary())
        .env("NOSTR_PREFER_LOCAL", "0")
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

    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("Failed to parse ndr output: {}\nOutput: {}", e, stdout))
}

/// Start ndr listen in background and return the child process
async fn start_ndr_listen(data_dir: &Path, chat_id: &str) -> Child {
    Command::new(common::ndr_binary())
        .env("NOSTR_PREFER_LOCAL", "0")
        .arg("--json")
        .arg("--data-dir")
        .arg(data_dir)
        .arg("listen")
        .arg("--chat")
        .arg(chat_id)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("Failed to start ndr listen")
}

/// Start the TypeScript e2e script and capture its output
async fn start_ts_script(relay_url: &str) -> (Child, BufReader<tokio::process::ChildStdout>) {
    let ts_dir = repo_root().join("ts");
    let mut child = Command::new("npx")
        .arg("tsx")
        .arg("e2e/ts-rust-multidevice-e2e.ts")
        .arg(relay_url)
        .current_dir(&ts_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("Failed to start TypeScript script");

    let stdout = BufReader::new(child.stdout.take().expect("Failed to capture stdout"));
    (child, stdout)
}

async fn read_until_marker(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    prefix: &str,
    timeout: Duration,
) -> Option<String> {
    tokio::time::timeout(timeout, async {
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => return None, // EOF
                Ok(_) => {
                    let trimmed = line.trim();
                    println!("[TS] {}", trimmed);
                    if let Some(rest) = trimmed.strip_prefix(prefix) {
                        return Some(rest.to_string());
                    }
                }
                Err(_) => return None,
            }
        }
    })
    .await
    .ok()
    .flatten()
}

#[tokio::test]
async fn test_ts_ndr_multidevice_appkeys_fanout() {
    // Start WebSocket relay
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);
    println!("Relay started at: {}", relay_url);

    // Start TS script
    let (mut ts_child, mut ts_reader) = start_ts_script(&relay_url).await;

    // Wait for essentials from TS
    let _connected = read_until_marker(&mut ts_reader, "E2E_RELAY_CONNECTED:", Duration::from_secs(10))
        .await
        .expect("TS failed to connect to relay");
    let _owner_pubkey = read_until_marker(&mut ts_reader, "E2E_OWNER_PUBKEY:", Duration::from_secs(10))
        .await
        .expect("Missing owner pubkey");
    let invite_url = read_until_marker(&mut ts_reader, "E2E_INVITE_URL:", Duration::from_secs(10))
        .await
        .expect("Missing invite URL");

    // Setup ndr (Bob)
    let bob_dir = TempDir::new().unwrap();
    let bob_sk = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    // Configure ndr to use our test relay
    let config_content = serde_json::json!({
        "relays": [&relay_url]
    });
    std::fs::write(
        bob_dir.path().join("config.json"),
        serde_json::to_string(&config_content).unwrap(),
    )
    .unwrap();

    // Bob logs in
    let result = run_ndr(bob_dir.path(), &["login", bob_sk]).await;
    assert_eq!(result["status"], "ok", "Bob login failed");

    // Bob joins via invite URL
    let result = run_ndr(bob_dir.path(), &["chat", "join", &invite_url]).await;
    assert_eq!(result["status"], "ok", "Bob join failed");
    let chat_id = result["data"]["id"].as_str().unwrap().to_string();
    let response_event = result["data"]["response_event"]
        .as_str()
        .unwrap()
        .to_string();

    // Publish the response event to the relay (ndr outputs it but doesn't publish)
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    let (mut ws, _) = connect_async(&relay_url)
        .await
        .expect("Failed to connect to relay");
    let event_msg = format!(r#"["EVENT",{}]"#, response_event);
    ws.send(Message::Text(event_msg))
        .await
        .expect("Failed to send invite response event");
    // Wait for OK response (best-effort)
    let _ = ws.next().await;

    // Wait for TS to establish the session
    let _session_identity = read_until_marker(
        &mut ts_reader,
        "E2E_SESSION1_CREATED:",
        Duration::from_secs(20),
    )
    .await
    .expect("TS did not establish session1");

    // Start ndr listen (needed to discover AppKeys updates + accept device invites)
    let mut listen_child = start_ndr_listen(bob_dir.path(), &chat_id).await;
    tokio::time::sleep(Duration::from_millis(800)).await;

    // Send PING1 (TS should receive and then publish device2 + AppKeys update)
    let result = run_ndr(bob_dir.path(), &["send", &chat_id, "PING1"]).await;
    assert_eq!(result["status"], "ok", "Bob send PING1 failed");
    let _ = read_until_marker(
        &mut ts_reader,
        "E2E_DEVICE1_RECEIVED:PING1",
        Duration::from_secs(30),
    )
    .await
    .expect("TS device1 did not receive PING1");

    // Wait for device2 to establish a session with ndr (TS prints this once it processes invite response)
    let _ = read_until_marker(
        &mut ts_reader,
        "E2E_DEVICE2_SESSION_CREATED:",
        Duration::from_secs(45),
    )
    .await
    .expect("TS device2 did not establish a session");

    // Send PING2 and expect TS to exit successfully after both devices decrypt it
    let result = run_ndr(bob_dir.path(), &["send", &chat_id, "PING2"]).await;
    assert_eq!(result["status"], "ok", "Bob send PING2 failed");
    let _ = read_until_marker(&mut ts_reader, "E2E_SUCCESS", Duration::from_secs(45))
        .await
        .expect("TS did not report success (missing fanout to both devices)");

    // Cleanup
    let _ = listen_child.kill().await;
    let _ = ts_child.kill().await;
    relay.stop().await;
}

