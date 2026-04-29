use super::*;
use nostr::Keys;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct FailFirstMessageQueuePutStorage {
    inner: Arc<dyn StorageAdapter>,
    failed: Arc<Mutex<bool>>,
}

impl FailFirstMessageQueuePutStorage {
    fn new(inner: Arc<dyn StorageAdapter>) -> Self {
        Self {
            inner,
            failed: Arc::new(Mutex::new(false)),
        }
    }
}

impl StorageAdapter for FailFirstMessageQueuePutStorage {
    fn get(&self, key: &str) -> Result<Option<String>> {
        self.inner.get(key)
    }

    fn put(&self, key: &str, value: String) -> Result<()> {
        if key.starts_with("v1/message-queue/") {
            let mut failed = self.failed.lock().unwrap();
            if !*failed {
                *failed = true;
                return Err(crate::Error::Storage(
                    "injected message-queue put failure".to_string(),
                ));
            }
        }
        self.inner.put(key, value)
    }

    fn del(&self, key: &str) -> Result<()> {
        self.inner.del(key)
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        self.inner.list(prefix)
    }
}

fn count_queue_entries(
    storage: &Arc<dyn StorageAdapter>,
    prefix: &str,
    target_key: &str,
    event_id: &str,
) -> usize {
    let mut count = 0usize;
    let keys = storage.list(prefix).unwrap();
    for key in keys {
        let Some(raw) = storage.get(&key).unwrap() else {
            continue;
        };
        let Ok(entry) = serde_json::from_str::<crate::QueueEntry>(&raw) else {
            continue;
        };
        if entry.target_key == target_key
            && entry.event.id.as_ref().map(|id| id.to_string()) == Some(event_id.to_string())
        {
            count += 1;
        }
    }
    count
}

fn drain_events(rx: &crossbeam_channel::Receiver<SessionManagerEvent>) -> Vec<SessionManagerEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

fn is_message_publish(event: &SessionManagerEvent) -> bool {
    matches!(
        event,
        SessionManagerEvent::PublishSigned(signed)
            | SessionManagerEvent::PublishSignedForInnerEvent {
                event: signed,
                ..
            } if signed.kind.as_u16() == crate::MESSAGE_EVENT_KIND as u16
    )
}

fn queued_publish_inner_event_id(event: &SessionManagerEvent) -> Option<&str> {
    match event {
        SessionManagerEvent::PublishSignedForInnerEvent {
            event,
            inner_event_id,
        } if event.kind.as_u16() == crate::MESSAGE_EVENT_KIND as u16 => inner_event_id.as_deref(),
        _ => None,
    }
}

fn sign_app_keys_event_with_created_at(
    app_keys: &AppKeys,
    owner_pubkey: PublicKey,
    owner_keys: &Keys,
    created_at: u64,
) -> nostr::Event {
    let mut tags = Vec::new();
    tags.push(
        nostr::Tag::parse(&["d".to_string(), "double-ratchet/app-keys".to_string()]).unwrap(),
    );
    tags.push(nostr::Tag::parse(&["version".to_string(), "1".to_string()]).unwrap());
    for device in app_keys.get_all_devices() {
        tags.push(
            nostr::Tag::parse(&[
                "device".to_string(),
                hex::encode(device.identity_pubkey.to_bytes()),
                device.created_at.to_string(),
            ])
            .unwrap(),
        );
    }

    nostr::EventBuilder::new(nostr::Kind::from(crate::APP_KEYS_EVENT_KIND as u16), "")
        .tags(tags)
        .custom_created_at(nostr::Timestamp::from(created_at))
        .build(owner_pubkey)
        .sign_with_keys(owner_keys)
        .unwrap()
}

fn test_session_state() -> crate::SessionState {
    let their_current = Keys::generate().public_key();
    let their_next = Keys::generate().public_key();
    let our_current = Keys::generate();
    let our_next = Keys::generate();

    crate::SessionState {
        root_key: [0u8; 32],
        their_current_nostr_public_key: Some(their_current),
        their_next_nostr_public_key: Some(their_next),
        our_current_nostr_key: Some(crate::SerializableKeyPair {
            public_key: our_current.public_key(),
            private_key: our_current.secret_key().to_secret_bytes(),
        }),
        our_next_nostr_key: crate::SerializableKeyPair {
            public_key: our_next.public_key(),
            private_key: our_next.secret_key().to_secret_bytes(),
        },
        receiving_chain_key: Some([1u8; 32]),
        sending_chain_key: Some([2u8; 32]),
        sending_chain_message_number: 0,
        receiving_chain_message_number: 0,
        previous_sending_chain_message_count: 0,
        skipped_keys: HashMap::new(),
    }
}

#[test]
fn merge_stored_user_record_keeps_more_advanced_existing_session() {
    let mut stale_state = test_session_state();
    stale_state.sending_chain_message_number = 3;

    let mut advanced_state = stale_state.clone();
    advanced_state.sending_chain_message_number = 4;

    let device2_state = test_session_state();
    let device3_state = test_session_state();

    let existing = crate::StoredUserRecord {
        user_id: "peer".to_string(),
        devices: vec![
            crate::StoredDeviceRecord {
                device_id: "device-1".to_string(),
                active_session: Some(advanced_state),
                inactive_sessions: Vec::new(),
                created_at: 1,
                is_stale: false,
                stale_timestamp: None,
                last_activity: Some(10),
            },
            crate::StoredDeviceRecord {
                device_id: "device-3".to_string(),
                active_session: Some(device3_state),
                inactive_sessions: Vec::new(),
                created_at: 3,
                is_stale: false,
                stale_timestamp: None,
                last_activity: Some(12),
            },
        ],
        known_device_identities: vec!["device-1".to_string(), "device-3".to_string()],
    };

    let current = crate::StoredUserRecord {
        user_id: "peer".to_string(),
        devices: vec![
            crate::StoredDeviceRecord {
                device_id: "device-1".to_string(),
                active_session: Some(stale_state),
                inactive_sessions: Vec::new(),
                created_at: 1,
                is_stale: false,
                stale_timestamp: None,
                last_activity: Some(9),
            },
            crate::StoredDeviceRecord {
                device_id: "device-2".to_string(),
                active_session: Some(device2_state),
                inactive_sessions: Vec::new(),
                created_at: 2,
                is_stale: false,
                stale_timestamp: None,
                last_activity: Some(11),
            },
        ],
        known_device_identities: vec!["device-1".to_string(), "device-2".to_string()],
    };

    let merged = SessionManager::merge_stored_user_record(existing, current);
    let device1 = merged
        .devices
        .iter()
        .find(|device| device.device_id == "device-1")
        .expect("device-1 should be preserved");
    assert_eq!(
        device1
            .active_session
            .as_ref()
            .map(|state| state.sending_chain_message_number),
        Some(4)
    );
    assert!(merged
        .devices
        .iter()
        .any(|device| device.device_id == "device-2"));
    assert!(merged
        .devices
        .iter()
        .any(|device| device.device_id == "device-3"));
    assert!(merged
        .known_device_identities
        .contains(&"device-3".to_string()));
}

#[test]
fn test_session_manager_new() {
    let keys = Keys::generate();
    let pubkey = keys.public_key();
    let identity_key = keys.secret_key().to_secret_bytes();
    let device_id = "test-device".to_string();

    let (tx, _rx) = crossbeam_channel::unbounded();

    let manager = SessionManager::new(
        pubkey,
        identity_key,
        device_id.clone(),
        pubkey,
        tx,
        None,
        None,
    );

    assert_eq!(manager.get_device_id(), device_id);
}

