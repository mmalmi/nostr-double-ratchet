//! E2E test: ndr chat join should route private invites by claimed owner pubkey

mod common;

use tempfile::TempDir;

use nostr_double_ratchet::Invite;

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
async fn test_chat_join_private_invite_routes_chat_to_owner_pubkey() {
    let mut relay = common::WsRelay::new();
    let addr = relay.start().await.expect("Failed to start relay");
    let relay_url = format!("ws://{}", addr);

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

    let owner_keys = nostr::Keys::generate();
    let owner_pubkey_hex = owner_keys.public_key().to_hex();

    let inviter_device_keys = nostr::Keys::generate();
    let inviter_device_pubkey = inviter_device_keys.public_key();
    let inviter_device_hex = inviter_device_pubkey.to_hex();

    let mut invite = Invite::create_new(
        inviter_device_pubkey,
        Some(inviter_device_hex.clone()),
        None,
    )
    .unwrap();
    invite.owner_public_key = Some(owner_keys.public_key());
    let invite_url = invite.get_url("https://chat.iris.to").unwrap();

    let join = run_ndr(alice_dir.path(), &["chat", "join", &invite_url]).await;
    assert_eq!(join["status"], "ok");
    assert_eq!(join["data"]["their_pubkey"], owner_pubkey_hex);

    let storage = ndr::storage::Storage::open(alice_dir.path()).unwrap();
    let owner_chat = storage.get_chat_by_pubkey(&owner_pubkey_hex).unwrap();
    assert!(
        owner_chat.is_some(),
        "expected owner-pubkey chat to be created"
    );
    let device_chat = storage.get_chat_by_pubkey(&inviter_device_hex).unwrap();
    assert!(
        device_chat.is_none(),
        "invite should not create chat keyed by device pubkey when owner claim exists"
    );

    relay.stop().await;
}
