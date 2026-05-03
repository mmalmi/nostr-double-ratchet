use nostr::{Event, Keys};
use nostr_double_ratchet::{
    AppKeys, DeviceEntry, GroupIncomingEvent, InMemoryStorage, NdrRuntime, SessionManagerEvent,
    StorageAdapter, GROUP_SENDER_KEY_MESSAGE_KIND, INVITE_EVENT_KIND, MESSAGE_EVENT_KIND,
};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn runtime(device: &Keys, owner: nostr::PublicKey, device_id: &str) -> NdrRuntime {
    NdrRuntime::new(
        device.public_key(),
        device.secret_key().to_secret_bytes(),
        device_id.to_string(),
        owner,
        None,
        None,
    )
}

fn runtime_with_storage(
    device: &Keys,
    owner: nostr::PublicKey,
    device_id: &str,
    storage: Arc<dyn StorageAdapter>,
) -> NdrRuntime {
    NdrRuntime::new(
        device.public_key(),
        device.secret_key().to_secret_bytes(),
        device_id.to_string(),
        owner,
        Some(storage),
        None,
    )
}

fn published_events(runtime: &NdrRuntime) -> Vec<Event> {
    runtime
        .drain_events()
        .into_iter()
        .filter_map(|event| match event {
            SessionManagerEvent::PublishSigned(event)
            | SessionManagerEvent::PublishSignedForInnerEvent { event, .. } => Some(event),
            _ => None,
        })
        .collect()
}

fn first_event_of_kind(events: &[Event], kind: u32) -> Event {
    events
        .iter()
        .find(|event| event.kind.as_u16() as u32 == kind)
        .cloned()
        .expect("event kind")
}

fn deliver_group_related_events(
    receiver: &NdrRuntime,
    events: Vec<Event>,
) -> Vec<GroupIncomingEvent> {
    let mut group_events = Vec::new();
    for event in events {
        if event.kind.as_u16() as u32 == GROUP_SENDER_KEY_MESSAGE_KIND {
            group_events.extend(receiver.group_handle_outer_event(&event));
            continue;
        }
        receiver.process_received_event(event);
        for runtime_event in receiver.drain_events() {
            if let SessionManagerEvent::DecryptedMessage {
                sender,
                sender_device,
                content,
                ..
            } = runtime_event
            {
                group_events.extend(
                    receiver
                        .group_handle_incoming_payload_outcome(
                            content.as_bytes(),
                            sender,
                            sender_device,
                        )
                        .events,
                );
            }
        }
    }
    group_events
}

