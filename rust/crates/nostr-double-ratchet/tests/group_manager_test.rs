use nostr::{Event, EventBuilder, Keys, Kind, PublicKey, Tag, Timestamp, UnsignedEvent};
use nostr_double_ratchet::{
    parse_group_metadata, CreateGroupOptions, Error, FanoutGroupMetadataOptions, GroupData,
    GroupManager, GroupManagerOptions, InMemoryStorage, SenderKeyDistribution, StorageAdapter,
    CHAT_MESSAGE_KIND, GROUP_METADATA_KIND, GROUP_SENDER_KEY_DISTRIBUTION_KIND,
};
use std::sync::Arc;

fn make_group(group_id: &str, members: &[PublicKey], admins: &[PublicKey]) -> GroupData {
    GroupData {
        id: group_id.to_string(),
        name: "Test".to_string(),
        description: None,
        picture: None,
        members: members.iter().map(|pk| pk.to_hex()).collect(),
        admins: admins.iter().map(|pk| pk.to_hex()).collect(),
        created_at: 1_700_000_000_000,
        secret: None,
        accepted: Some(true),
    }
}

fn parse_group_tag(event: &UnsignedEvent) -> Option<String> {
    event.tags.iter().find_map(|tag| {
        let parts = tag.clone().to_vec();
        if parts.first().map(|s| s.as_str()) == Some("l") {
            parts.get(1).cloned()
        } else {
            None
        }
    })
}

fn has_ms_tag(event: &UnsignedEvent) -> bool {
    event.tags.iter().any(|tag| {
        let parts = tag.clone().to_vec();
        parts.first().map(|s| s.as_str()) == Some("ms")
    })
}

#[test]
fn create_group_fans_out_metadata_by_default_and_returns_group_data() {
    let alice_owner = Keys::generate().public_key();
    let bob_owner = Keys::generate().public_key();
    let carol_owner = Keys::generate().public_key();
    let alice_device = Keys::generate().public_key();

    let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let mut manager = GroupManager::new(GroupManagerOptions {
        our_owner_pubkey: alice_owner,
        our_device_pubkey: alice_device,
        storage: Some(storage),
        one_to_many: None,
    });

    let bob_hex = bob_owner.to_hex();
    let carol_hex = carol_owner.to_hex();
    let members = [bob_hex.as_str(), carol_hex.as_str()];

    let mut sent: Vec<(PublicKey, UnsignedEvent)> = Vec::new();
    let mut send_pairwise = |recipient: PublicKey, rumor: &UnsignedEvent| {
        sent.push((recipient, rumor.clone()));
        Ok(())
    };

    let result = manager
        .create_group(
            "Metadata Group",
            &members,
            CreateGroupOptions {
                send_pairwise: Some(&mut send_pairwise),
                ..Default::default()
            },
        )
        .unwrap();

    assert_eq!(result.group.name, "Metadata Group");
    assert_eq!(
        result.group.members,
        vec![
            alice_owner.to_hex(),
            bob_owner.to_hex(),
            carol_owner.to_hex()
        ]
    );
    assert!(result.fanout.enabled);
    assert_eq!(result.fanout.attempted, 3);
    assert_eq!(
        result.fanout.succeeded,
        vec![
            alice_owner.to_hex(),
            bob_owner.to_hex(),
            carol_owner.to_hex()
        ]
    );
    assert_eq!(result.fanout.failed, Vec::<String>::new());

    let metadata_rumor = result
        .metadata_rumor
        .expect("metadata rumor should be returned");
    assert_eq!(
        metadata_rumor.kind,
        Kind::Custom(GROUP_METADATA_KIND as u16)
    );
    assert_eq!(metadata_rumor.pubkey, alice_device);
    assert_eq!(
        parse_group_tag(&metadata_rumor),
        Some(result.group.id.clone())
    );
    assert!(has_ms_tag(&metadata_rumor));

    let parsed = parse_group_metadata(&metadata_rumor.content).expect("metadata should parse");
    assert_eq!(parsed.id, result.group.id);
    assert_eq!(parsed.name, "Metadata Group");
    assert_eq!(
        parsed.members,
        vec![
            alice_owner.to_hex(),
            bob_owner.to_hex(),
            carol_owner.to_hex()
        ]
    );
    assert_eq!(parsed.admins, vec![alice_owner.to_hex()]);

    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0].0, alice_owner);
    assert_eq!(sent[1].0, bob_owner);
    assert_eq!(sent[2].0, carol_owner);
    assert_eq!(sent[0].1.kind, Kind::Custom(GROUP_METADATA_KIND as u16));
    assert_eq!(sent[1].1.kind, Kind::Custom(GROUP_METADATA_KIND as u16));
    assert_eq!(sent[2].1.kind, Kind::Custom(GROUP_METADATA_KIND as u16));
}

