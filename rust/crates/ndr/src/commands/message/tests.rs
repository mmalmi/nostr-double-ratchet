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
            device_id: None,
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
    init_test_env();
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
async fn test_send_stores_inner_rumor_id_as_message_id() {
    init_test_env();
    let (_temp, config, storage, _) = setup();

    let sent = super::send::send_message_impl(
        "test-chat",
        "Hello inner id!",
        None,
        None,
        None,
        &config,
        &storage,
    )
    .await
    .unwrap();

    assert!(!sent.id.is_empty());
    assert_eq!(sent.id, sent.inner_message_id);
    assert!(!sent.event_ids.is_empty());
    assert!(!sent.event_ids.contains(&sent.inner_message_id));

    let messages = storage.get_messages("test-chat", 10).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].id, sent.inner_message_id);
    assert_eq!(messages[0].content, "Hello inner id!");
    assert!(messages[0].is_outgoing);
}

#[tokio::test]
async fn test_send_fans_out_to_all_known_recipient_devices_in_session_manager() {
    init_test_env();

    let temp = TempDir::new().unwrap();
    let storage = Storage::open(temp.path()).unwrap();

    let sender_keys = nostr::Keys::generate();
    let our_device_id = sender_keys.public_key().to_hex();

    let mut config = Config::load(temp.path()).unwrap();
    config
        .set_private_key(&sender_keys.secret_key().to_secret_hex())
        .unwrap();
    let config = Config::load(temp.path()).unwrap();
    let output = Output::new(true);

    let recipient_owner_keys = nostr::Keys::generate();
    let recipient_device1_keys = nostr::Keys::generate();
    let recipient_device2_keys = nostr::Keys::generate();
    let recipient_owner = recipient_owner_keys.public_key();
    let device1_id = recipient_device1_keys.public_key().to_hex();
    let device2_id = recipient_device2_keys.public_key().to_hex();

    // Build two sender-side sessions for two recipient devices under the same owner.
    let invite1 = nostr_double_ratchet::Invite::create_new(
        recipient_device1_keys.public_key(),
        Some(device1_id.clone()),
        None,
    )
    .unwrap();
    let (session1, _) = invite1
        .accept_with_owner(
            sender_keys.public_key(),
            sender_keys.secret_key().to_secret_bytes(),
            Some(our_device_id.clone()),
            Some(sender_keys.public_key()),
        )
        .unwrap();

    let invite2 = nostr_double_ratchet::Invite::create_new(
        recipient_device2_keys.public_key(),
        Some(device2_id.clone()),
        None,
    )
    .unwrap();
    let (session2, _) = invite2
        .accept_with_owner(
            sender_keys.public_key(),
            sender_keys.secret_key().to_secret_bytes(),
            Some(our_device_id.clone()),
            Some(sender_keys.public_key()),
        )
        .unwrap();

    // Legacy chat storage tracks a single selected device session.
    storage
        .save_chat(&StoredChat {
            id: "peer-chat".to_string(),
            their_pubkey: recipient_owner.to_hex(),
            device_id: Some(device1_id.clone()),
            created_at: 1234567890,
            last_message_at: None,
            session_state: serde_json::to_string(&session1.state).unwrap(),
            message_ttl_seconds: None,
        })
        .unwrap();

    // Persist SessionManager state with both recipient devices + AppKeys authorization.
    {
        let session_manager_store: std::sync::Arc<dyn nostr_double_ratchet::StorageAdapter> =
            std::sync::Arc::new(
                nostr_double_ratchet::FileStorageAdapter::new(
                    storage.data_dir().join("session_manager"),
                )
                .unwrap(),
            );
        let (sm_tx, _sm_rx) = crossbeam_channel::unbounded();
        let manager = nostr_double_ratchet::SessionManager::new(
            sender_keys.public_key(),
            sender_keys.secret_key().to_secret_bytes(),
            our_device_id.clone(),
            sender_keys.public_key(),
            sm_tx,
            Some(session_manager_store),
            None,
        );
        manager.init().unwrap();
        manager
            .import_session_state(
                recipient_owner,
                Some(device1_id.clone()),
                session1.state.clone(),
            )
            .unwrap();
        manager
            .import_session_state(
                recipient_owner,
                Some(device2_id.clone()),
                session2.state.clone(),
            )
            .unwrap();

        let mut app_keys = nostr_double_ratchet::AppKeys::new(Vec::new());
        app_keys.add_device(nostr_double_ratchet::DeviceEntry::new(
            recipient_device1_keys.public_key(),
            1,
        ));
        app_keys.add_device(nostr_double_ratchet::DeviceEntry::new(
            recipient_device2_keys.public_key(),
            1,
        ));
        let app_keys_event = app_keys
            .get_event(recipient_owner)
            .sign_with_keys(&recipient_owner_keys)
            .unwrap();
        manager.process_received_event(app_keys_event);
    }

    send(
        "peer-chat",
        "fanout",
        None,
        None,
        None,
        &config,
        &storage,
        &output,
    )
    .await
    .unwrap();

    let session_manager_store: std::sync::Arc<dyn nostr_double_ratchet::StorageAdapter> =
        std::sync::Arc::new(
            nostr_double_ratchet::FileStorageAdapter::new(
                storage.data_dir().join("session_manager"),
            )
            .unwrap(),
        );
    let (sm_tx, _sm_rx) = crossbeam_channel::unbounded();
    let manager = nostr_double_ratchet::SessionManager::new(
        sender_keys.public_key(),
        sender_keys.secret_key().to_secret_bytes(),
        our_device_id,
        sender_keys.public_key(),
        sm_tx,
        Some(session_manager_store),
        None,
    );
    manager.init().unwrap();

    let mut recipient_sends: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    for (owner, device_id, state) in manager.export_active_sessions() {
        if owner == recipient_owner {
            recipient_sends.insert(device_id, state.sending_chain_message_number);
        }
    }

    assert_eq!(recipient_sends.get(&device1_id), Some(&1));
    assert_eq!(recipient_sends.get(&device2_id), Some(&1));
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
            device_id: None,
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
            device_id: None,
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

#[tokio::test]
async fn test_receive_typing_does_not_save_message() {
    init_test_env();

    let temp = TempDir::new().unwrap();
    let storage = Storage::open(temp.path()).unwrap();
    let output = Output::new(true);

    let (_alice_keys, bob_keys, mut bob_session, alice_session) = create_test_session_pair();

    let chat_id = "peer-chat".to_string();
    let session_state = serde_json::to_string(&alice_session.state).unwrap();
    storage
        .save_chat(&StoredChat {
            id: chat_id.clone(),
            their_pubkey: bob_keys.public_key().to_hex(),
            device_id: None,
            created_at: 1234567890,
            last_message_at: None,
            session_state,
            message_ttl_seconds: None,
        })
        .unwrap();

    let typing_event = bob_session.send_typing().unwrap();
    super::receive::receive(&typing_event.as_json(), &storage, &output)
        .await
        .unwrap();

    let messages = storage.get_messages(&chat_id, 10).unwrap();
    assert_eq!(messages.len(), 0);

    let chat = storage.get_chat(&chat_id).unwrap().unwrap();
    assert!(chat.last_message_at.is_none());
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
            device_id: None,
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
            device_id: None,
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
            device_id: None,
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
            device_id: None,
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
            device_id: None,
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
            device_id: None,
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

fn build_rumor_json(
    sender_owner_hex: &str,
    kind: u32,
    content: &str,
    tags: Vec<Vec<String>>,
) -> String {
    serde_json::json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "pubkey": sender_owner_hex,
        "created_at": 0,
        "kind": kind,
        "tags": tags,
        "content": content,
    })
    .to_string()
}

