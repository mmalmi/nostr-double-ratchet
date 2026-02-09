//! Cross-language E2E test: TypeScript group sender-keys <-> ndr CLI
//!
//! Verifies that:
//! - TypeScript can create a group (via metadata over a 1:1 session) and publish a sender-key
//!   one-to-many message that ndr can decrypt.
//! - ndr can send a sender-key one-to-many group message that TypeScript can decrypt.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Run ndr CLI command and return JSON output (async version).
async fn run_ndr(data_dir: &Path, args: &[&str]) -> serde_json::Value {
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

/// Start ndr listen in background and return (child, stdout_reader).
async fn start_ndr_listen(data_dir: &Path) -> (Child, BufReader<tokio::process::ChildStdout>) {
    let mut child = Command::new("cargo")
        .env("NOSTR_PREFER_LOCAL", "0")
        .args(["run", "-q", "-p", "ndr", "--"])
        .arg("--json")
        .arg("--data-dir")
        .arg(data_dir)
        .arg("listen")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("Failed to start ndr listen");

    let stdout = BufReader::new(child.stdout.take().expect("Failed to capture stdout"));
    (child, stdout)
}

/// Start the TypeScript group e2e script and capture its output.
async fn start_ts_group_script(relay_url: &str) -> (Child, BufReader<tokio::process::ChildStdout>) {
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
        .arg("e2e/ts-rust-group-e2e.ts")
        .arg(relay_url)
        .current_dir(&ts_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("Failed to start TypeScript script");

    let stdout = BufReader::new(child.stdout.take().expect("Failed to capture stdout"));
    (child, stdout)
}

fn setup_ndr_dir(relay_url: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
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

async fn read_until_ts_marker(
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

async fn read_until_ndr_command(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    command: &str,
    timeout: Duration,
) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let mut line = String::new();
        match tokio::time::timeout(Duration::from_millis(200), reader.read_line(&mut line)).await {
            Ok(Ok(0)) => return false, // EOF
            Ok(Ok(_)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if json.get("command").and_then(|v| v.as_str()) == Some(command) {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    false
}

async fn read_until_ndr_event(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    event_name: &str,
    timeout: Duration,
) -> Option<serde_json::Value> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let mut line = String::new();
        match tokio::time::timeout(Duration::from_millis(200), reader.read_line(&mut line)).await {
            Ok(Ok(0)) => return None, // EOF
            Ok(Ok(_)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if json.get("event").and_then(|v| v.as_str()) == Some(event_name) {
                        return Some(json);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

async fn read_until_ndr_group_messages(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    group_id: &str,
    expected_contents: &[&str],
    timeout: Duration,
) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let expected: std::collections::HashSet<&str> = expected_contents.iter().copied().collect();

    let start = Instant::now();
    while start.elapsed() < timeout && seen.len() < expected.len() {
        let mut line = String::new();
        match tokio::time::timeout(Duration::from_millis(200), reader.read_line(&mut line)).await {
            Ok(Ok(0)) => break, // EOF
            Ok(Ok(_)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                    continue;
                };
                if json.get("event").and_then(|v| v.as_str()) != Some("group_message") {
                    continue;
                }
                if json.get("group_id").and_then(|v| v.as_str()) != Some(group_id) {
                    continue;
                }
                let Some(content) = json.get("content").and_then(|v| v.as_str()) else {
                    continue;
                };
                if expected.contains(content) {
                    seen.insert(content.to_string());
                }
            }
            _ => {}
        }
    }

    let mut out: Vec<String> = seen.into_iter().collect();
    out.sort();
    out
}

async fn stop_child(mut child: Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            let _ = Command::new("kill")
                .arg("-INT")
                .arg(pid.to_string())
                .status()
                .await;
        }
    }

    if tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

#[tokio::test]
async fn test_ts_rust_group_sender_keys_e2e() {
    // Start WebSocket relay.
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);

    // Start TypeScript script (Alice).
    let (ts_child, mut ts_reader) = start_ts_group_script(&relay_url).await;

    // Wait for invite URL.
    let _alice_pubkey =
        read_until_ts_marker(&mut ts_reader, "E2E_ALICE_PUBKEY:", Duration::from_secs(20))
            .await
            .expect("Failed to get Alice pubkey");

    let invite_url =
        read_until_ts_marker(&mut ts_reader, "E2E_INVITE_URL:", Duration::from_secs(20))
            .await
            .expect("Failed to get invite URL");

    // Setup ndr (Bob).
    let bob_dir = setup_ndr_dir(&relay_url);
    let bob_sk = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    let _bob_login = run_ndr(bob_dir.path(), &["login", bob_sk]).await;

    // Start Bob listen (so it can receive group metadata + sender keys).
    let (bob_listen_child, mut bob_stdout) = start_ndr_listen(bob_dir.path()).await;
    assert!(
        read_until_ndr_command(&mut bob_stdout, "listen", Duration::from_secs(10)).await,
        "Bob should print listen command"
    );

    // Bob joins Alice's invite (creates 1:1 session and publishes response).
    let joined = run_ndr(bob_dir.path(), &["chat", "join", &invite_url]).await;
    let bob_chat_id = joined["data"]["id"]
        .as_str()
        .expect("join chat id")
        .to_string();

    // Wait for TS to report session created.
    let _session = read_until_ts_marker(
        &mut ts_reader,
        "E2E_SESSION_CREATED:",
        Duration::from_secs(20),
    )
    .await
    .expect("TS did not create session");

    // Bob sends a 1:1 handshake message so the inviter (TS) can send back (responder can't send first).
    let _ = run_ndr(bob_dir.path(), &["send", &bob_chat_id, "hi"]).await;

    // Wait for group to be created on Bob via metadata routed over the 1:1 session.
    let created = read_until_ndr_event(&mut bob_stdout, "group_metadata", Duration::from_secs(30))
        .await
        .expect("Expected group_metadata event");
    assert_eq!(
        created.get("action").and_then(|v| v.as_str()),
        Some("created"),
        "Expected group_metadata action=created, got: {created}"
    );
    let group_id = created
        .get("group_id")
        .and_then(|v| v.as_str())
        .expect("group_id")
        .to_string();

    // Accept group so ndr subscribes to group shared channel and per-sender published messages.
    let _ = run_ndr(bob_dir.path(), &["group", "accept", &group_id]).await;

    // TS should publish multiple sender-key one-to-many messages to the group (multi-device same owner).
    let expected_ts = [
        "hello from ts device1 first",
        "hello from ts device1 second",
        "hello from ts device2",
    ];
    let got = read_until_ndr_group_messages(
        &mut bob_stdout,
        &group_id,
        &expected_ts,
        Duration::from_secs(60),
    )
    .await;
    assert_eq!(
        got.len(),
        expected_ts.len(),
        "Expected all TS messages to be decrypted. Got: {got:?}"
    );

    // Now Bob sends a one-to-many group message; TS should decrypt it.
    let rust_msg = "hello from rust group";
    let _ = run_ndr(bob_dir.path(), &["group", "send", &group_id, rust_msg]).await;

    let got = read_until_ts_marker(
        &mut ts_reader,
        "E2E_GROUP_MESSAGE_RECEIVED:",
        Duration::from_secs(45),
    )
    .await
    .expect("TS did not decrypt Rust group message");
    assert_eq!(got, rust_msg);

    stop_child(ts_child).await;
    stop_child(bob_listen_child).await;
    relay.stop().await;
}