#[test]
fn create_group_can_disable_metadata_fanout() {
    let alice_owner = Keys::generate().public_key();
    let bob_owner = Keys::generate().public_key();
    let alice_device = Keys::generate().public_key();

    let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let mut manager = GroupManager::new(GroupManagerOptions {
        our_owner_pubkey: alice_owner,
        our_device_pubkey: alice_device,
        storage: Some(storage),
        one_to_many: None,
    });

    let bob_hex = bob_owner.to_hex();
    let members = [bob_hex.as_str()];
    let result = manager
        .create_group(
            "Local Draft Group",
            &members,
            CreateGroupOptions {
                fanout_metadata: false,
                ..Default::default()
            },
        )
        .unwrap();

    assert_eq!(result.group.name, "Local Draft Group");
    assert!(!result.fanout.enabled);
    assert_eq!(result.fanout.attempted, 0);
    assert_eq!(result.fanout.succeeded, Vec::<String>::new());
    assert_eq!(result.fanout.failed, Vec::<String>::new());
    assert!(result.metadata_rumor.is_none());
}

#[test]
fn create_group_requires_send_pairwise_when_fanout_enabled() {
    let alice_owner = Keys::generate().public_key();
    let bob_owner = Keys::generate().public_key();
    let alice_device = Keys::generate().public_key();

    let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let mut manager = GroupManager::new(GroupManagerOptions {
        our_owner_pubkey: alice_owner,
        our_device_pubkey: alice_device,
        storage: Some(storage),
        one_to_many: None,
    });

    let bob_hex = bob_owner.to_hex();
    let members = [bob_hex.as_str()];
    let result = manager.create_group("Needs Sender", &members, CreateGroupOptions::default());

    assert!(
        matches!(result, Err(Error::InvalidEvent(ref message)) if message.contains("send_pairwise")),
        "expected missing send_pairwise error, got: {:?}",
        result
    );
}

#[test]
fn fan_out_group_metadata_redacts_secret_for_removed_member() {
    let alice_owner = Keys::generate().public_key();
    let bob_owner = Keys::generate().public_key();
    let carol_owner = Keys::generate().public_key();
    let alice_device = Keys::generate().public_key();

    let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let mut manager = GroupManager::new(GroupManagerOptions {
        our_owner_pubkey: alice_owner,
        our_device_pubkey: alice_device,
        storage: Some(storage),
        one_to_many: None,
    });

    let updated_group = make_group(
        "group-metadata-update",
        &[alice_owner, bob_owner],
        &[alice_owner],
    );
    let removed_member_hex = carol_owner.to_hex();

    let mut sent: Vec<(PublicKey, UnsignedEvent)> = Vec::new();
    let mut send_pairwise = |recipient: PublicKey, rumor: &UnsignedEvent| {
        sent.push((recipient, rumor.clone()));
        Ok(())
    };

    let result = manager
        .fan_out_group_metadata(
            updated_group.clone(),
            FanoutGroupMetadataOptions {
                send_pairwise: &mut send_pairwise,
                exclude_secret_for: Some(removed_member_hex.as_str()),
                now_ms: Some(1_700_000_000_000),
            },
        )
        .unwrap();

    assert_eq!(result.group, updated_group);
    assert_eq!(result.fanout.attempted, 3);
    assert_eq!(
        result.fanout.succeeded,
        vec![
            alice_owner.to_hex(),
            bob_owner.to_hex(),
            removed_member_hex.clone()
        ]
    );
    assert_eq!(result.fanout.failed, Vec::<String>::new());

    let full_metadata = parse_group_metadata(&result.metadata_rumor.content).unwrap();
    assert_eq!(full_metadata.secret, updated_group.secret);

    let redacted_metadata = parse_group_metadata(
        &result
            .redacted_metadata_rumor
            .as_ref()
            .expect("removed member should receive redacted metadata")
            .content,
    )
    .unwrap();
    assert_eq!(redacted_metadata.secret, None);

    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0].0, alice_owner);
    assert_eq!(sent[1].0, bob_owner);
    assert_eq!(sent[2].0, carol_owner);

    let alice_metadata = parse_group_metadata(&sent[0].1.content).unwrap();
    assert_eq!(alice_metadata.secret, updated_group.secret);
    let bob_metadata = parse_group_metadata(&sent[1].1.content).unwrap();
    assert_eq!(bob_metadata.secret, updated_group.secret);
    let carol_metadata = parse_group_metadata(&sent[2].1.content).unwrap();
    assert_eq!(carol_metadata.secret, None);
}

