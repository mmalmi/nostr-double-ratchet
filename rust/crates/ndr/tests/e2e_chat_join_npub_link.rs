//! E2E test: ndr chat join accepts chat.iris.to/#npub... style chat links

mod common;

use tempfile::TempDir;
use tokio::time::{sleep, Duration};

use nostr_double_ratchet::{Invite, INVITE_EVENT_KIND, INVITE_RESPONSE_KIND, MESSAGE_EVENT_KIND};

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

    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("Failed to parse ndr output: {}\nOutput: {}", e, stdout))
}

#[tokio::test]
async fn test_chat_join_accepts_npub_link_and_creates_chat_without_sending_message() {
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);

    // Bob publishes a public invite event.
    let bob_sk = "1111111111111111111111111111111111111111111111111111111111111111";
    let bob_sk = nostr::SecretKey::from_hex(bob_sk).unwrap();
    let bob_keys = nostr::Keys::new(bob_sk);
    let bob_pubkey_hex = bob_keys.public_key().to_hex();
    let bob_npub = nostr::ToBech32::to_bech32(&bob_keys.public_key()).unwrap();

    let mut public_invite =
        Invite::create_new(bob_keys.public_key(), Some("public".to_string()), None).unwrap();
    public_invite.created_at = 1;
    let public_event = public_invite
        .get_event()
        .unwrap()
        .sign_with_keys(&bob_keys)
        .unwrap();

    let client = nostr_sdk::Client::default();
    client.add_relay(&relay_url).await.unwrap();
    client.connect().await;
    client.send_event(public_event).await.unwrap();

    sleep(Duration::from_millis(200)).await;

    // Alice config
    let alice_dir = TempDir::new().unwrap();
    let config_content = serde_json::json!({
        "relays": [&relay_url]
    });
    std::fs::write(
        alice_dir.path().join("config.json"),
        serde_json::to_string(&config_content).unwrap(),
    )
    .unwrap();

    let alice_sk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let login = run_ndr(alice_dir.path(), &["login", alice_sk]).await;
    assert_eq!(login["status"], "ok");

    let link = format!("https://chat.iris.to/#{}", bob_npub);
    let join = run_ndr(alice_dir.path(), &["chat", "join", &link]).await;
    assert_eq!(join["status"], "ok");
    assert_eq!(join["data"]["their_pubkey"], bob_pubkey_hex);

    sleep(Duration::from_millis(200)).await;

    // Verify relay saw invite + response, but no message.
    let events = relay.events().await;
    assert!(events.iter().any(|e| e.kind == INVITE_EVENT_KIND));
    assert!(events.iter().any(|e| e.kind == INVITE_RESPONSE_KIND));
    assert!(
        !events.iter().any(|e| e.kind == MESSAGE_EVENT_KIND),
        "Expected no message events when only joining"
    );

    // Verify Alice stored the chat.
    let storage = ndr::storage::Storage::open(alice_dir.path()).unwrap();
    let chat = storage.get_chat_by_pubkey(&bob_pubkey_hex).unwrap();
    assert!(chat.is_some(), "Expected chat to be created");

    relay.stop().await;
}