#[test]
fn test_send_text_no_sessions() {
    let keys = Keys::generate();
    let pubkey = keys.public_key();
    let identity_key = keys.secret_key().to_secret_bytes();
    let device_id = "test-device".to_string();

    let (tx, _rx) = crossbeam_channel::unbounded();

    let manager = SessionManager::new(pubkey, identity_key, device_id, pubkey, tx, None, None);

    let recipient = Keys::generate().public_key();
    let result = manager.send_text(recipient, "test".to_string(), None);

    assert!(result.is_ok());
}

#[test]
fn test_send_typing_does_not_record_in_message_history() {
    let keys = Keys::generate();
    let pubkey = keys.public_key();
    let identity_key = keys.secret_key().to_secret_bytes();
    let device_id = "test-device".to_string();

    let (tx, _rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(pubkey, identity_key, device_id, pubkey, tx, None, None);

    let recipient = Keys::generate().public_key();
    manager.send_typing(recipient, None).unwrap();

    let history = manager.message_history.lock().unwrap();
    assert!(history.is_empty());
}

#[test]
fn send_uses_send_capable_inactive_session_when_active_session_cannot_send() {
    let alice_keys = Keys::generate();
    let alice_pubkey = alice_keys.public_key();
    let alice_device_id = alice_pubkey.to_hex();
    let bob_keys = Keys::generate();
    let bob_pubkey = bob_keys.public_key();
    let bob_device_id = bob_pubkey.to_hex();

    // Alice-created invite leaves Alice with a receive-first session after Bob accepts.
    let alice_invite =
        Invite::create_new(alice_pubkey, Some(alice_device_id.clone()), None).unwrap();
    let (_bob_from_alice, alice_response) = alice_invite
        .accept_with_owner(
            bob_pubkey,
            bob_keys.secret_key().to_secret_bytes(),
            Some(bob_device_id.clone()),
            Some(bob_pubkey),
        )
        .unwrap();
    let alice_receive_only = alice_invite
        .process_invite_response(&alice_response, alice_keys.secret_key().to_secret_bytes())
        .unwrap()
        .unwrap()
        .session;
    assert!(!alice_receive_only.can_send());

    // Bob-created invite leaves Alice with a send-capable session after Alice accepts.
    let bob_invite = Invite::create_new(bob_pubkey, Some(bob_device_id.clone()), None).unwrap();
    let (alice_send_capable, _bob_response) = bob_invite
        .accept_with_owner(
            alice_pubkey,
            alice_keys.secret_key().to_secret_bytes(),
            Some(alice_device_id.clone()),
            Some(alice_pubkey),
        )
        .unwrap();
    assert!(alice_send_capable.can_send());

    let (tx, rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        alice_pubkey,
        alice_keys.secret_key().to_secret_bytes(),
        alice_device_id,
        alice_pubkey,
        tx,
        None,
        None,
    );

    manager.with_user_records({
        let bob_device_id = bob_device_id.clone();
        move |records| {
            let user_record = records
                .entry(bob_pubkey)
                .or_insert_with(|| UserRecord::new(hex::encode(bob_pubkey.to_bytes())));
            user_record.device_records.insert(
                bob_device_id.clone(),
                crate::DeviceRecord {
                    device_id: bob_device_id,
                    public_key: String::new(),
                    active_session: Some(alice_receive_only),
                    inactive_sessions: vec![alice_send_capable],
                    created_at: 0,
                    is_stale: false,
                    stale_timestamp: None,
                    last_activity: Some(0),
                },
            );
        }
    });

    let (_inner_id, published_ids) = manager
        .send_text_with_inner_id(bob_pubkey, "fallback inactive".to_string(), None)
        .unwrap();
    assert_eq!(published_ids.len(), 1);

    let events = drain_events(&rx);
    assert!(
        events.iter().any(|ev| {
            matches!(
                ev,
                SessionManagerEvent::PublishSigned(event)
                    if event.kind.as_u16() == crate::MESSAGE_EVENT_KIND as u16
            )
        }),
        "expected send to publish using promoted inactive session"
    );

    let (active_can_send, inactive_len) = manager.with_user_records(move |records| {
        let device_record = records
            .get(&bob_pubkey)
            .and_then(|record| record.device_records.get(&bob_device_id))
            .expect("device record");
        (
            device_record
                .active_session
                .as_ref()
                .is_some_and(|session| session.can_send()),
            device_record.inactive_sessions.len(),
        )
    });
    assert!(active_can_send);
    assert_eq!(inactive_len, 1);
}

#[test]
fn init_compacts_duplicate_stored_sessions_and_exposes_unique_message_authors() {
    let our_keys = Keys::generate();
    let peer = Keys::generate().public_key();
    let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());

    let state = test_session_state();
    let stored = crate::StoredUserRecord {
        user_id: hex::encode(peer.to_bytes()),
        devices: vec![crate::StoredDeviceRecord {
            device_id: "peer-device".to_string(),
            active_session: Some(state.clone()),
            inactive_sessions: vec![state.clone(), state],
            created_at: 1,
            is_stale: false,
            stale_timestamp: None,
            last_activity: Some(1),
        }],
        known_device_identities: Vec::new(),
    };

    storage
        .put(
            &format!("user/{}", hex::encode(peer.to_bytes())),
            serde_json::to_string(&stored).unwrap(),
        )
        .unwrap();

    let (tx, _rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        our_keys.public_key(),
        our_keys.secret_key().to_secret_bytes(),
        "test-device".to_string(),
        our_keys.public_key(),
        tx,
        Some(storage),
        None,
    );

    manager.init().unwrap();

    assert_eq!(manager.get_all_message_push_author_pubkeys().len(), 2);

    let (active_count, inactive_count) = manager.with_user_records({
        move |records| {
            let device_record = records
                .get(&peer)
                .and_then(|record| record.device_records.get("peer-device"))
                .unwrap();
            (
                usize::from(device_record.active_session.is_some()),
                device_record.inactive_sessions.len(),
            )
        }
    });
    assert_eq!(active_count, 1);
    assert_eq!(inactive_count, 0);
}

#[test]
fn session_manager_exposes_message_authors_without_emitting_subscriptions() {
    let our_keys = Keys::generate();
    let peer = Keys::generate().public_key();
    let peer_device = Keys::generate().public_key();
    let state = test_session_state();
    let expected_authors = SessionManager::session_state_tracked_sender_pubkeys(&state);

    let (tx, rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        our_keys.public_key(),
        our_keys.secret_key().to_secret_bytes(),
        our_keys.public_key().to_hex(),
        our_keys.public_key(),
        tx,
        None,
        None,
    );
    manager.init().unwrap();
    let _ = drain_events(&rx);

    manager
        .import_session_state(peer, Some(peer_device.to_hex()), state)
        .unwrap();

    assert_eq!(
        manager.get_all_message_push_author_pubkeys(),
        expected_authors
    );
    let events = drain_events(&rx);
    assert!(events
        .iter()
        .all(|event| !matches!(event, SessionManagerEvent::Subscribe { .. })));

    manager.delete_chat(peer).unwrap();

    assert!(manager.get_all_message_push_author_pubkeys().is_empty());
    let events = drain_events(&rx);
    assert!(events
        .iter()
        .all(|event| !matches!(event, SessionManagerEvent::Unsubscribe(_))));
}

#[test]
fn imported_device_session_is_authorized_for_owner_fanout() {
    let owner_keys = Keys::generate();
    let owner = owner_keys.public_key();
    let linked_device = Keys::generate().public_key();
    let linked_device_id = linked_device.to_hex();
    let primary_device_id = owner.to_hex();

    let (tx, rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        owner,
        owner_keys.secret_key().to_secret_bytes(),
        primary_device_id,
        owner,
        tx,
        None,
        None,
    );

    manager
        .import_session_state(owner, Some(linked_device_id.clone()), test_session_state())
        .unwrap();

    let known_identities = manager.with_user_records({
        move |records| {
            records
                .get(&owner)
                .map(|record| record.known_device_identities.clone())
                .unwrap_or_default()
        }
    });
    assert!(known_identities.contains(&linked_device_id));
    assert_eq!(manager.resolve_to_owner(&linked_device), owner);

    let event =
        nostr::EventBuilder::new(nostr::Kind::TextNote, "linked-device fanout").build(owner);
    let published_ids = manager.send_event(owner, event).unwrap();
    assert_eq!(published_ids.len(), 1);

    let events = drain_events(&rx);
    assert!(events.iter().any(|event| {
        matches!(
            event,
            SessionManagerEvent::PublishSigned(signed)
                if signed.kind.as_u16() == crate::MESSAGE_EVENT_KIND as u16
        )
    }));
}