#[test]
fn runtime_sender_copy_from_restored_same_owner_device_decrypts_with_conversation_metadata() {
    let alice_owner = Keys::generate();
    let alice_old_device = Keys::generate();
    let alice_fresh_device = Keys::generate();
    let bob_owner = Keys::generate();
    let bob_device = Keys::generate();

    let old = runtime(&alice_old_device, alice_owner.public_key(), "alice-old");
    let fresh = runtime(&alice_fresh_device, alice_owner.public_key(), "alice-fresh");
    let bob = runtime(&bob_device, bob_owner.public_key(), "bob");

    old.init().unwrap();
    fresh.init().unwrap();
    bob.init().unwrap();

    let old_invite = first_event_of_kind(&published_events(&old), INVITE_EVENT_KIND);
    let bob_invite = first_event_of_kind(&published_events(&bob), INVITE_EVENT_KIND);
    let _ = published_events(&fresh);

    let alice_app_keys = AppKeys::new(vec![
        DeviceEntry::new(alice_old_device.public_key(), 1),
        DeviceEntry::new(alice_fresh_device.public_key(), 2),
    ]);
    let bob_app_keys = AppKeys::new(vec![DeviceEntry::new(bob_device.public_key(), 1)]);

    let roster_created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_add(1);
    old.ingest_app_keys_snapshot(
        alice_owner.public_key(),
        alice_app_keys.clone(),
        roster_created_at,
    );
    fresh.ingest_app_keys_snapshot(alice_owner.public_key(), alice_app_keys, roster_created_at);
    fresh.ingest_app_keys_snapshot(bob_owner.public_key(), bob_app_keys, roster_created_at);
    fresh.setup_user(bob_owner.public_key()).unwrap();
    fresh.process_received_event(old_invite);
    fresh.process_received_event(bob_invite);
    let _ = published_events(&fresh);

    let body = "restored sender copy";
    let (_, event_ids) = fresh
        .send_text_with_inner_id(bob_owner.public_key(), body.to_string(), None)
        .unwrap();
    assert_eq!(event_ids.len(), 2);
    let outgoing = published_events(&fresh);
    assert_eq!(
        outgoing
            .iter()
            .filter(|event| event.kind.as_u16() as u32 == MESSAGE_EVENT_KIND)
            .count(),
        2
    );

    for event in &outgoing {
        old.process_received_event(event.clone());
    }
    for event in outgoing
        .iter()
        .filter(|event| event.kind.as_u16() as u32 == MESSAGE_EVENT_KIND)
    {
        old.process_received_event(event.clone());
    }

    let decrypted = old
        .drain_events()
        .into_iter()
        .find_map(|event| match event {
            SessionManagerEvent::DecryptedMessage {
                sender,
                conversation_owner,
                content,
                ..
            } if content.contains(body) => Some((sender, conversation_owner)),
            _ => None,
        });
    assert_eq!(
        decrypted,
        Some((alice_owner.public_key(), Some(bob_owner.public_key())))
    );
}

#[test]
fn runtime_replays_prepared_pairwise_publish_after_restart_before_app_drain() {
    let alice_owner = Keys::generate();
    let alice_device = Keys::generate();
    let bob_owner = Keys::generate();
    let bob_device = Keys::generate();
    let alice_storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());

    let alice = runtime_with_storage(
        &alice_device,
        alice_owner.public_key(),
        "alice-device",
        alice_storage.clone(),
    );
    let bob = runtime(&bob_device, bob_owner.public_key(), "bob-device");
    alice.init().unwrap();
    bob.init().unwrap();

    let bob_invite = first_event_of_kind(&published_events(&bob), INVITE_EVENT_KIND);
    let _ = published_events(&alice);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_add(1);
    alice.ingest_app_keys_snapshot(
        bob_owner.public_key(),
        AppKeys::new(vec![DeviceEntry::new(bob_device.public_key(), now)]),
        now,
    );
    alice.setup_user(bob_owner.public_key()).unwrap();
    alice.process_received_event(bob_invite);
    let _ = published_events(&alice);

    let body = "prepared publish survives runtime restart";
    let (_, event_ids) = alice
        .send_text_with_inner_id(bob_owner.public_key(), body.to_string(), None)
        .unwrap();
    assert_eq!(event_ids.len(), 1);
    let expected_event_id = event_ids[0].clone();
    drop(alice);

    let restarted = runtime_with_storage(
        &alice_device,
        alice_owner.public_key(),
        "alice-device",
        alice_storage,
    );
    restarted.init().unwrap();
    let replayed = published_events(&restarted);
    assert!(
        replayed
            .iter()
            .any(|event| event.id.to_string() == expected_event_id),
        "prepared message event should be re-emitted after restart before app drain"
    );

    for event in replayed {
        bob.process_received_event(event);
    }
    let decrypted = bob
        .drain_events()
        .into_iter()
        .find_map(|event| match event {
            SessionManagerEvent::DecryptedMessage { content, .. } if content.contains(body) => {
                Some(content)
            }
            _ => None,
        });
    assert!(
        decrypted.is_some(),
        "replayed event should decrypt at receiver"
    );
}

