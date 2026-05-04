use std::collections::BTreeSet;

use nostr::{Event, Keys, PublicKey};
use nostr_double_ratchet_runtime::{
    AuthorizedDevice, DevicePubkey, DeviceRoster, GroupIncomingEvent, Invite, NdrRuntime,
    OwnerPubkey, SessionManagerEvent, UnixSeconds, MESSAGE_EVENT_KIND,
};

#[derive(Clone)]
struct Published {
    event: Event,
    target_device_id: Option<String>,
}

fn runtime(owner: &Keys, device: &Keys) -> NdrRuntime {
    let device_id = device.public_key().to_hex();
    let invite = Invite::create_new(device.public_key(), Some(device_id.clone()), None)
        .expect("local invite");
    let runtime = NdrRuntime::new(
        device.public_key(),
        device.secret_key().to_secret_bytes(),
        device_id,
        owner.public_key(),
        None,
        Some(invite),
    );
    runtime.init().expect("runtime init");
    let _ = runtime.drain_events();
    runtime
}

fn owner(public_key: PublicKey) -> OwnerPubkey {
    OwnerPubkey::from_bytes(public_key.to_bytes())
}

fn device(public_key: PublicKey) -> DevicePubkey {
    DevicePubkey::from_bytes(public_key.to_bytes())
}

fn roster(devices: &[&Keys], created_at: u64) -> DeviceRoster {
    DeviceRoster::new(
        UnixSeconds(created_at),
        devices
            .iter()
            .map(|keys| AuthorizedDevice::new(device(keys.public_key()), UnixSeconds(created_at)))
            .collect(),
    )
}

fn drain_published(runtime: &NdrRuntime, signer: &Keys) -> Vec<Published> {
    runtime
        .drain_events()
        .into_iter()
        .filter_map(|event| match event {
            SessionManagerEvent::Publish(unsigned) if unsigned.pubkey == signer.public_key() => {
                unsigned.sign_with_keys(signer).ok().map(|event| Published {
                    event,
                    target_device_id: None,
                })
            }
            SessionManagerEvent::PublishSigned(event) => Some(Published {
                event,
                target_device_id: None,
            }),
            SessionManagerEvent::PublishSignedForInnerEvent {
                event,
                target_device_id,
                ..
            } => Some(Published {
                event,
                target_device_id,
            }),
            _ => None,
        })
        .collect()
}

fn deliver(events: &[Published], to: &NdrRuntime) -> Vec<GroupIncomingEvent> {
    for published in events {
        to.process_received_event(published.event.clone());
    }

    let mut group_events = Vec::new();
    for event in to.drain_events() {
        if let SessionManagerEvent::DecryptedMessage {
            sender,
            sender_device,
            content,
            ..
        } = event
        {
            let outcome =
                to.group_handle_incoming_payload_outcome(content.as_bytes(), sender, sender_device);
            group_events.extend(outcome.events);
        }
    }
    group_events
}

