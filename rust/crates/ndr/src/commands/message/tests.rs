use super::*;
use crate::config::Config;
use crate::output::Output;
use crate::storage::Storage;
use crate::storage::StoredChat;
use nostr::JsonUtil;
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

fn create_test_session_pair() -> (
    nostr::Keys,
    nostr::Keys,
    nostr_double_ratchet::Session,
    nostr_double_ratchet::Session,
) {
    // Create an invite
    let alice_keys = nostr::Keys::generate();
    let bob_keys = nostr::Keys::generate();

    let invite =
        nostr_double_ratchet::Invite::create_new(alice_keys.public_key(), None, None).unwrap();

    // Bob accepts: Bob becomes the initiator (can send first).
    let bob_pk = bob_keys.public_key();
    let (bob_session, response) = invite
        .accept_with_owner(
            bob_pk,
            bob_keys.secret_key().to_secret_bytes(),
            None,
            Some(bob_pk),
        )
        .unwrap();

    // Alice processes response: Alice must receive first.
    let alice_session = invite
        .process_invite_response(&response, alice_keys.secret_key().to_secret_bytes())
        .unwrap()
        .unwrap()
        .session;

    (alice_keys, bob_keys, bob_session, alice_session)
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
    let their_pubkey = nostr::Keys::generate().public_key().to_hex();
    storage
        .save_chat(&StoredChat {
            id: "test-chat".to_string(),
            their_pubkey,
            created_at: 1234567890,
            last_message_at: None,
            session_state: session_state.clone(),
            message_ttl_seconds: None,
        })
        .unwrap();

    (temp, config, storage, session_state)
}

#[tokio::test]
async fn test_send_message() {
    let (_temp, config, storage, _) = setup();
    let output = Output::new(true);

    send(
        "test-chat",
        "Hello!",
        None,
        None,
        None,
        &config,
        &storage,
        &output,
    )
    .await
    .unwrap();

    let messages = storage.get_messages("test-chat", 10).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "Hello!");
    assert!(messages[0].is_outgoing);
}

#[tokio::test]
async fn test_send_message_with_ttl_adds_expiration_tag() {
    init_test_env();

    let temp = TempDir::new().unwrap();
    let storage = Storage::open(temp.path()).unwrap();

    let (alice_keys, bob_keys, bob_session, mut alice_session) = create_test_session_pair();

    // Configure CLI identity to match Bob (sender).
    let mut config = Config::load(temp.path()).unwrap();
    config
        .set_private_key(&bob_keys.secret_key().to_secret_hex())
        .unwrap();
    let config = Config::load(temp.path()).unwrap();

    let chat_id = "test-chat".to_string();
    let session_state = serde_json::to_string(&bob_session.state).unwrap();
    storage
        .save_chat(&StoredChat {
            id: chat_id.clone(),
            their_pubkey: alice_keys.public_key().to_hex(),
            created_at: 1234567890,
            last_message_at: None,
            session_state,
            message_ttl_seconds: None,
        })
        .unwrap();

    let ttl_seconds = 60u64;

    // Encrypt without touching the network so we can assert on the decrypted rumor.
    let prepared = super::send::prepare_send_message(
        &chat_id,
        "Hello expiring",
        None,
        Some(ttl_seconds),
        None,
        &config,
        &storage,
    )
    .await
    .unwrap();

    assert_eq!(
        prepared.stored_message.expires_at,
        Some(prepared.timestamp + ttl_seconds)
    );

    let plaintext = alice_session
        .receive(&prepared.encrypted_event)
        .unwrap()
        .unwrap();
    let rumor = nostr::UnsignedEvent::from_json(&plaintext).unwrap();

    let mut exp: Option<String> = None;
    for t in rumor.tags.iter() {
        let v = t.clone().to_vec();
        if v.first().map(|s| s.as_str()) == Some(nostr_double_ratchet::EXPIRATION_TAG) {
            exp = v.get(1).cloned();
        }
    }
    let expected = prepared.stored_message.expires_at.unwrap().to_string();
    assert_eq!(exp.as_deref(), Some(expected.as_str()));
}