#[test]
fn test_delete_chat_removes_local_state_and_allows_reinit() {
    let keys = Keys::generate();
    let pubkey = keys.public_key();
    let identity_key = keys.secret_key().to_secret_bytes();
    let device_id = "test-device".to_string();

    let (tx, _rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(pubkey, identity_key, device_id, pubkey, tx, None, None);
    manager.init().unwrap();

    let peer = Keys::generate().public_key();
    manager.setup_user(peer);
    assert!(manager.get_user_pubkeys().contains(&peer));

    manager.delete_chat(peer).unwrap();
    assert!(!manager.get_user_pubkeys().contains(&peer));

    manager.send_text(peer, "reinit".to_string(), None).unwrap();
    assert!(manager.get_user_pubkeys().contains(&peer));
}

#[test]
fn group_sender_key_distribution_allows_decrypting_one_to_many_outer_messages() {
    let our_keys = Keys::generate();
    let our_pubkey = our_keys.public_key();
    let identity_key = our_keys.secret_key().to_secret_bytes();

    let storage = Arc::new(InMemoryStorage::new());
    let (tx, rx) = crossbeam_channel::unbounded();

    let manager = SessionManager::new(
        our_pubkey,
        identity_key,
        "test-device".to_string(),
        our_pubkey,
        tx,
        Some(storage),
        None,
    );

    let group_id = "g1".to_string();

    let sender_owner_pubkey = Keys::generate().public_key();
    let sender_device_pubkey = Keys::generate().public_key();

    let sender_event_keys = Keys::generate();
    let sender_event_pubkey_hex = hex::encode(sender_event_keys.public_key().to_bytes());

    let key_id = 123u32;
    let chain_key = [7u8; 32];
    let dist = SenderKeyDistribution {
        group_id: group_id.clone(),
        key_id,
        chain_key,
        iteration: 0,
        created_at: 1,
        sender_event_pubkey: Some(sender_event_pubkey_hex.clone()),
    };
    let dist_json = serde_json::to_string(&dist).unwrap();

    let dist_rumor = nostr::EventBuilder::new(
        nostr::Kind::Custom(GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16),
        &dist_json,
    )
    .tag(Tag::parse(&["l".to_string(), group_id.clone()]).unwrap())
    .custom_created_at(nostr::Timestamp::from(1))
    .build(sender_device_pubkey);

    manager
        .maybe_handle_group_sender_key_distribution(
            sender_owner_pubkey,
            Some(sender_device_pubkey),
            &dist_rumor,
        )
        .unwrap();

    let events = drain_events(&rx);
    let expected_subid = format!(
        "group-sender-event-{}",
        hex::encode(sender_event_keys.public_key().to_bytes())
    );
    assert!(events.iter().any(|ev| match ev {
        SessionManagerEvent::Subscribe { subid, .. } => subid == &expected_subid,
        _ => false,
    }));

    let inner = nostr::EventBuilder::new(
        nostr::Kind::Custom(crate::CHAT_MESSAGE_KIND as u16),
        "hello",
    )
    .tag(Tag::parse(&["l".to_string(), group_id.clone()]).unwrap())
    .custom_created_at(nostr::Timestamp::from(10))
    .build(sender_device_pubkey);
    let inner_json = serde_json::to_string(&inner).unwrap();

    let mut sender_state = SenderKeyState::new(key_id, chain_key, 0);
    let outer = OneToManyChannel::default()
        .encrypt_to_outer_event(
            &sender_event_keys,
            &mut sender_state,
            &inner_json,
            nostr::Timestamp::from(10),
        )
        .unwrap();

    manager.process_received_event(outer.clone());

    let events = drain_events(&rx);
    let dec = events.iter().find_map(|ev| match ev {
        SessionManagerEvent::DecryptedMessage {
            sender,
            sender_device,
            content,
            event_id,
        } => Some((*sender, *sender_device, content.clone(), event_id.clone())),
        _ => None,
    });
    let (sender, sender_device, content, event_id) = dec.expect("expected decrypted message");
    assert_eq!(sender, sender_owner_pubkey);
    assert_eq!(sender_device, Some(sender_device_pubkey));
    assert_eq!(event_id, Some(outer.id.to_string()));

    let rumor: UnsignedEvent = serde_json::from_str(&content).unwrap();
    assert_eq!(u32::from(rumor.kind.as_u16()), crate::CHAT_MESSAGE_KIND);
    assert_eq!(rumor.content, "hello");
    assert_eq!(SessionManager::tag_value(&rumor.tags, "l"), Some(group_id));
}

#[test]
fn group_sender_key_queues_outer_until_distribution_arrives_for_key_id() {
    let our_keys = Keys::generate();
    let our_pubkey = our_keys.public_key();
    let identity_key = our_keys.secret_key().to_secret_bytes();

    let storage = Arc::new(InMemoryStorage::new());
    let (tx, rx) = crossbeam_channel::unbounded();

    let manager = SessionManager::new(
        our_pubkey,
        identity_key,
        "test-device".to_string(),
        our_pubkey,
        tx,
        Some(storage),
        None,
    );

    let group_id = "g1".to_string();
    let sender_owner_pubkey = Keys::generate().public_key();
    let sender_device_pubkey = Keys::generate().public_key();

    let sender_event_keys = Keys::generate();
    let sender_event_pubkey_hex = hex::encode(sender_event_keys.public_key().to_bytes());

    // First distribution establishes the sender-event pubkey mapping (key id 1).
    let dist1 = SenderKeyDistribution {
        group_id: group_id.clone(),
        key_id: 1,
        chain_key: [1u8; 32],
        iteration: 0,
        created_at: 1,
        sender_event_pubkey: Some(sender_event_pubkey_hex.clone()),
    };
    let dist1_rumor = nostr::EventBuilder::new(
        nostr::Kind::Custom(GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16),
        serde_json::to_string(&dist1).unwrap(),
    )
    .tag(Tag::parse(&["l".to_string(), group_id.clone()]).unwrap())
    .custom_created_at(nostr::Timestamp::from(1))
    .build(sender_device_pubkey);
    manager
        .maybe_handle_group_sender_key_distribution(
            sender_owner_pubkey,
            Some(sender_device_pubkey),
            &dist1_rumor,
        )
        .unwrap();
    let _ = drain_events(&rx);

    // Now receive an outer message for a new key id (2) before we've seen its distribution.
    let key2 = 2u32;
    let chain2 = [2u8; 32];
    let inner = nostr::EventBuilder::new(
        nostr::Kind::Custom(crate::CHAT_MESSAGE_KIND as u16),
        "later",
    )
    .tag(Tag::parse(&["l".to_string(), group_id.clone()]).unwrap())
    .custom_created_at(nostr::Timestamp::from(10))
    .build(sender_device_pubkey);
    let inner_json = serde_json::to_string(&inner).unwrap();
    let mut sender_state = SenderKeyState::new(key2, chain2, 0);
    let outer = OneToManyChannel::default()
        .encrypt_to_outer_event(
            &sender_event_keys,
            &mut sender_state,
            &inner_json,
            nostr::Timestamp::from(10),
        )
        .unwrap();

    manager.process_received_event(outer.clone());
    assert!(
        drain_events(&rx)
            .iter()
            .all(|ev| !matches!(ev, SessionManagerEvent::DecryptedMessage { .. })),
        "outer should be queued until key distribution arrives"
    );

    // Distribution for key id 2 arrives; queued outer should now decrypt.
    let dist2 = SenderKeyDistribution {
        group_id: group_id.clone(),
        key_id: key2,
        chain_key: chain2,
        iteration: 0,
        created_at: 2,
        sender_event_pubkey: Some(sender_event_pubkey_hex),
    };
    let dist2_rumor = nostr::EventBuilder::new(
        nostr::Kind::Custom(GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16),
        serde_json::to_string(&dist2).unwrap(),
    )
    .tag(Tag::parse(&["l".to_string(), group_id.clone()]).unwrap())
    .custom_created_at(nostr::Timestamp::from(2))
    .build(sender_device_pubkey);
    manager
        .maybe_handle_group_sender_key_distribution(
            sender_owner_pubkey,
            Some(sender_device_pubkey),
            &dist2_rumor,
        )
        .unwrap();

    let events = drain_events(&rx);
    let dec = events.iter().find_map(|ev| match ev {
        SessionManagerEvent::DecryptedMessage {
            sender, content, ..
        } => Some((*sender, content.clone())),
        _ => None,
    });
    let (sender, content) = dec.expect("expected decrypted queued message");
    assert_eq!(sender, sender_owner_pubkey);

    let rumor: UnsignedEvent = serde_json::from_str(&content).unwrap();
    assert_eq!(rumor.content, "later");
}

#[test]
fn init_resubscribes_to_stored_group_sender_event_pubkeys() {
    let our_keys = Keys::generate();
    let our_pubkey = our_keys.public_key();

    let storage = Arc::new(InMemoryStorage::new());

    // First manager stores sender-event mapping in storage.
    {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let manager = SessionManager::new(
            our_pubkey,
            our_keys.secret_key().to_secret_bytes(),
            "test-device".to_string(),
            our_pubkey,
            tx,
            Some(storage.clone()),
            None,
        );

        let group_id = "g1".to_string();
        let sender_owner_pubkey = Keys::generate().public_key();
        let sender_device_pubkey = Keys::generate().public_key();
        let sender_event_keys = Keys::generate();

        let dist = SenderKeyDistribution {
            group_id,
            key_id: 1,
            chain_key: [3u8; 32],
            iteration: 0,
            created_at: 1,
            sender_event_pubkey: Some(hex::encode(sender_event_keys.public_key().to_bytes())),
        };
        let dist_rumor = nostr::EventBuilder::new(
            nostr::Kind::Custom(GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16),
            serde_json::to_string(&dist).unwrap(),
        )
        .tag(Tag::parse(&["l".to_string(), dist.group_id.clone()]).unwrap())
        .custom_created_at(nostr::Timestamp::from(1))
        .build(sender_device_pubkey);

        manager
            .maybe_handle_group_sender_key_distribution(
                sender_owner_pubkey,
                Some(sender_device_pubkey),
                &dist_rumor,
            )
            .unwrap();
    }

    let (tx, rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        our_pubkey,
        our_keys.secret_key().to_secret_bytes(),
        "test-device".to_string(),
        our_pubkey,
        tx,
        Some(storage),
        None,
    );
    manager.init().unwrap();

    let events = drain_events(&rx);
    assert!(
        events.iter().any(|ev| matches!(ev, SessionManagerEvent::Subscribe { subid, .. } if subid.starts_with("group-sender-event-"))),
        "expected group sender-key subscription on init"
    );
}

#[test]
fn queued_message_survives_restart_and_flushes_after_session_creation() {
    let alice_keys = Keys::generate();
    let alice_pubkey = alice_keys.public_key();
    let bob_keys = Keys::generate();
    let bob_pubkey = bob_keys.public_key();

    let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());

    let (tx1, _rx1) = crossbeam_channel::unbounded();
    let manager1 = SessionManager::new(
        alice_pubkey,
        alice_keys.secret_key().to_secret_bytes(),
        alice_pubkey.to_hex(),
        alice_pubkey,
        tx1,
        Some(storage.clone()),
        None,
    );
    manager1.init().unwrap();

    let (inner_id, published_ids) = manager1
        .send_text_with_inner_id(bob_pubkey, "queued before restart".to_string(), None)
        .unwrap();
    assert!(published_ids.is_empty());
    assert!(
        !storage.list("v1/discovery-queue/").unwrap().is_empty(),
        "expected discovery queue entries when recipient devices are unknown"
    );

    drop(manager1);

    let (tx2, rx2) = crossbeam_channel::unbounded();
    let manager2 = SessionManager::new(
        alice_pubkey,
        alice_keys.secret_key().to_secret_bytes(),
        alice_pubkey.to_hex(),
        alice_pubkey,
        tx2,
        Some(storage.clone()),
        None,
    );
    manager2.init().unwrap();
    let _ = drain_events(&rx2);

    let mut app_keys = AppKeys::new(vec![]);
    app_keys.add_device(DeviceEntry::new(bob_pubkey, 1));
    let app_keys_event = app_keys
        .get_event(bob_pubkey)
        .sign_with_keys(&bob_keys)
        .unwrap();
    manager2.process_received_event(app_keys_event);

    let bob_device_id = bob_pubkey.to_hex();
    let queued_keys = storage.list("v1/message-queue/").unwrap();
    assert!(
        queued_keys
            .iter()
            .any(|k| k.contains(&format!("{}/{}", inner_id, bob_device_id))),
        "expected discovery entry to expand into message queue for bob device"
    );

    let invite = Invite::create_new(bob_pubkey, Some(bob_device_id.clone()), None).unwrap();
    let invite_event = invite
        .get_event()
        .unwrap()
        .sign_with_keys(&bob_keys)
        .unwrap();
    manager2.process_received_event(invite_event);

    let events = drain_events(&rx2);
    assert!(
        events
            .iter()
            .any(|ev| queued_publish_inner_event_id(ev) == Some(inner_id.as_str())),
        "expected queued message publish to preserve original inner event id"
    );

    let remaining_keys = storage.list("v1/message-queue/").unwrap();
    assert!(
        !remaining_keys
            .iter()
            .any(|k| k.contains(&format!("{}/{}", inner_id, bob_device_id))),
        "expected queue entry to be removed after successful publish"
    );
}

