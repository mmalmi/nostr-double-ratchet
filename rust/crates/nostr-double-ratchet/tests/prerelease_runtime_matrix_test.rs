use nostr::{Event, Keys, PublicKey};
use nostr_double_ratchet::OwnerPubkey;
use nostr_double_ratchet_runtime::{
    AppKeys, DeviceEntry, InMemoryStorage, NdrRuntime, SessionManagerEvent, StorageAdapter,
    GROUP_SENDER_KEY_MESSAGE_KIND, INVITE_EVENT_KIND,
};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn runtime(device: &Keys, owner: PublicKey, device_id: &str) -> NdrRuntime {
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
    owner: PublicKey,
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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_add(1)
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

fn owner(pubkey: PublicKey) -> OwnerPubkey {
    OwnerPubkey::from_bytes(pubkey.to_bytes())
}

fn prepare_sender_for_recipient(
    sender: &NdrRuntime,
    recipient_owner: PublicKey,
    recipient_device: PublicKey,
    recipient_invite: Event,
    created_at: u64,
) {
    sender.ingest_app_keys_snapshot(
        recipient_owner,
        AppKeys::new(vec![DeviceEntry::new(recipient_device, created_at)]),
        created_at,
    );
    sender.setup_user(recipient_owner).unwrap();
    sender.process_received_event(recipient_invite);
    let _ = published_events(sender);
}

fn teach_receiver_sender_roster(
    receiver: &NdrRuntime,
    sender_owner: PublicKey,
    sender_device: PublicKey,
    created_at: u64,
) {
    receiver.ingest_app_keys_snapshot(
        sender_owner,
        AppKeys::new(vec![DeviceEntry::new(sender_device, created_at)]),
        created_at,
    );
}

fn deliver_group_related_events(receiver: &NdrRuntime, events: &[Event]) {
    for event in events {
        if event.kind.as_u16() as u32 == GROUP_SENDER_KEY_MESSAGE_KIND {
            let _ = receiver.group_handle_outer_event(event);
            continue;
        }
        receiver.process_received_event(event.clone());
        for runtime_event in receiver.drain_events() {
            if let SessionManagerEvent::DecryptedMessage {
                sender,
                sender_device,
                content,
                ..
            } = runtime_event
            {
                let _ = receiver.group_handle_incoming_payload_outcome(
                    content.as_bytes(),
                    sender,
                    sender_device,
                );
            }
        }
    }
}

#[test]
fn prerelease_runtime_group_membership_admin_and_restart_flow() {
    let alice_owner = Keys::generate();
    let alice_device = Keys::generate();
    let bob_owner = Keys::generate();
    let bob_device = Keys::generate();
    let charlie_owner = Keys::generate();
    let charlie_device = Keys::generate();
    let alice_storage: Arc<dyn StorageAdapter> = Arc::new(InMemoryStorage::new());

    let alice = runtime_with_storage(
        &alice_device,
        alice_owner.public_key(),
        "alice-device",
        alice_storage.clone(),
    );
    let bob = runtime(&bob_device, bob_owner.public_key(), "bob-device");
    let charlie = runtime(
        &charlie_device,
        charlie_owner.public_key(),
        "charlie-device",
    );
    alice.init().unwrap();
    bob.init().unwrap();
    charlie.init().unwrap();

    let alice_invite = first_event_of_kind(&published_events(&alice), INVITE_EVENT_KIND);
    let bob_invite = first_event_of_kind(&published_events(&bob), INVITE_EVENT_KIND);
    let charlie_invite = first_event_of_kind(&published_events(&charlie), INVITE_EVENT_KIND);
    let created_at = now_secs();

    prepare_sender_for_recipient(
        &alice,
        bob_owner.public_key(),
        bob_device.public_key(),
        bob_invite,
        created_at,
    );
    prepare_sender_for_recipient(
        &alice,
        charlie_owner.public_key(),
        charlie_device.public_key(),
        charlie_invite,
        created_at,
    );
    teach_receiver_sender_roster(
        &bob,
        alice_owner.public_key(),
        alice_device.public_key(),
        created_at,
    );
    teach_receiver_sender_roster(
        &charlie,
        alice_owner.public_key(),
        alice_device.public_key(),
        created_at,
    );
    bob.process_received_event(alice_invite.clone());
    charlie.process_received_event(alice_invite);
    let _ = published_events(&bob);
    let _ = published_events(&charlie);

    let created = alice
        .create_group(
            "Pre-release runtime".to_string(),
            vec![bob_owner.public_key()],
        )
        .expect("create group");
    let group_id = created.group.group_id.clone();
    let create_events = published_events(&alice);
    deliver_group_related_events(&bob, &create_events);

    assert_eq!(
        bob.group_snapshots()
            .into_iter()
            .find(|group| group.group_id == group_id)
            .expect("bob group")
            .name,
        "Pre-release runtime"
    );

    alice
        .add_group_members(&group_id, vec![charlie_owner.public_key()])
        .expect("add Charlie");
    let add_events = published_events(&alice);
    deliver_group_related_events(&bob, &add_events);
    deliver_group_related_events(&charlie, &add_events);

    alice
        .set_group_admin(&group_id, charlie_owner.public_key(), true)
        .expect("promote Charlie");
    let admin_events = published_events(&alice);
    deliver_group_related_events(&bob, &admin_events);
    deliver_group_related_events(&charlie, &admin_events);

    let charlie_group = charlie
        .group_snapshots()
        .into_iter()
        .find(|group| group.group_id == group_id)
        .expect("charlie group after admin");
    assert!(charlie_group.admins.contains(&owner(charlie_owner.public_key())));

    alice
        .remove_group_member(&group_id, bob_owner.public_key())
        .expect("remove Bob");
    let remove_events = published_events(&alice);
    deliver_group_related_events(&bob, &remove_events);
    deliver_group_related_events(&charlie, &remove_events);

    let bob_group = bob
        .group_snapshots()
        .into_iter()
        .find(|group| group.group_id == group_id)
        .expect("bob retained final group snapshot");
    assert!(!bob_group.members.contains(&owner(bob_owner.public_key())));
    assert!(
        bob.send_group_message(&group_id, b"removed member send".to_vec(), None)
            .is_err(),
        "removed member must not be able to send"
    );

    drop(alice);
    let restarted_alice = runtime_with_storage(
        &alice_device,
        alice_owner.public_key(),
        "alice-device",
        alice_storage,
    );
    restarted_alice.init().unwrap();
    let restored = restarted_alice
        .group_snapshots()
        .into_iter()
        .find(|group| group.group_id == group_id)
        .expect("restored group snapshot");
    assert!(!restored.members.contains(&owner(bob_owner.public_key())));
    assert!(restored
        .admins
        .contains(&owner(charlie_owner.public_key())));
}