#[tokio::test]
async fn test_chat_default_ttl_is_applied_when_sending_without_overrides() {
    init_test_env();

    let temp = TempDir::new().unwrap();
    let storage = Storage::open(temp.path()).unwrap();

    let (alice_keys, bob_keys, bob_session, mut alice_session) = create_test_session_pair();

    // Configure CLI identity to match Bob (sender).
    let mut config = Config::load(temp.path()).unwrap();
    config
        .set_private_key(&bob_keys.secret_key().to_secret_hex())
        .unwrap();
    let config = Config::load(temp.path()).unwrap();

    let chat_id = "test-chat".to_string();
    let session_state = serde_json::to_string(&bob_session.state).unwrap();
    let ttl_seconds = 90u64;
    storage
        .save_chat(&StoredChat {
            id: chat_id.clone(),
            their_pubkey: alice_keys.public_key().to_hex(),
            created_at: 1234567890,
            last_message_at: None,
            session_state,
            message_ttl_seconds: Some(ttl_seconds),
        })
        .unwrap();

    let prepared = super::send::prepare_send_message(
        &chat_id,
        "Hello default expiring",
        None,
        None,
        None,
        &config,
        &storage,
    )
    .await
    .unwrap();

    assert_eq!(
        prepared.stored_message.expires_at,
        Some(prepared.timestamp + ttl_seconds)
    );

    let plaintext = alice_session
        .receive(&prepared.encrypted_event)
        .unwrap()
        .unwrap();
    let rumor = nostr::UnsignedEvent::from_json(&plaintext).unwrap();

    let mut exp: Option<String> = None;
    for t in rumor.tags.iter() {
        let v = t.clone().to_vec();
        if v.first().map(|s| s.as_str()) == Some(nostr_double_ratchet::EXPIRATION_TAG) {
            exp = v.get(1).cloned();
        }
    }
    let expected = prepared.stored_message.expires_at.unwrap().to_string();
    assert_eq!(exp.as_deref(), Some(expected.as_str()));
}

#[tokio::test]
async fn test_read_messages() {
    let (_temp, config, storage, _) = setup();
    let output = Output::new(true);

    send(
        "test-chat",
        "One",
        None,
        None,
        None,
        &config,
        &storage,
        &output,
    )
    .await
    .unwrap();
    send(
        "test-chat",
        "Two",
        None,
        None,
        None,
        &config,
        &storage,
        &output,
    )
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

    send(
        "test-chat",
        "Hello!",
        None,
        None,
        None,
        &config,
        &storage,
        &output,
    )
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
            message_ttl_seconds: None,
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
            message_ttl_seconds: None,
        })
        .unwrap();

    let chat = resolve_target(&npub, &storage).unwrap();
    assert_eq!(chat.id, "npub-chat");
}

#[test]
fn test_resolve_target_by_chat_link_hash_npub() {
    let (_temp, _config, storage, _) = setup();
    let keys = nostr::Keys::generate();
    let pubkey_hex = keys.public_key().to_hex();
    let npub = nostr::ToBech32::to_bech32(&keys.public_key()).unwrap();

    let session = create_test_session();
    let session_state = serde_json::to_string(&session.state).unwrap();
    storage
        .save_chat(&StoredChat {
            id: "link-chat".to_string(),
            their_pubkey: pubkey_hex.clone(),
            created_at: 1234567890,
            last_message_at: None,
            session_state,
            message_ttl_seconds: None,
        })
        .unwrap();

    let link = format!("https://chat.iris.to/#{}", npub);
    let chat = resolve_target(&link, &storage).unwrap();
    assert_eq!(chat.id, "link-chat");

    let link_slash = format!("https://chat.iris.to/#/{}", npub);
    let chat = resolve_target(&link_slash, &storage).unwrap();
    assert_eq!(chat.id, "link-chat");
}

#[test]
fn test_resolve_target_pubkey_accepts_chat_link_hash_npub() {
    let (_temp, _config, storage, _) = setup();
    let keys = nostr::Keys::generate();
    let pubkey_hex = keys.public_key().to_hex();
    let npub = nostr::ToBech32::to_bech32(&keys.public_key()).unwrap();

    let link = format!("https://chat.iris.to/#{}", npub);
    let resolved = super::common::resolve_target_pubkey(&link, &storage).unwrap();
    assert_eq!(resolved, pubkey_hex);

    let nostr_uri = format!("nostr:{}", npub);
    let resolved = super::common::resolve_target_pubkey(&nostr_uri, &storage).unwrap();
    assert_eq!(resolved, pubkey_hex);
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
            message_ttl_seconds: None,
        })
        .unwrap();
    storage
        .save_chat(&StoredChat {
            id: "new-chat".to_string(),
            their_pubkey: pubkey_hex.clone(),
            created_at: 1000,
            last_message_at: Some(5000),
            session_state: serde_json::to_string(&session2.state).unwrap(),
            message_ttl_seconds: None,
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
            message_ttl_seconds: None,
        })
        .unwrap();

    storage.add_contact(&npub, "alice").unwrap();
    let chat = resolve_target("alice", &storage).unwrap();
    assert_eq!(chat.id, "pet-chat");
}