#[test]
fn drains_queued_outer_after_sender_key_distribution() {
    let group_id = "group-manager-queue";

    let alice_owner = Keys::generate().public_key();
    let bob_owner = Keys::generate().public_key();
    let alice_device = Keys::generate().public_key();
    let bob_device = Keys::generate().public_key();

    let storage_alice: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let storage_bob: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());

    let mut alice = GroupManager::new(GroupManagerOptions {
        our_owner_pubkey: alice_owner,
        our_device_pubkey: alice_device,
        storage: Some(storage_alice),
        one_to_many: None,
    });
    let mut bob = GroupManager::new(GroupManagerOptions {
        our_owner_pubkey: bob_owner,
        our_device_pubkey: bob_device,
        storage: Some(storage_bob),
        one_to_many: None,
    });

    let group = make_group(group_id, &[alice_owner, bob_owner], &[alice_owner]);
    alice.upsert_group(group.clone()).unwrap();
    bob.upsert_group(group).unwrap();

    let mut distribution: Option<UnsignedEvent> = None;
    let mut outer: Option<Event> = None;

    let mut send_pairwise = |_to: PublicKey, rumor: &UnsignedEvent| {
        distribution = Some(rumor.clone());
        Ok(())
    };
    let mut publish_outer = |event: &Event| {
        outer = Some(event.clone());
        Ok(())
    };

    let sent = alice
        .send_message(
            group_id,
            "hello group",
            &mut send_pairwise,
            &mut publish_outer,
            Some(1_700_000_000_000),
        )
        .unwrap();

    assert_eq!(sent.inner.pubkey, alice_device);
    assert!(distribution.is_some());
    assert!(outer.is_some());

    let early = bob.handle_outer_event(outer.as_ref().unwrap());
    assert!(early.is_none());

    let drained = bob.handle_incoming_session_event(
        distribution.as_ref().unwrap(),
        alice_owner,
        Some(alice_device),
    );
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].inner.content, "hello group");
    assert_eq!(
        parse_group_tag(&drained[0].inner),
        Some(group_id.to_string())
    );
}