#[test]
fn runtime_replays_prepared_group_sender_key_publish_after_restart_before_app_drain() {
    let alice_owner = Keys::generate();
    let alice_device = Keys::generate();
    let storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());

    let alice = runtime_with_storage(
        &alice_device,
        alice_owner.public_key(),
        "alice-device",
        storage.clone(),
    );
    alice.init().unwrap();
    let _ = published_events(&alice);
    let created = alice
        .create_group("Crash window".to_string(), Vec::new())
        .expect("create self-only group");
    let _ = published_events(&alice);

    let event_ids = alice
        .send_group_message(
            &created.group.group_id,
            b"prepared group publish survives restart".to_vec(),
            Some("group-inner".to_string()),
        )
        .expect("send group");
    assert_eq!(event_ids.len(), 1);
    drop(alice);

    let restarted = runtime_with_storage(
        &alice_device,
        alice_owner.public_key(),
        "alice-device",
        storage,
    );
    restarted.init().unwrap();
    let replayed = published_events(&restarted);
    let replayed_sender_key_ids = replayed
        .iter()
        .filter(|event| event.kind.as_u16() as u32 == GROUP_SENDER_KEY_MESSAGE_KIND)
        .map(|event| event.id.to_string())
        .collect::<Vec<_>>();

    assert_eq!(
        replayed_sender_key_ids, event_ids,
        "prepared group sender-key outer event should be replayed exactly, not re-encrypted"
    );
}

#[test]
fn runtime_group_create_missing_roster_queues_and_retries_after_appkeys_and_invite() {
    let alice_owner = Keys::generate();
    let alice_device = Keys::generate();
    let bob_owner = Keys::generate();
    let bob_device = Keys::generate();
    let alice_storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());

    let alice = runtime_with_storage(
        &alice_device,
        alice_owner.public_key(),
        "alice-device",
        alice_storage.clone(),
    );
    let bob = runtime(&bob_device, bob_owner.public_key(), "bob-device");
    alice.init().unwrap();
    bob.init().unwrap();
    let bob_invite = first_event_of_kind(&published_events(&bob), INVITE_EVENT_KIND);
    let _ = published_events(&alice);

    let created = alice
        .create_group(
            "Delayed create fanout".to_string(),
            vec![bob_owner.public_key()],
        )
        .expect("create group with missing recipient protocol data");
    assert!(
        !created.prepared.remote.relay_gaps.is_empty(),
        "create should expose missing recipient protocol data"
    );
    assert!(
        !published_events(&alice)
            .iter()
            .any(|event| event.kind.as_u16() as u32 == MESSAGE_EVENT_KIND),
        "create should not publish a pairwise delivery before recipient protocol data exists"
    );
    drop(alice);

    let alice = runtime_with_storage(
        &alice_device,
        alice_owner.public_key(),
        "alice-device",
        alice_storage,
    );
    alice.init().unwrap();
    let _ = published_events(&alice);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_add(1);
    alice.ingest_app_keys_snapshot(
        bob_owner.public_key(),
        AppKeys::new(vec![DeviceEntry::new(bob_device.public_key(), now)]),
        now,
    );
    assert!(
        !published_events(&alice)
            .iter()
            .any(|event| event.kind.as_u16() as u32 == MESSAGE_EVENT_KIND),
        "roster alone should not publish until the device invite is available"
    );

    alice.process_received_event(bob_invite);
    let retried = published_events(&alice);
    assert!(
        retried
            .iter()
            .any(|event| event.kind.as_u16() as u32 == MESSAGE_EVENT_KIND),
        "queued group create fanout should publish after AppKeys and invite arrive"
    );
}

