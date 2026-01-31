//! E2E test: ndr listen handles group metadata and group messages over WebSocket relay

mod common;

use std::path::Path;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Run ndr CLI command and return JSON output
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

    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("Failed to parse ndr output: {}\nOutput: {}", e, stdout)
    })
}

/// Start ndr listen in background and return (child, stdout_reader)
async fn start_ndr_listen(
    data_dir: &Path,
) -> (Child, BufReader<tokio::process::ChildStdout>) {
    let mut child = Command::new("cargo")
        .env("NOSTR_PREFER_LOCAL", "0")
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
    (child, stdout)
}

/// Setup ndr data directory with config for test relay
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

async fn read_until_command(
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

async fn read_until_event(
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
async fn test_listen_group_metadata_and_message() {
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);
    println!("Relay started at: {}", relay_url);

    let alice_dir = setup_ndr_dir(&relay_url);
    let bob_dir = setup_ndr_dir(&relay_url);

    let alice_sk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let bob_sk = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    let alice_login = run_ndr(alice_dir.path(), &["login", alice_sk]).await;
    let alice_pubkey = alice_login["data"]["pubkey"]
        .as_str()
        .expect("alice pubkey")
        .to_string();

    let bob_login = run_ndr(bob_dir.path(), &["login", bob_sk]).await;
    let bob_pubkey = bob_login["data"]["pubkey"]
        .as_str()
        .expect("bob pubkey")
        .to_string();

    let mut alice_child: Option<Child> = None;
    let mut bob_child: Option<Child> = None;

    let result = async {
        // Start Alice listen so invite response can be processed.
        let (child, mut alice_stdout) = start_ndr_listen(alice_dir.path()).await;
        alice_child = Some(child);
        assert!(
            read_until_command(&mut alice_stdout, "listen", Duration::from_secs(5)).await,
            "Alice should print listen message"
        );

        // Alice creates invite, Bob joins.
        let invite = run_ndr(alice_dir.path(), &["invite", "create", "-l", "group-test"]).await;
        let invite_url = invite["data"]["url"].as_str().expect("invite url").to_string();
        let _bob_join = run_ndr(bob_dir.path(), &["chat", "join", &invite_url]).await;

        // Alice should receive session_created.
        let session_event = read_until_event(&mut alice_stdout, "session_created", Duration::from_secs(10))
            .await
            .expect("Alice should receive session_created event");
        assert_eq!(
            session_event["their_pubkey"].as_str(),
            Some(bob_pubkey.as_str()),
            "Alice session_created should reference Bob pubkey"
        );

        // Bob sends a kickoff message so Alice can send later.
        let kickoff_text = "kickoff";
        let _ = run_ndr(bob_dir.path(), &["send", &alice_pubkey, kickoff_text]).await;
        let kickoff_event = read_until_event(&mut alice_stdout, "message", Duration::from_secs(10))
            .await
            .expect("Alice should receive kickoff message");
        assert_eq!(kickoff_event["content"].as_str(), Some(kickoff_text));

        // Start Bob listen.
        let (child, mut bob_stdout) = start_ndr_listen(bob_dir.path()).await;
        bob_child = Some(child);
        assert!(
            read_until_command(&mut bob_stdout, "listen", Duration::from_secs(5)).await,
            "Bob should print listen message"
        );

        // Alice creates a group with Bob.
        let group_create = run_ndr(
            alice_dir.path(),
            &["group", "create", "--name", "Test Group", "--members", &bob_pubkey],
        )
        .await;
        let group_id = group_create["data"]["id"]
            .as_str()
            .expect("group id")
            .to_string();

        // Bob should receive group metadata creation.
        let metadata_event = read_until_event(&mut bob_stdout, "group_metadata", Duration::from_secs(10))
            .await
            .expect("Bob should receive group_metadata event");
        assert_eq!(
            metadata_event["group_id"].as_str(),
            Some(group_id.as_str())
        );
        assert_eq!(metadata_event["action"].as_str(), Some("created"));

        // Alice sends a group message.
        let msg_text = "hello group";
        let _ = run_ndr(
            alice_dir.path(),
            &["group", "send", &group_id, msg_text],
        )
        .await;

        // Bob should receive group message.
        let msg_event = read_until_event(&mut bob_stdout, "group_message", Duration::from_secs(10))
            .await
            .expect("Bob should receive group_message event");
        assert_eq!(msg_event["group_id"].as_str(), Some(group_id.as_str()));
        assert_eq!(msg_event["content"].as_str(), Some(msg_text));

        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Some(child) = alice_child {
        stop_child(child).await;
    }
    if let Some(child) = bob_child {
        stop_child(child).await;
    }
    relay.stop().await;

    if let Err(err) = result {
        panic!("{:?}", err);
    }
}
