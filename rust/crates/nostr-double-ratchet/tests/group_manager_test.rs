use nostr::{Event, EventBuilder, Keys, Kind, PublicKey, Tag, Timestamp, UnsignedEvent};
use nostr_double_ratchet::{
    GroupData, GroupManager, GroupManagerOptions, InMemoryStorage, SenderKeyDistribution,
    StorageAdapter, CHAT_MESSAGE_KIND, GROUP_SENDER_KEY_DISTRIBUTION_KIND,
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
    assert_eq!(pairwise_first.len(), 1);
    assert_eq!(
        pairwise_first[0].kind,
        Kind::Custom(GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16)
    );
    assert_eq!(pairwise_first[0].pubkey, alice_device);
    assert_eq!(published_first.len(), 1);

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
