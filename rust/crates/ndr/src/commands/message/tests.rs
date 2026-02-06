use super::*;
use crate::config::Config;
use crate::output::Output;
use crate::storage::Storage;
use crate::storage::StoredChat;
use std::sync::Once;
use tempfile::TempDir;

fn create_test_session() -> nostr_double_ratchet::Session {
    // Create an invite
    let alice_keys = nostr::Keys::generate();
    let bob_keys = nostr::Keys::generate();

    let invite =
        nostr_double_ratchet::Invite::create_new(alice_keys.public_key(), None, None).unwrap();

    // Bob accepts the invite - this creates a session where Bob can send
    let bob_pk = bob_keys.public_key();
    let (bob_session, _response) = invite
        .accept_with_owner(
            bob_pk,
            bob_keys.secret_key().to_secret_bytes(),
            None,
            Some(bob_pk),
        )
        .unwrap();

    bob_session
}

fn init_test_env() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        std::env::set_var("NDR_IGNORE_PUBLISH_ERRORS", "1");
        std::env::set_var("NOSTR_PREFER_LOCAL", "0");
    });
}

fn setup() -> (TempDir, Config, Storage, String) {
    init_test_env();
    let temp = TempDir::new().unwrap();
    let mut config = Config::load(temp.path()).unwrap();
    config
        .set_private_key("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        .unwrap();
    let config = Config::load(temp.path()).unwrap();
    let storage = Storage::open(temp.path()).unwrap();

    // Create a proper test session
    let session = create_test_session();
    let session_state = serde_json::to_string(&session.state).unwrap();

    // Create a test chat with valid session
    storage
        .save_chat(&StoredChat {
            id: "test-chat".to_string(),
            their_pubkey: "abc123".to_string(),
            created_at: 1234567890,
            last_message_at: None,
            session_state: session_state.clone(),
        })
        .unwrap();

    (temp, config, storage, session_state)
}

#[tokio::test]
async fn test_send_message() {
    let (_temp, config, storage, _) = setup();
    let output = Output::new(true);

    send("test-chat", "Hello!", None, &config, &storage, &output)
        .await
        .unwrap();

    let messages = storage.get_messages("test-chat", 10).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "Hello!");
    assert!(messages[0].is_outgoing);
}

#[tokio::test]
async fn test_read_messages() {
    let (_temp, config, storage, _) = setup();
    let output = Output::new(true);

    send("test-chat", "One", None, &config, &storage, &output)
        .await
        .unwrap();
    send("test-chat", "Two", None, &config, &storage, &output)
        .await
        .unwrap();

    read("test-chat", 10, &storage, &output).await.unwrap();
}

#[tokio::test]
async fn test_send_updates_last_message_at() {
    let (_temp, config, storage, _) = setup();
    let output = Output::new(true);

    let before = storage.get_chat("test-chat").unwrap().unwrap();
    assert!(before.last_message_at.is_none());

    send("test-chat", "Hello!", None, &config, &storage, &output)
        .await
        .unwrap();

    let after = storage.get_chat("test-chat").unwrap().unwrap();
    assert!(after.last_message_at.is_some());
}

#[test]
fn test_resolve_target_by_chat_id() {
    let (_temp, _config, storage, _) = setup();
    let chat = resolve_target("test-chat", &storage).unwrap();
    assert_eq!(chat.id, "test-chat");
}

#[test]
fn test_resolve_target_by_hex_pubkey() {
    let (_temp, _config, storage, _) = setup();
    let keys = nostr::Keys::generate();
    let pubkey_hex = keys.public_key().to_hex();

    // Create a chat with this pubkey
    let session = create_test_session();
    let session_state = serde_json::to_string(&session.state).unwrap();
    storage
        .save_chat(&StoredChat {
            id: "pk-chat".to_string(),
            their_pubkey: pubkey_hex.clone(),
            created_at: 1234567890,
            last_message_at: None,
            session_state,
        })
        .unwrap();

    let chat = resolve_target(&pubkey_hex, &storage).unwrap();
    assert_eq!(chat.id, "pk-chat");
}

#[test]
fn test_resolve_target_by_npub() {
    let (_temp, _config, storage, _) = setup();
    let keys = nostr::Keys::generate();
    let pubkey_hex = keys.public_key().to_hex();
    let npub = nostr::ToBech32::to_bech32(&keys.public_key()).unwrap();

    let session = create_test_session();
    let session_state = serde_json::to_string(&session.state).unwrap();
    storage
        .save_chat(&StoredChat {
            id: "npub-chat".to_string(),
            their_pubkey: pubkey_hex,
            created_at: 1234567890,
            last_message_at: None,
            session_state,
        })
        .unwrap();

    let chat = resolve_target(&npub, &storage).unwrap();
    assert_eq!(chat.id, "npub-chat");
}

#[test]
fn test_resolve_target_not_found() {
    let (_temp, _config, storage, _) = setup();
    assert!(resolve_target("nonexistent", &storage).is_err());
}

#[test]
fn test_resolve_target_prefers_recent() {
    let (_temp, _config, storage, _) = setup();
    let keys = nostr::Keys::generate();
    let pubkey_hex = keys.public_key().to_hex();

    let session1 = create_test_session();
    let session2 = create_test_session();
    storage
        .save_chat(&StoredChat {
            id: "old-chat".to_string(),
            their_pubkey: pubkey_hex.clone(),
            created_at: 1000,
            last_message_at: Some(2000),
            session_state: serde_json::to_string(&session1.state).unwrap(),
        })
        .unwrap();
    storage
        .save_chat(&StoredChat {
            id: "new-chat".to_string(),
            their_pubkey: pubkey_hex.clone(),
            created_at: 1000,
            last_message_at: Some(5000),
            session_state: serde_json::to_string(&session2.state).unwrap(),
        })
        .unwrap();

    let chat = resolve_target(&pubkey_hex, &storage).unwrap();
    assert_eq!(chat.id, "new-chat");
}

#[test]
fn test_resolve_target_by_petname() {
    let (_temp, _config, storage, _) = setup();
    let keys = nostr::Keys::generate();
    let pubkey_hex = keys.public_key().to_hex();
    let npub = nostr::ToBech32::to_bech32(&keys.public_key()).unwrap();

    let session = create_test_session();
    let session_state = serde_json::to_string(&session.state).unwrap();
    storage
        .save_chat(&StoredChat {
            id: "pet-chat".to_string(),
            their_pubkey: pubkey_hex,
            created_at: 1234567890,
            last_message_at: None,
            session_state,
        })
        .unwrap();

    storage.add_contact(&npub, "alice").unwrap();
    let chat = resolve_target("alice", &storage).unwrap();
    assert_eq!(chat.id, "pet-chat");
}
