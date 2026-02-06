//! E2E test: link a device and register it in AppKeys (multi-device support)

mod common;

use std::time::{Duration, Instant};

use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Run ndr CLI command and return JSON output
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
) -> (Child, BufReader<tokio::process::ChildStdout>) {
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

#[tokio::test]
async fn test_link_flow_publishes_app_keys_and_links_device() {
    // Start WebSocket relay
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);

    // Device to be linked: create link invite (auto-generates identity)
    let device_dir = setup_ndr_dir(&relay_url);
    let created = run_ndr(device_dir.path(), &["link", "create"]).await;
    assert_eq!(created["status"], "ok");
    let link_url = created["data"]["url"]
        .as_str()
        .expect("Expected link URL")
        .to_string();
    let device_pubkey = created["data"]["device_pubkey"]
        .as_str()
        .expect("Expected device pubkey")
        .to_string();

    // Start listener on device (waits for link acceptance)
    let (mut device_listen, mut device_stdout) = start_ndr_listen(device_dir.path()).await;

    // Owner device: accept the link invite (auto-generates identity)
    let owner_dir = setup_ndr_dir(&relay_url);
    let accepted = run_ndr(owner_dir.path(), &["link", "accept", &link_url]).await;
    assert_eq!(accepted["status"], "ok");
    let owner_pubkey = accepted["data"]["owner_pubkey"]
        .as_str()
        .expect("Expected owner pubkey")
        .to_string();

    // Wait for link_accepted event from device listener
    let mut linked_owner: Option<String> = None;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        let mut line = String::new();
        let read = tokio::time::timeout(
            Duration::from_millis(250),
            device_stdout.read_line(&mut line),
        )
        .await;
        match read {
            Ok(Ok(0)) => break, // EOF
            Ok(Ok(_)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if v.get("event").and_then(|e| e.as_str()) == Some("link_accepted") {
                        linked_owner = v
                            .get("owner_pubkey")
                            .and_then(|p| p.as_str())
                            .map(|s| s.to_string());
                        break;
                    }
                }
            }
            Ok(Err(_)) => break,
            Err(_) => continue, // timeout
        }
    }

    let _ = device_listen.kill().await;
    assert_eq!(linked_owner.as_deref(), Some(owner_pubkey.as_str()));

    // Device config should now be in linked mode
    let device_config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(device_dir.path().join("config.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        device_config["linked_owner"].as_str(),
        Some(owner_pubkey.as_str())
    );

    // Relay should have an AppKeys event from owner listing both owner and device
    tokio::time::sleep(Duration::from_millis(200)).await;
    let events = relay.events().await;

    let is_d = |event: &common::ws_relay::NostrEvent, value: &str| {
        event
            .tags
            .iter()
            .any(|t| t.len() >= 2 && t[0] == "d" && t[1] == value)
    };

    let app_keys_event = events
        .iter()
        .find(|e| {
            e.kind == nostr_double_ratchet::APP_KEYS_EVENT_KIND
                && e.pubkey == owner_pubkey
                && is_d(e, "double-ratchet/app-keys")
        })
        .expect("Expected AppKeys event to be published");

    let has_device = |pk: &str| {
        app_keys_event
            .tags
            .iter()
            .any(|t| t.len() >= 3 && t[0] == "device" && t[1] == pk)
    };
    assert!(has_device(&owner_pubkey));
    assert!(has_device(&device_pubkey));

    // Device should also publish its device invite under d=double-ratchet/invites/<device_pubkey>
    let expected_invite_d = format!("double-ratchet/invites/{}", device_pubkey);
    let device_invite_event = events
        .iter()
        .find(|e| {
            e.kind == nostr_double_ratchet::INVITE_EVENT_KIND
                && e.pubkey == device_pubkey
                && is_d(e, &expected_invite_d)
        })
        .expect("Expected device Invite event to be published");
    assert!(device_invite_event
        .tags
        .iter()
        .any(|t| t.len() >= 2 && t[0] == "l" && t[1] == "double-ratchet/invites"));

    relay.stop().await;
}