#[test]
fn runtime_sender_key_distribution_gap_retries_after_invite_without_reencrypting_outer() {
    let alice_owner = Keys::generate();
    let alice_device = Keys::generate();
    let bob_owner = Keys::generate();
    let bob_device = Keys::generate();

    let alice = runtime(&alice_device, alice_owner.public_key(), "alice-device");
    let bob = runtime(&bob_device, bob_owner.public_key(), "bob-device");
    alice.init().unwrap();
    bob.init().unwrap();
    let bob_invite = first_event_of_kind(&published_events(&bob), INVITE_EVENT_KIND);
    let _ = published_events(&alice);

    let created = alice
        .create_group("Sender key gap".to_string(), Vec::new())
        .expect("create self-only group");
    let _ = published_events(&alice);

    let updated = alice
        .add_group_members(&created.group.group_id, vec![bob_owner.public_key()])
        .expect("add member with missing protocol data");
    assert_eq!(updated.revision, 2);
    assert!(
        !published_events(&alice)
            .iter()
            .any(|event| event.kind.as_u16() as u32 == MESSAGE_EVENT_KIND),
        "member add should not publish pairwise fanout before recipient protocol data exists"
    );

    let body = b"sender-key message waiting on distribution".to_vec();
    let event_ids = alice
        .send_group_message(&created.group.group_id, body.clone(), None)
        .expect("send group message");
    assert_eq!(event_ids.len(), 1);
    let initial_group_outers = published_events(&alice)
        .into_iter()
        .filter(|event| event.kind.as_u16() as u32 == GROUP_SENDER_KEY_MESSAGE_KIND)
        .collect::<Vec<_>>();
    assert_eq!(initial_group_outers.len(), 1);
    assert_eq!(initial_group_outers[0].id.to_string(), event_ids[0]);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_add(1);
    alice.ingest_app_keys_snapshot(
        bob_owner.public_key(),
        AppKeys::new(vec![DeviceEntry::new(bob_device.public_key(), now)]),
        now,
    );
    bob.ingest_app_keys_snapshot(
        alice_owner.public_key(),
        AppKeys::new(vec![DeviceEntry::new(alice_device.public_key(), now)]),
        now,
    );
    let _ = published_events(&alice);
    alice.process_received_event(bob_invite);
    let retried = published_events(&alice);
    assert!(
        retried
            .iter()
            .any(|event| event.kind.as_u16() as u32 == MESSAGE_EVENT_KIND),
        "queued sender-key distribution should publish after invite arrives"
    );
    assert!(
        !retried
            .iter()
            .any(|event| event.kind.as_u16() as u32 == GROUP_SENDER_KEY_MESSAGE_KIND),
        "retrying missing pairwise sender-key distribution must not re-encrypt the outer group message"
    );

    for event in retried {
        bob.process_received_event(event);
        for decrypted in bob.drain_events() {
            if let SessionManagerEvent::DecryptedMessage {
                sender,
                sender_device,
                content,
                ..
            } = decrypted
            {
                let _ = bob.group_handle_incoming_payload_outcome(
                    content.as_bytes(),
                    sender,
                    sender_device,
                );
            }
        }
    }
    let group_events = bob.group_handle_outer_event(&initial_group_outers[0]);
    let received_body = group_events.into_iter().find_map(|event| match event {
        nostr_double_ratchet::GroupIncomingEvent::Message(message) => Some(message.body),
        _ => None,
    });
    assert_eq!(
        received_body,
        Some(body),
        "delayed original sender-key distribution should decrypt the already-published outer event"
    );
}

