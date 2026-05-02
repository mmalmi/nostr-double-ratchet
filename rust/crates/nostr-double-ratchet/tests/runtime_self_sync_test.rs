use nostr::{Event, Keys};
use nostr_double_ratchet::{
    AppKeys, DeviceEntry, NdrRuntime, SessionManagerEvent, INVITE_EVENT_KIND, MESSAGE_EVENT_KIND,
};
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