#[test]
fn queued_message_for_known_appkeys_device_flushes_without_new_appkeys_event() {
    let alice_keys = Keys::generate();
    let alice_pubkey = alice_keys.public_key();
    let bob_keys = Keys::generate();
    let bob_pubkey = bob_keys.public_key();
    let bob_device_id = bob_pubkey.to_hex();

    let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let (tx, rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        alice_pubkey,
        alice_keys.secret_key().to_secret_bytes(),
        alice_pubkey.to_hex(),
        alice_pubkey,
        tx,
        Some(storage.clone()),
        None,
    );
    manager.init().unwrap();
    let _ = drain_events(&rx);

    // Learn recipient devices first (AppKeys known) but don't establish a session yet.
    let mut app_keys = AppKeys::new(vec![]);
    app_keys.add_device(DeviceEntry::new(bob_pubkey, 1));
    let app_keys_event = app_keys
        .get_event(bob_pubkey)
        .sign_with_keys(&bob_keys)
        .unwrap();
    manager.process_received_event(app_keys_event);
    let _ = drain_events(&rx);

    let (inner_id, published_ids) = manager
        .send_text_with_inner_id(bob_pubkey, "queued with known appkeys".to_string(), None)
        .unwrap();
    assert!(
        published_ids.is_empty(),
        "without an active session, send should queue for later"
    );

    // TS parity: this should be queued directly per known device.
    let queued_keys = storage.list("v1/message-queue/").unwrap();
    assert!(
        queued_keys
            .iter()
            .any(|k| k.contains(&format!("{}/{}", inner_id, bob_device_id))),
        "expected recipient message queue entry when AppKeys are already known"
    );

    // Ensure we did not put this recipient message back into discovery queue.
    let mut bob_discovery_count = 0usize;
    for key in storage.list("v1/discovery-queue/").unwrap() {
        let Some(raw) = storage.get(&key).unwrap() else {
            continue;
        };
        let Ok(entry) = serde_json::from_str::<crate::QueueEntry>(&raw) else {
            continue;
        };
        if entry.target_key == bob_pubkey.to_hex()
            && entry.event.id.as_ref().map(|id| id.to_string()) == Some(inner_id.clone())
        {
            bob_discovery_count += 1;
        }
    }
    assert_eq!(
        bob_discovery_count, 0,
        "recipient should not rely on discovery queue after AppKeys are known"
    );

    // Accept invite for that known device without sending another AppKeys event.
    let invite = Invite::create_new(bob_pubkey, Some(bob_device_id.clone()), None).unwrap();
    let invite_event = invite
        .get_event()
        .unwrap()
        .sign_with_keys(&bob_keys)
        .unwrap();
    manager.process_received_event(invite_event);

    let events = drain_events(&rx);
    assert!(
        events
            .iter()
            .any(|ev| queued_publish_inner_event_id(ev) == Some(inner_id.as_str())),
        "expected queued message publish to preserve original inner event id"
    );

    let remaining_keys = storage.list("v1/message-queue/").unwrap();
    assert!(
        !remaining_keys
            .iter()
            .any(|k| k.contains(&format!("{}/{}", inner_id, bob_device_id))),
        "expected recipient queue entry removal after successful publish"
    );
}