#[test]
fn runtime_queues_sender_key_outer_until_required_revision_arrives() {
    let alice_owner = Keys::generate();
    let alice_device = Keys::generate();
    let bob_owner = Keys::generate();
    let bob_device = Keys::generate();

    let alice = runtime(&alice_device, alice_owner.public_key(), "alice-device");
    let bob = runtime(&bob_device, bob_owner.public_key(), "bob-device");
    alice.init().unwrap();
    bob.init().unwrap();
    let bob_invite = first_event_of_kind(&published_events(&bob), INVITE_EVENT_KIND);
    let _ = published_events(&alice);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_add(1);
    alice.ingest_app_keys_snapshot(
        bob_owner.public_key(),
        AppKeys::new(vec![DeviceEntry::new(bob_device.public_key(), now)]),
        now,
    );
    bob.ingest_app_keys_snapshot(
        alice_owner.public_key(),
        AppKeys::new(vec![DeviceEntry::new(alice_device.public_key(), now)]),
        now,
    );
    alice.process_received_event(bob_invite);
    let _ = published_events(&alice);

    let created = alice
        .create_group("Revision queue".to_string(), vec![bob_owner.public_key()])
        .expect("create group");
    let create_events = published_events(&alice);
    let _ = deliver_group_related_events(&bob, create_events);

    alice
        .update_group_name(&created.group.group_id, "Revision queue v2".to_string())
        .expect("rename group");
    let revision_events = published_events(&alice);

    let body = b"future revision sender key message".to_vec();
    let ids = alice
        .send_group_message(&created.group.group_id, body.clone(), None)
        .expect("send group message at revision 2");
    assert_eq!(ids.len(), 1);
    let outer = first_event_of_kind(&published_events(&alice), GROUP_SENDER_KEY_MESSAGE_KIND);

    assert!(
        bob.group_handle_outer_event(&outer).is_empty(),
        "future revision sender-key outer should be queued until metadata arrives"
    );
    let replayed = deliver_group_related_events(&bob, revision_events);
    let received_body = replayed.into_iter().find_map(|event| match event {
        GroupIncomingEvent::Message(message) => Some(message.body),
        _ => None,
    });
    assert_eq!(
        received_body,
        Some(body),
        "queued sender-key outer should replay after the required revision arrives"
    );
}

#[test]
fn runtime_queues_pairwise_group_payload_until_required_revision_arrives() {
    let alice_owner = Keys::generate();
    let alice_device = Keys::generate();
    let bob_owner = Keys::generate();
    let bob_device = Keys::generate();

    let alice = runtime(&alice_device, alice_owner.public_key(), "alice-device");
    let bob = runtime(&bob_device, bob_owner.public_key(), "bob-device");
    alice.init().unwrap();
    bob.init().unwrap();
    let bob_invite = first_event_of_kind(&published_events(&bob), INVITE_EVENT_KIND);
    let _ = published_events(&alice);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_add(1);
    alice.ingest_app_keys_snapshot(
        bob_owner.public_key(),
        AppKeys::new(vec![DeviceEntry::new(bob_device.public_key(), now)]),
        now,
    );
    bob.ingest_app_keys_snapshot(
        alice_owner.public_key(),
        AppKeys::new(vec![DeviceEntry::new(alice_device.public_key(), now)]),
        now,
    );
    alice.process_received_event(bob_invite);
    let _ = published_events(&alice);

    let created = alice
        .create_group(
            "Pairwise revision queue".to_string(),
            vec![bob_owner.public_key()],
        )
        .expect("create group");
    let _ = deliver_group_related_events(&bob, published_events(&alice));

    alice
        .update_group_name(&created.group.group_id, "revision 2".to_string())
        .expect("rename revision 2");
    let revision_2_events = published_events(&alice);
    alice
        .update_group_name(&created.group.group_id, "revision 3".to_string())
        .expect("rename revision 3");
    let revision_3_events = published_events(&alice);

    let early = deliver_group_related_events(&bob, revision_3_events);
    assert!(
        early.is_empty(),
        "future pairwise group payload should be queued until base revision arrives"
    );
    let replayed = deliver_group_related_events(&bob, revision_2_events);
    assert!(
        replayed.iter().any(|event| {
            matches!(
                event,
                GroupIncomingEvent::MetadataUpdated(group)
                    if group.group_id == created.group.group_id
                        && group.revision == 3
                        && group.name == "revision 3"
            )
        }),
        "queued pairwise revision should replay after the missing base revision arrives"
    );
}
