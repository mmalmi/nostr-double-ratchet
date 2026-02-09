//! Cross-language E2E test: TypeScript <-> ndr CLI
//!
//! This test verifies that ndr CLI can communicate with the TypeScript
//! implementation through a real WebSocket relay.

mod common;

use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Run ndr CLI command and return JSON output (async version)
async fn run_ndr(data_dir: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let output = Command::new("cargo")
        .env("NOSTR_PREFER_LOCAL", "0")
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

    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("Failed to parse ndr output: {}\nOutput: {}", e, stdout))
}

/// Start ndr listen in background and return the child process with stdout reader
async fn start_ndr_listen(
    data_dir: &std::path::Path,
    chat_id: &str,
) -> (Child, BufReader<tokio::process::ChildStdout>) {
    let mut child = Command::new("cargo")
        .env("NOSTR_PREFER_LOCAL", "0")
        .args(["run", "-q", "-p", "ndr", "--"])
        .arg("--json")
        .arg("--data-dir")
        .arg(data_dir)
        .arg("listen")
        .arg("--chat")
        .arg(chat_id)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("Failed to start ndr listen");

    let stdout = BufReader::new(child.stdout.take().expect("Failed to capture stdout"));
    (child, stdout)
}

/// Start the TypeScript e2e script and capture its output
async fn start_ts_script(relay_url: &str) -> (Child, BufReader<tokio::process::ChildStdout>) {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();

    let ts_dir = repo_root.join("ts");

    let mut child = Command::new("npx")
        .arg("tsx")
        .arg("e2e/ts-rust-e2e.ts")
        .arg(relay_url)
        .current_dir(&ts_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("Failed to start TypeScript script");

    let stdout = BufReader::new(child.stdout.take().expect("Failed to capture stdout"));
    (child, stdout)
}

/// Read lines from TypeScript script until we find a specific marker
async fn read_until_marker(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    prefix: &str,
) -> Option<String> {
    let timeout = Duration::from_secs(30);

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
async fn test_ts_rust_e2e() {
    // Start WebSocket relay
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);
    println!("Relay started at: {}", relay_url);

    // Start TypeScript script
    let (mut ts_child, mut ts_reader) = start_ts_script(&relay_url).await;

    // Wait for TypeScript to output the invite URL
    let _alice_pubkey = read_until_marker(&mut ts_reader, "E2E_ALICE_PUBKEY:")
        .await
        .expect("Failed to get Alice pubkey");
    println!("Got Alice pubkey");

    // Wait for WS connection
    let _ws_open = read_until_marker(&mut ts_reader, "E2E_WS_OPEN").await;
    let _relay_connected = read_until_marker(&mut ts_reader, "E2E_RELAY_CONNECTED:")
        .await
        .expect("TypeScript failed to connect to relay");
    println!("TypeScript connected to relay");

    let invite_url = read_until_marker(&mut ts_reader, "E2E_INVITE_URL:")
        .await
        .expect("Failed to get invite URL");
    let _listening = read_until_marker(&mut ts_reader, "E2E_LISTENING")
        .await
        .expect("TypeScript not listening");

    println!("Got invite URL: {}", invite_url);

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

    // Bob joins via the invite URL
    let result = run_ndr(bob_dir.path(), &["chat", "join", &invite_url]).await;
    assert_eq!(result["status"], "ok", "Bob join failed");
    let bob_chat_id = result["data"]["id"].as_str().unwrap().to_string();
    let response_event = result["data"]["response_event"]
        .as_str()
        .unwrap()
        .to_string();

    println!("Bob joined chat: {}", bob_chat_id);

    // Publish the response event to the relay (ndr outputs it but doesn't publish)
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    let (mut ws, _) = connect_async(&relay_url)
        .await
        .expect("Failed to connect to relay");
    let event_msg = format!(r#"["EVENT",{}]"#, response_event);
    ws.send(Message::Text(event_msg))
        .await
        .expect("Failed to send event");

    // Wait for OK response
    if let Some(Ok(Message::Text(response))) = ws.next().await {
        println!("Relay response: {}", response);
    }

    // Give TypeScript time to process the response event
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Check if TypeScript created a session
    let session_created = read_until_marker(&mut ts_reader, "E2E_SESSION_CREATED:").await;
    if session_created.is_some() {
        println!("TypeScript created session");
    }

    // Bob sends a message
    let result = run_ndr(bob_dir.path(), &["send", &bob_chat_id, "Hello from ndr!"]).await;
    assert_eq!(result["status"], "ok", "Bob send failed");
    let encrypted_event = result["data"]["event"].as_str().unwrap().to_string();

    println!("Bob sent message, publishing to relay...");

    // Publish the encrypted message to the relay
    let event_msg = format!(r#"["EVENT",{}]"#, encrypted_event);
    ws.send(Message::Text(event_msg))
        .await
        .expect("Failed to send message event");

    // Wait for TypeScript to receive the message and send reply
    let mut ndr_to_ts_success = false;
    let timeout_instant = std::time::Instant::now();
    while timeout_instant.elapsed() < Duration::from_secs(10) {
        let mut line = String::new();
        let read_result =
            tokio::time::timeout(Duration::from_millis(100), ts_reader.read_line(&mut line)).await;

        if let Ok(Ok(n)) = read_result {
            if n > 0 {
                let trimmed = line.trim();
                println!("[TS] {}", trimmed);
                if let Some(content) = trimmed.strip_prefix("E2E_MESSAGE_RECEIVED:") {
                    assert_eq!(content, "Hello from ndr!");
                    ndr_to_ts_success = true;
                }
                if trimmed.starts_with("E2E_REPLY_SENT") {
                    break;
                }
            }
        }
    }

    assert!(
        ndr_to_ts_success,
        "TypeScript did not receive the message from ndr"
    );
    println!("ndr -> TypeScript: OK");

    // Now test TypeScript -> ndr direction via relay
    // Start ndr listen to receive messages through the relay
    println!("Starting ndr listen to receive reply via relay...");
    let (mut ndr_listen_child, mut ndr_listen_reader) =
        start_ndr_listen(bob_dir.path(), &bob_chat_id).await;

    // Give ndr listen time to connect and subscribe
    tokio::time::sleep(Duration::from_millis(500)).await;

    // TypeScript's reply was already published to relay when it received our message
    // ndr listen should receive it through the relay subscription

    // Wait for ndr listen to receive the message
    let mut ts_to_ndr_success = false;
    let timeout_instant = std::time::Instant::now();
    while timeout_instant.elapsed() < Duration::from_secs(10) {
        let mut line = String::new();
        let read_result = tokio::time::timeout(
            Duration::from_millis(100),
            ndr_listen_reader.read_line(&mut line),
        )
        .await;

        if let Ok(Ok(n)) = read_result {
            if n > 0 {
                let trimmed = line.trim();
                println!("[ndr listen] {}", trimmed);
                // ndr listen outputs JSON events like {"event":"message","content":"..."}
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if json["event"] == "message" {
                        let content = json["content"].as_str().unwrap_or("");
                        if content == "Hello from TypeScript!" {
                            ts_to_ndr_success = true;
                            break;
                        }
                    }
                }
            }
        }
    }

    // Cleanup
    let _ = ndr_listen_child.kill().await;
    let _ = ts_child.kill().await;
    relay.stop().await;

    assert!(
        ts_to_ndr_success,
        "ndr did not receive the message from TypeScript via relay"
    );
    println!("TypeScript -> ndr (via relay): OK");
    println!("Bidirectional E2E test passed!");
}