#[test]
fn send_message_uses_device_pubkey_and_distributes_sender_key_once() {
    let group_id = "group-manager-send";

    let alice_owner = Keys::generate().public_key();
    let bob_owner = Keys::generate().public_key();
    let alice_device = Keys::generate().public_key();

    let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let mut manager = GroupManager::new(GroupManagerOptions {
        our_owner_pubkey: alice_owner,
        our_device_pubkey: alice_device,
        storage: Some(storage),
        one_to_many: None,
    });
    manager
        .upsert_group(make_group(
            group_id,
            &[alice_owner, bob_owner],
            &[alice_owner],
        ))
        .unwrap();

    let mut pairwise_first: Vec<UnsignedEvent> = Vec::new();
    let mut published_first: Vec<Event> = Vec::new();
    let mut send_pairwise_first = |_to: PublicKey, rumor: &UnsignedEvent| {
        pairwise_first.push(rumor.clone());
        Ok(())
    };
    let mut publish_outer_first = |event: &Event| {
        published_first.push(event.clone());
        Ok(())
    };

    let sent = manager
        .send_message(
            group_id,
            "from-device",
            &mut send_pairwise_first,
            &mut publish_outer_first,
            Some(1_700_000_100_000),
        )
        .unwrap();

    assert_eq!(sent.inner.pubkey, alice_device);
    assert_eq!(pairwise_first.len(), 2);
    assert_eq!(
        pairwise_first[0].kind,
        Kind::Custom(GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16)
    );
    assert_eq!(pairwise_first[0].pubkey, alice_device);
    assert_eq!(pairwise_first[1].kind, pairwise_first[0].kind);
    assert_eq!(pairwise_first[1].pubkey, pairwise_first[0].pubkey);
    assert_eq!(published_first.len(), 1);
    let known_sender_events = manager.known_sender_event_pubkeys();
    assert_eq!(
        known_sender_events.len(),
        0,
        "local sender-event pubkeys should not be surfaced for outer subscriptions"
    );

    let mut pairwise_second: Vec<UnsignedEvent> = Vec::new();
    let mut published_second: Vec<Event> = Vec::new();
    let mut send_pairwise_second = |_to: PublicKey, rumor: &UnsignedEvent| {
        pairwise_second.push(rumor.clone());
        Ok(())
    };
    let mut publish_outer_second = |event: &Event| {
        published_second.push(event.clone());
        Ok(())
    };

    let _ = manager
        .send_message(
            group_id,
            "second",
            &mut send_pairwise_second,
            &mut publish_outer_second,
            Some(1_700_000_200_000),
        )
        .unwrap();

    assert_eq!(pairwise_second.len(), 0);
    assert_eq!(published_second.len(), 1);
}

#[test]
fn same_owner_sibling_device_can_decrypt_group_message_after_self_distribution() {
    let group_id = "group-manager-same-owner";

    let owner = Keys::generate().public_key();
    let device_a = Keys::generate().public_key();
    let device_b = Keys::generate().public_key();

    let storage_a: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let storage_b: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());

    let mut sender = GroupManager::new(GroupManagerOptions {
        our_owner_pubkey: owner,
        our_device_pubkey: device_a,
        storage: Some(storage_a),
        one_to_many: None,
    });
    let mut receiver = GroupManager::new(GroupManagerOptions {
        our_owner_pubkey: owner,
        our_device_pubkey: device_b,
        storage: Some(storage_b),
        one_to_many: None,
    });

    let group = make_group(group_id, &[owner], &[owner]);
    sender.upsert_group(group.clone()).unwrap();
    receiver.upsert_group(group).unwrap();

    let mut pairwise: Vec<(PublicKey, UnsignedEvent)> = Vec::new();
    let mut outer: Option<Event> = None;
    let mut send_pairwise = |recipient: PublicKey, rumor: &UnsignedEvent| {
        pairwise.push((recipient, rumor.clone()));
        Ok(())
    };
    let mut publish_outer = |event: &Event| {
        outer = Some(event.clone());
        Ok(())
    };

    sender
        .send_message(
            group_id,
            "hello sibling device",
            &mut send_pairwise,
            &mut publish_outer,
            Some(1_700_000_300_000),
        )
        .unwrap();

    assert_eq!(pairwise.len(), 1);
    assert_eq!(pairwise[0].0, owner);

    let drained = receiver.handle_incoming_session_event(&pairwise[0].1, owner, Some(device_a));
    assert!(drained.is_empty());

    let decrypted = receiver
        .handle_outer_event(outer.as_ref().expect("outer event"))
        .expect("sibling device should decrypt outer message");
    assert_eq!(decrypted.inner.content, "hello sibling device");
}