#[test]
fn sender_key_group_create_syncs_to_linked_runtime_device() {
    let alice_owner = Keys::generate();
    let alice_primary_device = Keys::generate();
    let alice_linked_device = Keys::generate();
    let bob_owner = Keys::generate();
    let bob_device = Keys::generate();
    let charlie_owner = Keys::generate();
    let charlie_device = Keys::generate();

    let alice_primary = runtime(&alice_owner, &alice_primary_device);
    let alice_linked = runtime(&alice_owner, &alice_linked_device);
    let bob = runtime(&bob_owner, &bob_device);
    let charlie = runtime(&charlie_owner, &charlie_device);

    alice_primary.with_group_context(|core, _, _| {
        core.apply_local_roster(roster(
            &[&alice_primary_device, &alice_linked_device],
            3_000_000_010,
        ));
        core.observe_device_invite(
            owner(alice_owner.public_key()),
            alice_linked.local_invite().expect("linked invite"),
        )
        .expect("observe linked invite");
        core.observe_peer_roster(
            owner(bob_owner.public_key()),
            roster(&[&bob_device], 3_000_000_011),
        );
        core.observe_device_invite(
            owner(bob_owner.public_key()),
            bob.local_invite().expect("bob invite"),
        )
        .expect("observe bob invite");
        core.observe_peer_roster(
            owner(charlie_owner.public_key()),
            roster(&[&charlie_device], 3_000_000_012),
        );
        core.observe_device_invite(
            owner(charlie_owner.public_key()),
            charlie.local_invite().expect("charlie invite"),
        )
        .expect("observe charlie invite");
    });
    alice_linked.with_group_context(|core, _, _| {
        core.apply_local_roster(roster(
            &[&alice_primary_device, &alice_linked_device],
            3_000_000_010,
        ));
    });

    let initial_alice_devices =
        alice_primary.known_device_identity_pubkeys_for_owner(alice_owner.public_key());
    assert!(
        initial_alice_devices.contains(&alice_linked_device.public_key()),
        "primary runtime should know linked device after setup; devices={:?}",
        initial_alice_devices
            .iter()
            .map(PublicKey::to_hex)
            .collect::<Vec<_>>()
    );

    alice_primary
        .send_text(bob_owner.public_key(), "seed bob".to_string(), None)
        .expect("seed bob");
    let seed_bob = drain_published(&alice_primary, &alice_primary_device);
    deliver(&seed_bob, &bob);
    deliver(&seed_bob, &alice_linked);

    alice_primary
        .send_text(charlie_owner.public_key(), "seed charlie".to_string(), None)
        .expect("seed charlie");
    let seed_charlie = drain_published(&alice_primary, &alice_primary_device);
    deliver(&seed_charlie, &charlie);
    deliver(&seed_charlie, &alice_linked);

    let alice_devices =
        alice_primary.known_device_identity_pubkeys_for_owner(alice_owner.public_key());
    assert!(
        alice_devices.contains(&alice_linked_device.public_key()),
        "primary runtime should still know linked device in local roster; devices={:?}",
        alice_devices
            .iter()
            .map(PublicKey::to_hex)
            .collect::<Vec<_>>()
    );
    let linked_session_count = alice_primary.with_group_context(|core, _, _| {
        core.snapshot()
            .users
            .into_iter()
            .find(|user| user.owner_pubkey == owner(alice_owner.public_key()))
            .and_then(|user| {
                user.devices
                    .into_iter()
                    .find(|record| record.device_pubkey == device(alice_linked_device.public_key()))
                    .map(|record| {
                        usize::from(record.active_session.is_some())
                            + record.inactive_sessions.len()
                    })
            })
            .unwrap_or_default()
    });
    assert!(
        (1..=2).contains(&linked_session_count),
        "direct sender-copy fanout may refresh a one-way linked-device bootstrap, but should keep the session set bounded; count={linked_session_count}"
    );

    let created = alice_primary
        .create_group(
            "Alice Bob Charlie".to_string(),
            vec![bob_owner.public_key(), charlie_owner.public_key()],
        )
        .expect("create group");
    let group_events = drain_published(&alice_primary, &alice_primary_device);
    let linked_subscribed_authors = alice_linked
        .get_all_message_push_author_pubkeys()
        .into_iter()
        .collect::<BTreeSet<_>>();
    let linked_device_hex = alice_linked_device.public_key().to_hex();
    let linked_target_count = group_events
        .iter()
        .filter(|published| published.target_device_id.as_deref() == Some(&linked_device_hex))
        .count();
    assert!(
        linked_target_count >= 2,
        "group create should publish metadata and sender-key distribution to linked device; targets={:?}",
        group_events
            .iter()
            .map(|published| published.target_device_id.clone())
            .collect::<Vec<_>>()
    );

    let _ = deliver(&group_events, &bob);
    let _ = deliver(&group_events, &charlie);
    let linked_visible_events = group_events
        .iter()
        .filter(|published| {
            u32::from(published.event.kind.as_u16()) == MESSAGE_EVENT_KIND
                && linked_subscribed_authors.contains(&published.event.pubkey)
        })
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        !linked_visible_events.is_empty(),
        "group local-sibling sync must include at least one event from an author already visible to the linked device subscription; subscribed={:?} event_authors={:?}",
        linked_subscribed_authors
            .iter()
            .map(PublicKey::to_hex)
            .collect::<Vec<_>>(),
        group_events
            .iter()
            .map(|published| published.event.pubkey.to_hex())
            .collect::<Vec<_>>()
    );
    let linked_events = deliver(&linked_visible_events, &alice_linked);

    assert!(
        linked_events
            .iter()
            .any(|event| matches!(event, GroupIncomingEvent::MetadataUpdated(snapshot) if snapshot.group_id == created.group.group_id)),
        "linked device should observe group metadata"
    );
    assert!(
        alice_linked
            .group_snapshots()
            .iter()
            .any(|snapshot| snapshot.group_id == created.group.group_id),
        "linked runtime should retain the group snapshot"
    );
}