#[test]
fn pending_invite_response_subscribes_to_claimed_owner_appkeys() {
    let alice_keys = Keys::generate();
    let alice_pubkey = alice_keys.public_key();
    let alice_device_id = alice_pubkey.to_hex();

    let bob_owner_keys = Keys::generate();
    let bob_owner = bob_owner_keys.public_key();
    let bob_device_keys = Keys::generate();
    let bob_device = bob_device_keys.public_key();
    let bob_device_id = bob_device.to_hex();

    let mut invite = Invite::create_new(alice_pubkey, Some(alice_device_id.clone()), None).unwrap();
    invite.owner_public_key = Some(alice_pubkey);

    let (tx, rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        alice_pubkey,
        alice_keys.secret_key().to_secret_bytes(),
        alice_device_id,
        alice_pubkey,
        tx,
        Some(Arc::new(InMemoryStorage::new())),
        Some(invite.clone()),
    );
    manager.init().unwrap();
    let _ = drain_events(&rx);

    let (_, response_event) = invite
        .accept_with_owner(
            bob_device,
            bob_device_keys.secret_key().to_secret_bytes(),
            Some(bob_device_id.clone()),
            Some(bob_owner),
        )
        .unwrap();

    manager.process_received_event(response_event);
    let events = drain_events(&rx);
    assert!(
        events.iter().any(|event| {
            let SessionManagerEvent::Subscribe { filter_json, .. } = event else {
                return false;
            };
            let Ok(filter) = serde_json::from_str::<serde_json::Value>(filter_json) else {
                return false;
            };
            let has_app_keys_kind = filter
                .get("kinds")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|kinds| {
                    kinds
                        .iter()
                        .any(|kind| kind.as_u64() == Some(crate::APP_KEYS_EVENT_KIND as u64))
                });
            let has_owner_author = filter
                .get("authors")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|authors| {
                    authors
                        .iter()
                        .any(|author| author.as_str() == Some(bob_owner.to_hex().as_str()))
                });
            has_app_keys_kind && has_owner_author
        }),
        "expected pending invite response to subscribe to claimed owner AppKeys"
    );

    assert!(
        manager
            .pending_invite_response_owner_pubkeys()
            .contains(&bob_owner),
        "expected invite response to stay pending until owner AppKeys prove device membership"
    );

    let mut app_keys = AppKeys::new(vec![]);
    app_keys.add_device(DeviceEntry::new(bob_device, 1));
    let app_keys_event = app_keys
        .get_event(bob_owner)
        .sign_with_keys(&bob_owner_keys)
        .unwrap();
    manager.process_received_event(app_keys_event);

    assert!(
        manager
            .export_active_session_state(bob_owner)
            .unwrap()
            .is_some(),
        "expected pending invite response to install after claimed owner AppKeys arrive"
    );
}

#[test]
fn owner_side_link_invite_accepts_new_device_not_yet_in_cached_appkeys() {
    let owner_keys = Keys::generate();
    let owner_pubkey = owner_keys.public_key();
    let known_device_keys = Keys::generate();
    let known_device_pubkey = known_device_keys.public_key();
    let new_device_keys = Keys::generate();
    let new_device_pubkey = new_device_keys.public_key();

    let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let (tx, rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        owner_pubkey,
        owner_keys.secret_key().to_secret_bytes(),
        owner_pubkey.to_hex(),
        owner_pubkey,
        tx,
        Some(storage),
        None,
    );
    manager.init().unwrap();
    let _ = drain_events(&rx);

    let mut app_keys = AppKeys::new(vec![]);
    app_keys.add_device(DeviceEntry::new(owner_pubkey, 1));
    app_keys.add_device(DeviceEntry::new(known_device_pubkey, 2));
    let app_keys_event = app_keys
        .get_event(owner_pubkey)
        .sign_with_keys(&owner_keys)
        .unwrap();
    manager.process_received_event(app_keys_event);

    let mut link_invite =
        Invite::create_new(new_device_pubkey, Some(new_device_pubkey.to_hex()), Some(1)).unwrap();
    link_invite.purpose = Some("link".to_string());
    link_invite.owner_public_key = Some(owner_pubkey);

    let accepted = manager.accept_invite(&link_invite, Some(owner_pubkey));
    assert!(
        accepted.is_ok(),
        "owner-side link invite should allow pre-registration acceptance"
    );
    assert!(accepted.unwrap().created_new_session);
}

#[test]
fn accept_invite_publishes_bootstrap_message_event() {
    let alice_keys = Keys::generate();
    let alice_pubkey = alice_keys.public_key();
    let bob_keys = Keys::generate();
    let bob_pubkey = bob_keys.public_key();

    let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let (tx, rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        alice_pubkey,
        alice_keys.secret_key().to_secret_bytes(),
        alice_pubkey.to_hex(),
        alice_pubkey,
        tx,
        Some(storage),
        None,
    );
    manager.init().unwrap();
    let _ = drain_events(&rx);

    let invite = Invite::create_new(bob_pubkey, Some(bob_pubkey.to_hex()), Some(1)).unwrap();
    let accepted = manager.accept_invite(&invite, Some(bob_pubkey));
    assert!(
        accepted.is_ok(),
        "accept_invite should succeed for single-device peer"
    );

    let events = drain_events(&rx);
    assert!(
        events.iter().any(|ev| {
            matches!(
                ev,
                SessionManagerEvent::PublishSigned(event)
                    if event.kind.as_u16() == crate::MESSAGE_EVENT_KIND as u16
            )
        }),
        "expected a bootstrap message event after invite acceptance"
    );
}