#[test]
fn test_session_manager_decrypted_incoming_from_same_owner_routes_to_single_chat() {
    init_test_env();

    let temp = TempDir::new().unwrap();
    let mut config = Config::load(temp.path()).unwrap();
    let me = nostr::Keys::generate();
    config
        .set_private_key(&me.secret_key().to_secret_hex())
        .unwrap();
    let config = Config::load(temp.path()).unwrap();
    let output = Output::new(true);
    let storage = Storage::open(temp.path()).unwrap();

    let peer_owner = nostr::Keys::generate().public_key().to_hex();
    storage
        .save_chat(&StoredChat {
            id: "peer-chat".to_string(),
            their_pubkey: peer_owner.clone(),
            device_id: None,
            created_at: 1000,
            last_message_at: None,
            session_state: "{}".to_string(),
            message_ttl_seconds: None,
        })
        .unwrap();

    let rumor1 = build_rumor_json(&peer_owner, 14, "hi from peer device 1", vec![]);
    let rumor2 = build_rumor_json(&peer_owner, 14, "hi from peer device 2", vec![]);

    let handled1 = super::listen::apply_session_manager_one_to_one_decrypted(
        nostr::PublicKey::from_hex(&peer_owner).unwrap(),
        &rumor1,
        Some("outer-1"),
        2000,
        &config,
        &storage,
        &output,
    )
    .unwrap();
    let handled2 = super::listen::apply_session_manager_one_to_one_decrypted(
        nostr::PublicKey::from_hex(&peer_owner).unwrap(),
        &rumor2,
        Some("outer-2"),
        2001,
        &config,
        &storage,
        &output,
    )
    .unwrap();

    assert!(handled1);
    assert!(handled2);

    let messages = storage.get_messages("peer-chat", 10).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].chat_id, "peer-chat");
    assert_eq!(messages[1].chat_id, "peer-chat");
    assert!(!messages[0].is_outgoing);
    assert!(!messages[1].is_outgoing);
}