#[test]
fn decrypts_typescript_outer_after_distribution_mapping() {
    #[derive(serde::Deserialize)]
    struct OneToManyVectors {
        sender_pubkey: String,
        key_id: u32,
        chain_key_hex: String,
        iteration: u32,
        created_at: u64,
        messages: Vec<OneToManyMessageVector>,
    }
    #[derive(serde::Deserialize)]
    struct OneToManyMessageVector {
        plaintext: String,
        outer_event: serde_json::Value,
    }

    let vectors_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("test-vectors")
        .join("ts-one-to-many-vectors.json");

    if !vectors_path.exists() {
        eprintln!("Skipping: missing {:?}", vectors_path);
        return;
    }

    let content = std::fs::read_to_string(vectors_path).unwrap();
    let vectors: OneToManyVectors = serde_json::from_str(&content).unwrap();

    let alice_owner = Keys::generate().public_key();
    let bob_owner = Keys::generate().public_key();
    let sender_device = Keys::generate().public_key();

    let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let mut manager = GroupManager::new(GroupManagerOptions {
        our_owner_pubkey: bob_owner,
        our_device_pubkey: Keys::generate().public_key(),
        storage: Some(storage),
        one_to_many: None,
    });

    let group_id = "interop-group";
    manager
        .upsert_group(make_group(
            group_id,
            &[alice_owner, bob_owner],
            &[alice_owner],
        ))
        .unwrap();

    let chain = hex::decode(&vectors.chain_key_hex).unwrap();
    let mut chain_key = [0u8; 32];
    chain_key.copy_from_slice(&chain);

    let dist = SenderKeyDistribution {
        group_id: group_id.to_string(),
        key_id: vectors.key_id,
        chain_key,
        iteration: vectors.iteration,
        created_at: vectors.created_at,
        sender_event_pubkey: Some(vectors.sender_pubkey.clone()),
    };

    let dist_event = EventBuilder::new(
        Kind::Custom(GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16),
        serde_json::to_string(&dist).unwrap(),
    )
    .tag(Tag::parse(&["l".to_string(), group_id.to_string()]).unwrap())
    .custom_created_at(Timestamp::from(vectors.created_at))
    .build(sender_device);

    let _ = manager.handle_incoming_session_event(&dist_event, alice_owner, Some(sender_device));
    let known_sender_events = manager.known_sender_event_pubkeys();
    assert_eq!(known_sender_events.len(), 1);
    assert_eq!(known_sender_events[0].to_hex(), vectors.sender_pubkey);

    let outer: Event = serde_json::from_value(vectors.messages[0].outer_event.clone()).unwrap();
    let decrypted = manager
        .handle_outer_event(&outer)
        .expect("outer should decrypt through manager");

    assert_eq!(decrypted.key_id, vectors.key_id);
    assert_eq!(decrypted.group_id, group_id);
    assert_eq!(
        decrypted.sender_event_pubkey.to_hex(),
        vectors.sender_pubkey
    );
    assert_eq!(decrypted.inner.kind, Kind::Custom(CHAT_MESSAGE_KIND as u16));
    let parsed_plaintext_content =
        serde_json::from_str::<serde_json::Value>(&vectors.messages[0].plaintext)
            .ok()
            .and_then(|value| {
                value
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string())
            });
    assert!(
        decrypted.inner.content == vectors.messages[0].plaintext
            || parsed_plaintext_content
                .as_deref()
                .is_some_and(|content| content == decrypted.inner.content)
    );
}

#[test]
fn suppresses_local_device_outer_echo_by_default() {
    let group_id = "group-local-echo";

    let alice_owner = Keys::generate().public_key();
    let bob_owner = Keys::generate().public_key();
    let alice_device = Keys::generate().public_key();

    let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let mut manager = GroupManager::new(GroupManagerOptions {
        our_owner_pubkey: alice_owner,
        our_device_pubkey: alice_device,
        storage: Some(storage),
        one_to_many: None,
    });
    manager
        .upsert_group(make_group(
            group_id,
            &[alice_owner, bob_owner],
            &[alice_owner],
        ))
        .unwrap();

    let mut outer: Option<Event> = None;
    let mut send_pairwise = |_to: PublicKey, _rumor: &UnsignedEvent| Ok(());
    let mut publish_outer = |event: &Event| {
        outer = Some(event.clone());
        Ok(())
    };

    let sent = manager
        .send_message(
            group_id,
            "local-device-message",
            &mut send_pairwise,
            &mut publish_outer,
            None,
        )
        .unwrap();
    assert_eq!(sent.inner.pubkey, alice_device);

    let decrypted = manager.handle_outer_event(outer.as_ref().expect("outer event"));
    assert!(
        decrypted.is_none(),
        "local-device outer echo should be suppressed"
    );
}