#[test]
fn accept_owner_invite_can_send_first_message_after_app_keys_proof() {
    let alice_owner_keys = Keys::generate();
    let alice_owner = alice_owner_keys.public_key();
    let alice_device_keys = Keys::generate();
    let alice_device = alice_device_keys.public_key();
    let bob_keys = Keys::generate();
    let bob_pubkey = bob_keys.public_key();

    let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let (tx, rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        bob_pubkey,
        bob_keys.secret_key().to_secret_bytes(),
        bob_pubkey.to_hex(),
        bob_pubkey,
        tx,
        Some(storage),
        None,
    );
    manager.init().unwrap();
    let _ = drain_events(&rx);

    let mut invite =
        Invite::create_new(alice_device, Some(alice_device.to_hex()), Some(1)).unwrap();
    invite.owner_public_key = Some(alice_owner);

    let app_keys = AppKeys::new(vec![DeviceEntry::new(alice_device, 1)]);
    manager.ingest_app_keys_snapshot(alice_owner, app_keys, 1);

    let accepted = manager.accept_invite(&invite, Some(alice_owner));
    assert!(
        accepted.is_ok(),
        "accept_invite should succeed for owner/device public invite"
    );
    let _ = drain_events(&rx);

    let event_ids = manager
        .send_text(alice_owner, "first message".to_string(), None)
        .unwrap();
    assert_eq!(
        event_ids.len(),
        1,
        "first message after accepting an owner/device invite should publish immediately"
    );
}

#[test]
fn accept_invite_retries_bootstrap_message_event_with_future_expiration() {
    let alice_keys = Keys::generate();
    let alice_pubkey = alice_keys.public_key();
    let bob_keys = Keys::generate();
    let bob_pubkey = bob_keys.public_key();

    let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let (tx, rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        alice_pubkey,
        alice_keys.secret_key().to_secret_bytes(),
        alice_pubkey.to_hex(),
        alice_pubkey,
        tx,
        Some(storage),
        None,
    );
    manager.init().unwrap();
    let _ = drain_events(&rx);

    let invite = Invite::create_new(bob_pubkey, Some(bob_pubkey.to_hex()), Some(1)).unwrap();
    manager
        .accept_invite(&invite, Some(bob_pubkey))
        .expect("accept_invite should succeed for single-device peer");

    let initial_events = drain_events(&rx);
    assert!(
        initial_events.iter().any(|ev| {
            matches!(
                ev,
                SessionManagerEvent::PublishSigned(event)
                    if event.kind.as_u16() == crate::MESSAGE_EVENT_KIND as u16
            )
        }),
        "expected immediate bootstrap publish"
    );

    std::thread::sleep(Duration::from_millis(2_100));
    let retry_events = drain_events(&rx);
    let retry_count = retry_events
        .iter()
        .filter(|ev| {
            matches!(
                ev,
                SessionManagerEvent::PublishSigned(event)
                    if event.kind.as_u16() == crate::MESSAGE_EVENT_KIND as u16
            )
        })
        .count();
    assert!(
        retry_count >= 2,
        "expected delayed bootstrap retries after invite acceptance"
    );
}

#[test]
fn owner_side_link_invite_accepts_new_device_not_yet_in_stored_appkeys_after_restart() {
    let owner_keys = Keys::generate();
    let owner_pubkey = owner_keys.public_key();
    let known_device_keys = Keys::generate();
    let known_device_pubkey = known_device_keys.public_key();
    let new_device_keys = Keys::generate();
    let new_device_pubkey = new_device_keys.public_key();

    let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let (tx1, rx1) = crossbeam_channel::unbounded();
    let manager1 = SessionManager::new(
        owner_pubkey,
        owner_keys.secret_key().to_secret_bytes(),
        owner_pubkey.to_hex(),
        owner_pubkey,
        tx1,
        Some(storage.clone()),
        None,
    );
    manager1.init().unwrap();
    let _ = drain_events(&rx1);

    let mut app_keys = AppKeys::new(vec![]);
    app_keys.add_device(DeviceEntry::new(owner_pubkey, 1));
    app_keys.add_device(DeviceEntry::new(known_device_pubkey, 2));
    let app_keys_event = app_keys
        .get_event(owner_pubkey)
        .sign_with_keys(&owner_keys)
        .unwrap();
    manager1.process_received_event(app_keys_event);

    drop(manager1);

    let (tx2, rx2) = crossbeam_channel::unbounded();
    let manager2 = SessionManager::new(
        owner_pubkey,
        owner_keys.secret_key().to_secret_bytes(),
        owner_pubkey.to_hex(),
        owner_pubkey,
        tx2,
        Some(storage),
        None,
    );
    manager2.init().unwrap();
    let _ = drain_events(&rx2);

    let mut link_invite =
        Invite::create_new(new_device_pubkey, Some(new_device_pubkey.to_hex()), Some(1)).unwrap();
    link_invite.purpose = Some("link".to_string());
    link_invite.owner_public_key = Some(owner_pubkey);

    let accepted = manager2.accept_invite(&link_invite, Some(owner_pubkey));
    assert!(
        accepted.is_ok(),
        "owner-side link invite should allow pre-registration acceptance after restart"
    );
    assert!(accepted.unwrap().created_new_session);
}

#[test]
fn discovery_entry_retained_when_discovery_expansion_partially_fails() {
    let alice_keys = Keys::generate();
    let alice_pubkey = alice_keys.public_key();
    let bob_keys = Keys::generate();
    let bob_pubkey = bob_keys.public_key();
    let bob_device_id = bob_pubkey.to_hex();

    let base_storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let flaky_storage: Arc<dyn StorageAdapter> =
        Arc::new(FailFirstMessageQueuePutStorage::new(base_storage.clone()));

    let (tx, rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        alice_pubkey,
        alice_keys.secret_key().to_secret_bytes(),
        alice_pubkey.to_hex(),
        alice_pubkey,
        tx,
        Some(flaky_storage.clone()),
        None,
    );
    manager.init().unwrap();
    let _ = drain_events(&rx);

    let (inner_id, published_ids) = manager
        .send_text_with_inner_id(
            bob_pubkey,
            "retry after partial discovery expansion".to_string(),
            None,
        )
        .unwrap();
    assert!(published_ids.is_empty());

    let discovery_count_before = count_queue_entries(
        &flaky_storage,
        "v1/discovery-queue/",
        &bob_pubkey.to_hex(),
        &inner_id,
    );
    assert!(
        discovery_count_before > 0,
        "expected discovery entry before appkeys expansion"
    );

    let mut app_keys = AppKeys::new(vec![]);
    app_keys.add_device(DeviceEntry::new(bob_pubkey, 1));
    let app_keys_event = app_keys
        .get_event(bob_pubkey)
        .sign_with_keys(&bob_keys)
        .unwrap();
    manager.process_received_event(app_keys_event.clone());

    let discovery_count_after_first = count_queue_entries(
        &flaky_storage,
        "v1/discovery-queue/",
        &bob_pubkey.to_hex(),
        &inner_id,
    );
    assert!(
        discovery_count_after_first > 0,
        "discovery entry should be retained when expansion only partially succeeds"
    );

    // Retry AppKeys processing; the injected one-time queue failure is now consumed.
    manager.process_received_event(app_keys_event);
    let queued_count_after_retry = count_queue_entries(
        &flaky_storage,
        "v1/message-queue/",
        &bob_device_id,
        &inner_id,
    );
    assert!(
        queued_count_after_retry > 0,
        "expected retry expansion to enqueue message per device"
    );

    let invite = Invite::create_new(bob_pubkey, Some(bob_device_id.clone()), None).unwrap();
    let invite_event = invite
        .get_event()
        .unwrap()
        .sign_with_keys(&bob_keys)
        .unwrap();
    manager.process_received_event(invite_event);

    let events = drain_events(&rx);
    assert!(
        events.iter().any(is_message_publish),
        "expected queued message to publish after retry and session creation"
    );
}

