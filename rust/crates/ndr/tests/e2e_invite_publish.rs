//! E2E test: ndr publishes invite event to relays

mod common;

use tempfile::TempDir;
use tokio::time::{sleep, Duration};

/// Run ndr CLI command and return JSON output
async fn run_ndr(data_dir: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let output = tokio::process::Command::new(common::ndr_binary())
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

#[tokio::test]
async fn test_invite_publish_creates_event() {
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);

    let temp = TempDir::new().unwrap();
    let data_dir = temp.path();

    let config_content = serde_json::json!({
        "relays": [&relay_url]
    });
    std::fs::write(
        data_dir.join("config.json"),
        serde_json::to_string(&config_content).unwrap(),
    )
    .unwrap();

    let alice_sk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let login = run_ndr(data_dir, &["login", alice_sk]).await;
    assert_eq!(login["status"], "ok");

    let publish = run_ndr(
        data_dir,
        &[
            "invite",
            "publish",
            "--device-id",
            "test-device",
            "-l",
            "test",
        ],
    )
    .await;
    assert_eq!(publish["status"], "ok");

    // Allow relay to receive event
    sleep(Duration::from_millis(200)).await;

    let events = relay.events().await;
    let invite_event = events
        .iter()
        .find(|e| e.kind == nostr_double_ratchet::INVITE_EVENT_KIND)
        .expect("Expected invite event to be published");

    let sk = nostr::SecretKey::from_hex(alice_sk).unwrap();
    let keys = nostr::Keys::new(sk);
    let expected_pubkey = keys.public_key().to_hex();

    assert_eq!(invite_event.pubkey, expected_pubkey);

    let has_tag = |name: &str, value: &str| {
        invite_event
            .tags
            .iter()
            .any(|t| t.len() >= 2 && t[0] == name && t[1] == value)
    };

    assert!(has_tag("d", "double-ratchet/invites/test-device"));
    assert!(has_tag("l", "double-ratchet/invites"));
    assert!(invite_event
        .tags
        .iter()
        .any(|t| t.first().map(|s| s.as_str()) == Some("ephemeralKey")));
    assert!(invite_event
        .tags
        .iter()
        .any(|t| t.first().map(|s| s.as_str()) == Some("sharedSecret")));

    relay.stop().await;
}

#[tokio::test]
async fn test_invite_publish_defaults_device_id_to_pubkey() {
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);

    let temp = TempDir::new().unwrap();
    let data_dir = temp.path();

    let config_content = serde_json::json!({
        "relays": [&relay_url]
    });
    std::fs::write(
        data_dir.join("config.json"),
        serde_json::to_string(&config_content).unwrap(),
    )
    .unwrap();

    let alice_sk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let login = run_ndr(data_dir, &["login", alice_sk]).await;
    assert_eq!(login["status"], "ok");

    let publish = run_ndr(data_dir, &["invite", "publish", "-l", "test"]).await;
    assert_eq!(publish["status"], "ok");

    sleep(Duration::from_millis(200)).await;

    let events = relay.events().await;
    let invite_event = events
        .iter()
        .find(|e| e.kind == nostr_double_ratchet::INVITE_EVENT_KIND)
        .expect("Expected invite event to be published");

    let has_tag = |name: &str, value: &str| {
        invite_event
            .tags
            .iter()
            .any(|t| t.len() >= 2 && t[0] == name && t[1] == value)
    };

    let sk = nostr::SecretKey::from_hex(alice_sk).unwrap();
    let keys = nostr::Keys::new(sk);
    let expected_pubkey = keys.public_key().to_hex();
    assert!(has_tag(
        "d",
        &format!("double-ratchet/invites/{}", expected_pubkey)
    ));

    relay.stop().await;
}