#[test]
fn removing_member_purges_sender_mapping_and_blocks_future_delivery() {
    let group_id = "group-manager-revocation";

    let alice_owner = Keys::generate().public_key();
    let bob_owner = Keys::generate().public_key();
    let alice_device = Keys::generate().public_key();
    let bob_device = Keys::generate().public_key();

    let storage_alice: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());
    let storage_bob: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());

    let mut alice = GroupManager::new(GroupManagerOptions {
        our_owner_pubkey: alice_owner,
        our_device_pubkey: alice_device,
        storage: Some(storage_alice),
        one_to_many: None,
    });
    let mut bob = GroupManager::new(GroupManagerOptions {
        our_owner_pubkey: bob_owner,
        our_device_pubkey: bob_device,
        storage: Some(storage_bob),
        one_to_many: None,
    });

    let initial_group = make_group(group_id, &[alice_owner, bob_owner], &[alice_owner]);
    alice.upsert_group(initial_group.clone()).unwrap();
    bob.upsert_group(initial_group).unwrap();

    let mut first_distribution: Option<UnsignedEvent> = None;
    let mut first_outer: Option<Event> = None;
    let mut send_pairwise_first = |_to: PublicKey, rumor: &UnsignedEvent| {
        first_distribution = Some(rumor.clone());
        Ok(())
    };
    let mut publish_outer_first = |event: &Event| {
        first_outer = Some(event.clone());
        Ok(())
    };

    alice
        .send_message(
            group_id,
            "before-revocation",
            &mut send_pairwise_first,
            &mut publish_outer_first,
            Some(1_700_000_300_000),
        )
        .unwrap();

    let drained = bob.handle_incoming_session_event(
        first_distribution.as_ref().unwrap(),
        alice_owner,
        Some(alice_device),
    );
    assert!(drained.is_empty());

    let known_sender_events = bob.known_sender_event_pubkeys();
    assert_eq!(known_sender_events.len(), 1);

    let before = bob
        .handle_outer_event(first_outer.as_ref().unwrap())
        .expect("outer should decrypt before revocation");
    assert_eq!(before.inner.content, "before-revocation");

    bob.upsert_group(make_group(group_id, &[bob_owner], &[bob_owner]))
        .unwrap();
    assert_eq!(
        bob.known_sender_event_pubkeys().len(),
        0,
        "removed member sender mapping should be purged"
    );

    let mut second_outer: Option<Event> = None;
    let mut send_pairwise_second = |_to: PublicKey, _rumor: &UnsignedEvent| Ok(());
    let mut publish_outer_second = |event: &Event| {
        second_outer = Some(event.clone());
        Ok(())
    };

    alice
        .send_message(
            group_id,
            "after-revocation",
            &mut send_pairwise_second,
            &mut publish_outer_second,
            Some(1_700_000_400_000),
        )
        .unwrap();

    assert!(
        bob.handle_outer_event(second_outer.as_ref().unwrap())
            .is_none(),
        "removed member outer message should not decrypt"
    );

    let mut rotate_distribution: Option<UnsignedEvent> = None;
    let mut send_pairwise_rotate = |_to: PublicKey, rumor: &UnsignedEvent| {
        rotate_distribution = Some(rumor.clone());
        Ok(())
    };
    alice
        .rotate_sender_key(group_id, &mut send_pairwise_rotate, Some(1_700_000_500_000))
        .unwrap();

    let drained_after_removal = bob.handle_incoming_session_event(
        rotate_distribution.as_ref().unwrap(),
        alice_owner,
        Some(alice_device),
    );
    assert!(drained_after_removal.is_empty());
    assert_eq!(
        bob.known_sender_event_pubkeys().len(),
        0,
        "removed member distribution should not reintroduce sender mapping"
    );
}
