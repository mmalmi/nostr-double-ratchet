//! E2E test: Alice publishes invite, Bob fetches from relay, both exchange messages

mod common;

use tempfile::TempDir;
use tokio::time::{sleep, Duration};

use nostr_double_ratchet::{Invite, INVITE_EVENT_KIND};

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
async fn test_publish_fetch_and_message_both_ways() {
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);

    // Alice setup — publishes invite via ndr CLI
    let alice_dir = TempDir::new().unwrap();
    let alice_sk = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    std::fs::write(
        alice_dir.path().join("config.json"),
        serde_json::to_string(&serde_json::json!({ "relays": [&relay_url] })).unwrap(),
    ).unwrap();
    let login = run_ndr(alice_dir.path(), &["login", alice_sk]).await;
    assert_eq!(login["status"], "ok");

    let publish = run_ndr(alice_dir.path(), &["invite", "publish", "-l", "test"]).await;
    assert_eq!(publish["status"], "ok");
    let published_url = publish["data"]["url"].as_str().expect("Expected invite URL");

    sleep(Duration::from_millis(300)).await;

    // Bob fetches invite from relay
    let alice_sk_key = nostr::SecretKey::from_hex(alice_sk).unwrap();
    let alice_keys = nostr::Keys::new(alice_sk_key.clone());
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

    let invite_event = events
        .iter()
        .find(|e| e.kind.as_u16() == INVITE_EVENT_KIND as u16)
        .expect("Expected invite event");

    let invite = Invite::from_event(invite_event)
        .expect("Failed to parse invite from fetched event");

    assert_eq!(invite.inviter.to_hex(), alice_pubkey.to_hex());
    let fetched_url = invite.get_url("https://iris.to").unwrap();
    assert_eq!(fetched_url, published_url);

    // Bob accepts invite
    let bob_sk = "1111111111111111111111111111111111111111111111111111111111111111";
    let bob_sk_key = nostr::SecretKey::from_hex(bob_sk).unwrap();
    let bob_keys = nostr::Keys::new(bob_sk_key.clone());
    let bob_pubkey = bob_keys.public_key();

    let (mut bob_session, response_event) = invite
        .accept(bob_pubkey, bob_sk_key.secret_bytes(), None)
        .expect("Failed to accept invite");

    // Publish response to relay
    bob_client.send_event(response_event.clone()).await.expect("Failed to publish response");
    sleep(Duration::from_millis(300)).await;

    // Alice loads her stored invite and processes Bob's response to get her session
    let alice_storage = ndr::storage::Storage::open(alice_dir.path()).unwrap();
    let stored_invites = alice_storage.list_invites().unwrap();
    assert!(!stored_invites.is_empty(), "Alice should have a stored invite");

    let stored = &stored_invites[0];
    let alice_invite = Invite::deserialize(&stored.serialized)
        .expect("Failed to deserialize Alice's stored invite");

    let (mut alice_session, _bob_identity, _device_id) = alice_invite
        .process_invite_response(&response_event, alice_sk_key.secret_bytes())
        .expect("Failed to process invite response")
        .expect("Expected session from invite response");

    // Helper to extract content from decrypted inner event JSON
    fn extract_content(decrypted: Option<String>) -> String {
        let json_str = decrypted.expect("Expected decrypted message");
        let v: serde_json::Value = serde_json::from_str(&json_str).expect("Expected valid JSON");
        v["content"].as_str().expect("Expected content field").to_string()
    }

    // Bob sends a message to Alice
    let bob_msg_event = bob_session.send("hello from bob".to_string())
        .expect("Bob failed to send message");
    let decrypted = alice_session.receive(&bob_msg_event)
        .expect("Alice failed to decrypt Bob's message");
    assert_eq!(extract_content(decrypted), "hello from bob");

    // Alice sends a message back to Bob
    let alice_msg_event = alice_session.send("hello from alice".to_string())
        .expect("Alice failed to send message");
    let decrypted = bob_session.receive(&alice_msg_event)
        .expect("Bob failed to decrypt Alice's message");
    assert_eq!(extract_content(decrypted), "hello from alice");

    // Another round to verify ratchet continues working
    let bob_msg2 = bob_session.send("second message from bob".to_string())
        .expect("Bob failed to send second message");
    let decrypted = alice_session.receive(&bob_msg2)
        .expect("Alice failed to decrypt Bob's second message");
    assert_eq!(extract_content(decrypted), "second message from bob");

    relay.stop().await;
}
