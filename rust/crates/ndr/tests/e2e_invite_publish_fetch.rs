//! E2E test: Alice publishes invite to relay, Bob fetches and parses it

mod common;

use tempfile::TempDir;
use tokio::time::{sleep, Duration};

use nostr_double_ratchet::INVITE_EVENT_KIND;

/// Run ndr CLI command and return JSON output
async fn run_ndr(data_dir: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let output = tokio::process::Command::new("cargo")
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

#[tokio::test]
async fn test_publish_then_fetch_invite() {
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);

    // Alice setup
    let alice_dir = TempDir::new().unwrap();
    let alice_sk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    std::fs::write(
        alice_dir.path().join("config.json"),
        serde_json::to_string(&serde_json::json!({ "relays": [&relay_url] })).unwrap(),
    ).unwrap();
    let login = run_ndr(alice_dir.path(), &["login", alice_sk]).await;
    assert_eq!(login["status"], "ok");

    // Alice publishes invite
    let publish = run_ndr(alice_dir.path(), &["invite", "publish", "-l", "test"]).await;
    assert_eq!(publish["status"], "ok");
    let published_url = publish["data"]["url"].as_str().expect("Expected invite URL");

    sleep(Duration::from_millis(300)).await;

    // Bob fetches the invite event from relay using nostr-sdk
    let alice_sk_key = nostr::SecretKey::from_hex(alice_sk).unwrap();
    let alice_keys = nostr::Keys::new(alice_sk_key);
    let alice_pubkey = alice_keys.public_key();

    let bob_client = nostr_sdk::Client::default();
    bob_client.add_relay(&relay_url).await.unwrap();
    bob_client.connect().await;

    let filter = nostr_sdk::Filter::new()
        .kind(nostr::Kind::Custom(INVITE_EVENT_KIND as u16))
        .author(alice_pubkey)
        .limit(10);

    let events = bob_client
        .fetch_events(vec![filter], Some(Duration::from_secs(5)))
        .await
        .expect("Failed to fetch events");

    assert!(!events.is_empty(), "Expected at least one invite event from relay");

    // Parse invite from the fetched event
    let invite_event = events
        .iter()
        .find(|e| e.kind.as_u16() == INVITE_EVENT_KIND as u16)
        .expect("Expected invite event");

    let invite = nostr_double_ratchet::Invite::from_event(invite_event)
        .expect("Failed to parse invite from fetched event");

    // Verify invite matches what was published
    assert_eq!(invite.inviter.to_hex(), alice_pubkey.to_hex());
    let fetched_url = invite.get_url("https://iris.to").expect("Failed to get URL from fetched invite");
    assert_eq!(fetched_url, published_url, "Fetched invite URL should match published URL");

    // Bob accepts the invite and establishes a session
    let bob_sk = "1111111111111111111111111111111111111111111111111111111111111111";
    let bob_sk_key = nostr::SecretKey::from_hex(bob_sk).unwrap();
    let bob_keys = nostr::Keys::new(bob_sk_key.clone());
    let bob_pubkey = bob_keys.public_key();

    let (session, response_event) = invite
        .accept(bob_pubkey, bob_sk_key.secret_bytes(), None)
        .expect("Failed to accept invite");

    // Verify session was created (root_key is populated after key agreement)
    assert!(!session.state.root_key.is_empty(), "Session root key should not be empty");

    // Response event is already signed by accept()
    bob_client.send_event(response_event).await.expect("Failed to publish response");

    sleep(Duration::from_millis(200)).await;

    // Verify response event hit the relay
    let relay_events = relay.events().await;
    let has_response = relay_events.iter().any(|e| e.kind == nostr_double_ratchet::INVITE_RESPONSE_KIND);
    assert!(has_response, "Expected invite response event on relay");

    relay.stop().await;
}