#[test]
fn appkeys_replacement_cleans_revoked_device_queue_entries() {
    let alice_keys = Keys::generate();
    let alice_pubkey = alice_keys.public_key();
    let bob_owner_keys = Keys::generate();
    let bob_owner_pubkey = bob_owner_keys.public_key();
    let bob_device1_keys = Keys::generate();
    let bob_device2_keys = Keys::generate();
    let bob_device1_id = bob_device1_keys.public_key().to_hex();
    let bob_device2_id = bob_device2_keys.public_key().to_hex();

    let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let (tx, rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        alice_pubkey,
        alice_keys.secret_key().to_secret_bytes(),
        alice_pubkey.to_hex(),
        alice_pubkey,
        tx,
        Some(storage.clone()),
        None,
    );
    manager.init().unwrap();
    let _ = drain_events(&rx);

    // Learn two recipient devices first; no sessions yet.
    let mut app_keys_two = AppKeys::new(vec![]);
    app_keys_two.add_device(DeviceEntry::new(bob_device1_keys.public_key(), 1));
    app_keys_two.add_device(DeviceEntry::new(bob_device2_keys.public_key(), 2));
    let app_keys_two_event =
        sign_app_keys_event_with_created_at(&app_keys_two, bob_owner_pubkey, &bob_owner_keys, 1);
    manager.process_received_event(app_keys_two_event);
    let _ = drain_events(&rx);

    let (inner_id, published_ids) = manager
        .send_text_with_inner_id(
            bob_owner_pubkey,
            "queued for two devices pre-revoke".to_string(),
            None,
        )
        .unwrap();
    assert!(
        published_ids.is_empty(),
        "without sessions, message should queue per known device"
    );
    assert_eq!(
        count_queue_entries(&storage, "v1/message-queue/", &bob_device1_id, &inner_id),
        1
    );
    assert_eq!(
        count_queue_entries(&storage, "v1/message-queue/", &bob_device2_id, &inner_id),
        1
    );

    // Replace AppKeys with only device1 (device2 revoked).
    let mut app_keys_one = AppKeys::new(vec![]);
    app_keys_one.add_device(DeviceEntry::new(bob_device1_keys.public_key(), 3));
    let app_keys_one_event =
        sign_app_keys_event_with_created_at(&app_keys_one, bob_owner_pubkey, &bob_owner_keys, 2);
    manager.process_received_event(app_keys_one_event);

    assert_eq!(
        count_queue_entries(&storage, "v1/message-queue/", &bob_device2_id, &inner_id),
        0,
        "revoked device queue entries should be purged on appkeys replacement"
    );
    assert_eq!(
        count_queue_entries(&storage, "v1/message-queue/", &bob_device1_id, &inner_id),
        1,
        "still-authorized device queue entries should remain"
    );

    // Authorized sibling can still establish session and receive flush.
    let invite = Invite::create_new(
        bob_device1_keys.public_key(),
        Some(bob_device1_id.clone()),
        None,
    )
    .unwrap();
    let invite_event = invite
        .get_event()
        .unwrap()
        .sign_with_keys(&bob_device1_keys)
        .unwrap();
    manager.process_received_event(invite_event);
    let events = drain_events(&rx);
    assert!(
        events.iter().any(is_message_publish),
        "expected queued message to publish for still-authorized device"
    );
}

#[test]
fn stale_appkeys_replay_does_not_remove_newer_devices() {
    fn sign_app_keys_event(
        app_keys: &AppKeys,
        owner_pubkey: PublicKey,
        owner_keys: &Keys,
        created_at: u64,
    ) -> nostr::Event {
        let mut tags = Vec::new();
        tags.push(
            nostr::Tag::parse(&["d".to_string(), "double-ratchet/app-keys".to_string()]).unwrap(),
        );
        tags.push(nostr::Tag::parse(&["version".to_string(), "1".to_string()]).unwrap());
        for device in app_keys.get_all_devices() {
            tags.push(
                nostr::Tag::parse(&[
                    "device".to_string(),
                    hex::encode(device.identity_pubkey.to_bytes()),
                    device.created_at.to_string(),
                ])
                .unwrap(),
            );
        }

        nostr::EventBuilder::new(nostr::Kind::from(crate::APP_KEYS_EVENT_KIND as u16), "")
            .tags(tags)
            .custom_created_at(nostr::Timestamp::from(created_at))
            .build(owner_pubkey)
            .sign_with_keys(owner_keys)
            .unwrap()
    }

    let alice_keys = Keys::generate();
    let alice_pubkey = alice_keys.public_key();
    let bob_owner_keys = Keys::generate();
    let bob_owner_pubkey = bob_owner_keys.public_key();
    let bob_device1_keys = Keys::generate();
    let bob_device2_keys = Keys::generate();
    let bob_device1_id = bob_device1_keys.public_key().to_hex();
    let bob_device2_id = bob_device2_keys.public_key().to_hex();

    let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let (tx, rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        alice_pubkey,
        alice_keys.secret_key().to_secret_bytes(),
        alice_pubkey.to_hex(),
        alice_pubkey,
        tx,
        Some(storage.clone()),
        None,
    );
    manager.init().unwrap();
    let _ = drain_events(&rx);

    let mut app_keys_two = AppKeys::new(vec![]);
    app_keys_two.add_device(DeviceEntry::new(bob_device1_keys.public_key(), 1));
    app_keys_two.add_device(DeviceEntry::new(bob_device2_keys.public_key(), 2));
    manager.process_received_event(sign_app_keys_event(
        &app_keys_two,
        bob_owner_pubkey,
        &bob_owner_keys,
        2,
    ));
    let _ = drain_events(&rx);

    let mut stale_one_device = AppKeys::new(vec![]);
    stale_one_device.add_device(DeviceEntry::new(bob_device1_keys.public_key(), 1));
    manager.process_received_event(sign_app_keys_event(
        &stale_one_device,
        bob_owner_pubkey,
        &bob_owner_keys,
        1,
    ));
    let _ = drain_events(&rx);

    let (inner_id, published_ids) = manager
        .send_text_with_inner_id(
            bob_owner_pubkey,
            "stale appkeys replay should not collapse fanout".to_string(),
            None,
        )
        .unwrap();
    assert!(
        published_ids.is_empty(),
        "without established sessions, message should queue per known device"
    );
    assert_eq!(
        count_queue_entries(&storage, "v1/message-queue/", &bob_device1_id, &inner_id),
        1
    );
    assert_eq!(
        count_queue_entries(&storage, "v1/message-queue/", &bob_device2_id, &inner_id),
        1,
        "older appkeys replay must not revoke the newer second device"
    );
}