#[test]
fn test_session_manager_decrypted_sibling_copy_routes_to_peer_chat_as_outgoing() {
    init_test_env();

    let temp = TempDir::new().unwrap();
    let mut config = Config::load(temp.path()).unwrap();
    let me = nostr::Keys::generate();
    let my_owner_hex = me.public_key().to_hex();
    config
        .set_private_key(&me.secret_key().to_secret_hex())
        .unwrap();
    let config = Config::load(temp.path()).unwrap();
    let output = Output::new(true);
    let storage = Storage::open(temp.path()).unwrap();

    let peer_owner = nostr::Keys::generate().public_key().to_hex();
    storage
        .save_chat(&StoredChat {
            id: "peer-chat".to_string(),
            their_pubkey: peer_owner.clone(),
            device_id: None,
            created_at: 1000,
            last_message_at: None,
            session_state: "{}".to_string(),
            message_ttl_seconds: None,
        })
        .unwrap();
    storage
        .save_chat(&StoredChat {
            id: "self-chat".to_string(),
            their_pubkey: my_owner_hex.clone(),
            device_id: Some("my-other-device".to_string()),
            created_at: 1000,
            last_message_at: None,
            session_state: "{}".to_string(),
            message_ttl_seconds: None,
        })
        .unwrap();

    let rumor = build_rumor_json(
        &my_owner_hex,
        14,
        "hello from sibling device",
        vec![vec!["p".to_string(), peer_owner.clone()]],
    );

    let handled = super::listen::apply_session_manager_one_to_one_decrypted(
        nostr::PublicKey::from_hex(&my_owner_hex).unwrap(),
        &rumor,
        Some("outer-self-1"),
        3000,
        &config,
        &storage,
        &output,
    )
    .unwrap();
    assert!(handled);

    let peer_messages = storage.get_messages("peer-chat", 10).unwrap();
    assert_eq!(peer_messages.len(), 1);
    assert!(peer_messages[0].is_outgoing);
    assert_eq!(peer_messages[0].chat_id, "peer-chat");

    let self_messages = storage.get_messages("self-chat", 10).unwrap();
    assert!(self_messages.is_empty());
}