#[test]
fn same_timestamp_appkeys_replay_preserves_known_devices() {
    fn sign_app_keys_event(
        app_keys: &AppKeys,
        owner_pubkey: PublicKey,
        owner_keys: &Keys,
        created_at: u64,
    ) -> nostr::Event {
        let mut tags = Vec::new();
        tags.push(
            nostr::Tag::parse(&["d".to_string(), "double-ratchet/app-keys".to_string()]).unwrap(),
        );
        tags.push(nostr::Tag::parse(&["version".to_string(), "1".to_string()]).unwrap());
        for device in app_keys.get_all_devices() {
            tags.push(
                nostr::Tag::parse(&[
                    "device".to_string(),
                    hex::encode(device.identity_pubkey.to_bytes()),
                    device.created_at.to_string(),
                ])
                .unwrap(),
            );
        }

        nostr::EventBuilder::new(nostr::Kind::from(crate::APP_KEYS_EVENT_KIND as u16), "")
            .tags(tags)
            .custom_created_at(nostr::Timestamp::from(created_at))
            .build(owner_pubkey)
            .sign_with_keys(owner_keys)
            .unwrap()
    }

    let alice_keys = Keys::generate();
    let alice_pubkey = alice_keys.public_key();
    let bob_owner_keys = Keys::generate();
    let bob_owner_pubkey = bob_owner_keys.public_key();
    let bob_device1_keys = Keys::generate();
    let bob_device2_keys = Keys::generate();
    let bob_device1_id = bob_device1_keys.public_key().to_hex();
    let bob_device2_id = bob_device2_keys.public_key().to_hex();

    let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let (tx, rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        alice_pubkey,
        alice_keys.secret_key().to_secret_bytes(),
        alice_pubkey.to_hex(),
        alice_pubkey,
        tx,
        Some(storage.clone()),
        None,
    );
    manager.init().unwrap();
    let _ = drain_events(&rx);

    let mut app_keys_two = AppKeys::new(vec![]);
    app_keys_two.add_device(DeviceEntry::new(bob_device1_keys.public_key(), 1));
    app_keys_two.add_device(DeviceEntry::new(bob_device2_keys.public_key(), 2));
    manager.process_received_event(sign_app_keys_event(
        &app_keys_two,
        bob_owner_pubkey,
        &bob_owner_keys,
        5,
    ));
    let _ = drain_events(&rx);

    let mut same_second_subset = AppKeys::new(vec![]);
    same_second_subset.add_device(DeviceEntry::new(bob_device1_keys.public_key(), 1));
    manager.process_received_event(sign_app_keys_event(
        &same_second_subset,
        bob_owner_pubkey,
        &bob_owner_keys,
        5,
    ));
    let _ = drain_events(&rx);

    let (inner_id, published_ids) = manager
        .send_text_with_inner_id(
            bob_owner_pubkey,
            "same-second replay should not collapse fanout".to_string(),
            None,
        )
        .unwrap();
    assert!(published_ids.is_empty());
    assert_eq!(
        count_queue_entries(&storage, "v1/message-queue/", &bob_device1_id, &inner_id),
        1
    );
    assert_eq!(
        count_queue_entries(&storage, "v1/message-queue/", &bob_device2_id, &inner_id),
        1,
        "same-second appkeys replay should preserve previously known devices"
    );
}

#[test]
fn transient_expansion_failure_then_revocation_keeps_only_authorized_retry_path() {
    let alice_keys = Keys::generate();
    let alice_pubkey = alice_keys.public_key();
    let bob_owner_keys = Keys::generate();
    let bob_owner_pubkey = bob_owner_keys.public_key();
    let bob_device1_keys = Keys::generate();
    let bob_device2_keys = Keys::generate();
    let bob_device1_id = bob_device1_keys.public_key().to_hex();
    let bob_device2_id = bob_device2_keys.public_key().to_hex();

    let base_storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let flaky_storage: Arc<dyn StorageAdapter> =
        Arc::new(FailFirstMessageQueuePutStorage::new(base_storage.clone()));

    let (tx, rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        alice_pubkey,
        alice_keys.secret_key().to_secret_bytes(),
        alice_pubkey.to_hex(),
        alice_pubkey,
        tx,
        Some(flaky_storage.clone()),
        None,
    );
    manager.init().unwrap();
    let _ = drain_events(&rx);

    // Queue in discovery first (unknown recipient devices).
    let (inner_id, published_ids) = manager
        .send_text_with_inner_id(
            bob_owner_pubkey,
            "queued before appkeys/revocation".to_string(),
            None,
        )
        .unwrap();
    assert!(published_ids.is_empty());

    // AppKeys with two devices: first expansion attempt will partially fail.
    let mut app_keys_two = AppKeys::new(vec![]);
    app_keys_two.add_device(DeviceEntry::new(bob_device1_keys.public_key(), 1));
    app_keys_two.add_device(DeviceEntry::new(bob_device2_keys.public_key(), 2));
    let app_keys_two_event =
        sign_app_keys_event_with_created_at(&app_keys_two, bob_owner_pubkey, &bob_owner_keys, 1);
    manager.process_received_event(app_keys_two_event);
    assert!(
        count_queue_entries(
            &flaky_storage,
            "v1/discovery-queue/",
            &bob_owner_pubkey.to_hex(),
            &inner_id
        ) > 0,
        "discovery entry should survive partial expansion failure"
    );

    // Revoke device2 by AppKeys replacement. Retry path should keep only device1.
    let mut app_keys_one = AppKeys::new(vec![]);
    app_keys_one.add_device(DeviceEntry::new(bob_device1_keys.public_key(), 3));
    let app_keys_one_event =
        sign_app_keys_event_with_created_at(&app_keys_one, bob_owner_pubkey, &bob_owner_keys, 2);
    manager.process_received_event(app_keys_one_event.clone());
    manager.process_received_event(app_keys_one_event);

    assert_eq!(
        count_queue_entries(
            &flaky_storage,
            "v1/message-queue/",
            &bob_device2_id,
            &inner_id
        ),
        0,
        "revoked device should not keep retryable queue entries"
    );
    assert!(
        count_queue_entries(
            &flaky_storage,
            "v1/message-queue/",
            &bob_device1_id,
            &inner_id
        ) > 0,
        "authorized sibling should retain retryable queue entry"
    );

    let invite = Invite::create_new(
        bob_device1_keys.public_key(),
        Some(bob_device1_id.clone()),
        None,
    )
    .unwrap();
    let invite_event = invite
        .get_event()
        .unwrap()
        .sign_with_keys(&bob_device1_keys)
        .unwrap();
    manager.process_received_event(invite_event);
    let events = drain_events(&rx);
    assert!(
        events.iter().any(is_message_publish),
        "authorized device should receive queued message after retry path"
    );
}

#[test]
fn test_auto_adopt_chat_settings_sender_copy_uses_p_tag_peer() {
    let keys = Keys::generate();
    let pubkey = keys.public_key();
    let identity_key = keys.secret_key().to_secret_bytes();
    let device_id = "test-device".to_string();
    let (tx, _rx) = crossbeam_channel::unbounded();

    let manager = SessionManager::new(pubkey, identity_key, device_id, pubkey, tx, None, None);

    let peer = Keys::generate().public_key();
    let peer_hex = hex::encode(peer.to_bytes());

    // Sender-copy: from_owner_pubkey == us, so peer must be taken from the ["p", ...] tag.
    let payload = serde_json::json!({
        "type": "chat-settings",
        "v": 1,
        "messageTtlSeconds": 90,
    })
    .to_string();

    let rumor = nostr::EventBuilder::new(
        nostr::Kind::from(crate::CHAT_SETTINGS_KIND as u16),
        &payload,
    )
    .tag(
        Tag::parse(&["p".to_string(), peer_hex])
            .map_err(|e| crate::Error::InvalidEvent(e.to_string()))
            .unwrap(),
    )
    .build(pubkey);

    manager.maybe_auto_adopt_chat_settings(pubkey, &rumor);

    let opts = manager
        .peer_send_options
        .lock()
        .unwrap()
        .get(&peer)
        .cloned()
        .unwrap();
    assert_eq!(opts.ttl_seconds, Some(90));
    assert_eq!(opts.expires_at, None);

    // Null disables per-peer expiration (stores an empty SendOptions override).
    let payload_disable = serde_json::json!({
        "type": "chat-settings",
        "v": 1,
        "messageTtlSeconds": null,
    })
    .to_string();

    let rumor_disable = nostr::EventBuilder::new(
        nostr::Kind::from(crate::CHAT_SETTINGS_KIND as u16),
        &payload_disable,
    )
    .tag(
        Tag::parse(&["p".to_string(), hex::encode(peer.to_bytes())])
            .map_err(|e| crate::Error::InvalidEvent(e.to_string()))
            .unwrap(),
    )
    .build(pubkey);

    manager.maybe_auto_adopt_chat_settings(pubkey, &rumor_disable);
    let opts_disable = manager
        .peer_send_options
        .lock()
        .unwrap()
        .get(&peer)
        .cloned()
        .unwrap();
    assert_eq!(opts_disable.ttl_seconds, None);
    assert_eq!(opts_disable.expires_at, None);
}
