mod support;

use nostr_double_ratchet::{
    GroupIncomingEvent, GroupManagerSnapshot, GroupProtocol, GroupSenderKeyHandleResult,
    GroupSenderKeyMessage, GroupSenderKeyMessageEnvelope, Result, SessionManager, UnixSeconds,
};
use nostr_double_ratchet_nostr::NostrGroupManager as GroupManager;
use serde_json::json;
use support::{
    context, manager_device, manager_observe_invite_response, manager_public_device_invite,
    manager_receive_delivery, roster_for, session_manager, snapshot,
};

struct SenderKeyFixture {
    alice: support::ManagerDevice,
    alice_manager: SessionManager,
    alice_groups: GroupManager,
    bob_groups: GroupManager,
    group_id: String,
}

fn sender_key_message_from_envelope(
    envelope: &GroupSenderKeyMessageEnvelope,
) -> GroupSenderKeyMessage {
    GroupSenderKeyMessage {
        group_id: envelope.group_id.clone(),
        sender_event_pubkey: envelope.sender_event_pubkey,
        key_id: envelope.key_id,
        message_number: envelope.message_number,
        created_at: envelope.created_at,
        ciphertext: envelope.ciphertext.clone(),
    }
}

fn observe_matching_invite_responses(
    manager: &mut nostr_double_ratchet::SessionManager,
    responses: &[nostr_double_ratchet::InviteResponseEnvelope],
    seed: u64,
    now_secs: u64,
) -> Result<()> {
    let recipient = manager
        .snapshot()
        .local_invite
        .expect("local invite must exist before filtering responses")
        .inviter_ephemeral_public_key;
    let mut ctx = context(seed, now_secs);
    for response in responses
        .iter()
        .filter(|response| response.recipient == recipient)
    {
        manager_observe_invite_response(manager, &mut ctx, response)?;
    }
    Ok(())
}

fn deliver_pairwise_group_events_for(
    manager: &mut nostr_double_ratchet::SessionManager,
    groups: &mut GroupManager,
    recipient_owner: nostr_double_ratchet::OwnerPubkey,
    sender_owner: nostr_double_ratchet::OwnerPubkey,
    prepared: &nostr_double_ratchet::GroupPreparedSend,
    seed: u64,
    now_secs: u64,
) -> Result<Vec<GroupIncomingEvent>> {
    let mut ctx = context(seed, now_secs);
    let mut events = Vec::new();
    for delivery in prepared
        .remote
        .deliveries
        .iter()
        .filter(|delivery| delivery.owner_pubkey == recipient_owner)
    {
        if let Some(received) = manager_receive_delivery(manager, &mut ctx, sender_owner, delivery)?
        {
            if let Some(event) = groups.handle_pairwise_payload(
                received.owner_pubkey,
                received.device_pubkey,
                &received.payload,
            )? {
                events.push(event);
            }
        }
    }
    Ok(events)
}

fn deliver_pairwise_group_events(
    manager: &mut nostr_double_ratchet::SessionManager,
    groups: &mut GroupManager,
    sender_owner: nostr_double_ratchet::OwnerPubkey,
    prepared: &nostr_double_ratchet::GroupPreparedSend,
    seed: u64,
    now_secs: u64,
) -> Result<Vec<GroupIncomingEvent>> {
    let mut ctx = context(seed, now_secs);
    let mut events = Vec::new();
    for delivery in prepared.remote.deliveries.iter() {
        if let Some(received) = manager_receive_delivery(manager, &mut ctx, sender_owner, delivery)?
        {
            if let Some(event) = groups.handle_pairwise_payload(
                received.owner_pubkey,
                received.device_pubkey,
                &received.payload,
            )? {
                events.push(event);
            }
        }
    }
    Ok(events)
}

fn established_sender_key_fixture(owner_fill: u8, base_secs: u64) -> Result<SenderKeyFixture> {
    let alice = manager_device(owner_fill, owner_fill.wrapping_add(40));
    let bob = manager_device(owner_fill.wrapping_add(1), owner_fill.wrapping_add(41));
    let mut alice_manager = session_manager(&alice);
    let mut bob_manager = session_manager(&bob);
    let mut alice_groups = GroupManager::new(alice.owner_pubkey);
    let mut bob_groups = GroupManager::new(bob.owner_pubkey);

    bob_manager.observe_peer_roster(alice.owner_pubkey, roster_for(&[&alice], base_secs));
    alice_manager.observe_peer_roster(bob.owner_pubkey, roster_for(&[&bob], base_secs + 1));
    alice_manager.observe_device_invite(
        bob.owner_pubkey,
        manager_public_device_invite(&mut bob_manager, &bob, base_secs + 2, base_secs + 2)?,
    )?;

    let created = alice_groups.create_group_with_protocol(
        &mut alice_manager,
        &mut context(base_secs + 3, base_secs + 3),
        "Sender-key fixture".to_string(),
        vec![bob.owner_pubkey],
        GroupProtocol::sender_key_v1(),
    )?;
    let group_id = created.group.group_id.clone();
    observe_matching_invite_responses(
        &mut bob_manager,
        &created.prepared.remote.invite_responses,
        base_secs + 4,
        base_secs + 4,
    )?;
    let create_events = deliver_pairwise_group_events(
        &mut bob_manager,
        &mut bob_groups,
        alice.owner_pubkey,
        &created.prepared,
        base_secs + 5,
        base_secs + 5,
    )?;
    assert_eq!(create_events.len(), 2);
    assert_eq!(bob_groups.known_sender_event_pubkeys().len(), 1);

    Ok(SenderKeyFixture {
        alice,
        alice_manager,
        alice_groups,
        bob_groups,
        group_id,
    })
}

#[test]
fn sender_key_group_create_distributes_key_and_shared_message_decrypts() -> Result<()> {
    let alice = manager_device(1, 11);
    let bob = manager_device(2, 21);
    let mut alice_manager = session_manager(&alice);
    let mut bob_manager = session_manager(&bob);
    let mut alice_groups = GroupManager::new(alice.owner_pubkey);
    let mut bob_groups = GroupManager::new(bob.owner_pubkey);

    bob_manager.observe_peer_roster(alice.owner_pubkey, roster_for(&[&alice], 10));
    alice_manager.observe_peer_roster(bob.owner_pubkey, roster_for(&[&bob], 11));
    alice_manager.observe_device_invite(
        bob.owner_pubkey,
        manager_public_device_invite(&mut bob_manager, &bob, 12, 1_900_010_000)?,
    )?;

    let mut create_ctx = context(13, 1_900_010_001);
    let created = alice_groups.create_group_with_protocol(
        &mut alice_manager,
        &mut create_ctx,
        "Sender keys".to_string(),
        vec![bob.owner_pubkey],
        GroupProtocol::sender_key_v1(),
    )?;
    assert_eq!(created.group.protocol, GroupProtocol::sender_key_v1());
    assert_eq!(created.prepared.remote.sender_key_messages.len(), 0);
    assert_eq!(created.prepared.remote.deliveries.len(), 2);

    observe_matching_invite_responses(
        &mut bob_manager,
        &created.prepared.remote.invite_responses,
        14,
        1_900_010_002,
    )?;
    let create_events = deliver_pairwise_group_events(
        &mut bob_manager,
        &mut bob_groups,
        alice.owner_pubkey,
        &created.prepared,
        15,
        1_900_010_003,
    )?;
    assert_eq!(create_events.len(), 2);
    assert_eq!(bob_groups.known_sender_event_pubkeys().len(), 1);

    let mut send_ctx = context(16, 1_900_010_004);
    let sent = alice_groups.send_message(
        &mut alice_manager,
        &mut send_ctx,
        &created.group.group_id,
        b"hello sender-key group".to_vec(),
    )?;
    assert_eq!(sent.remote.deliveries.len(), 0);
    assert_eq!(sent.remote.sender_key_messages.len(), 1);

    let outer = sent.remote.sender_key_messages[0].clone();
    let result = bob_groups.handle_sender_key_message(GroupSenderKeyMessage {
        group_id: outer.group_id,
        sender_event_pubkey: outer.sender_event_pubkey,
        key_id: outer.key_id,
        message_number: outer.message_number,
        created_at: outer.created_at,
        ciphertext: outer.ciphertext,
    })?;
    assert!(matches!(
        result,
        GroupSenderKeyHandleResult::Event(GroupIncomingEvent::Message(message))
            if message.group_id == created.group.group_id
                && message.sender_owner == alice.owner_pubkey
                && message.sender_device == Some(alice.device_pubkey)
                && message.body == b"hello sender-key group".to_vec()
    ));

    Ok(())
}

#[test]
fn sender_key_group_create_syncs_to_local_sibling() -> Result<()> {
    let alice1 = manager_device(30, 31);
    let alice2 = manager_device(30, 32);
    let bob = manager_device(33, 34);
    let mut alice1_manager = session_manager(&alice1);
    let mut alice2_manager = session_manager(&alice2);
    let mut bob_manager = session_manager(&bob);
    let mut alice1_groups = GroupManager::new(alice1.owner_pubkey);
    let mut alice2_groups = GroupManager::new(alice2.owner_pubkey);

    alice1_manager.apply_local_roster(roster_for(&[&alice1, &alice2], 40));
    alice2_manager.apply_local_roster(roster_for(&[&alice1, &alice2], 40));
    alice1_manager.observe_device_invite(
        alice1.owner_pubkey,
        manager_public_device_invite(&mut alice2_manager, &alice2, 41, 1_900_030_100)?,
    )?;
    alice1_manager.observe_peer_roster(bob.owner_pubkey, roster_for(&[&bob], 42));
    alice1_manager.observe_device_invite(
        bob.owner_pubkey,
        manager_public_device_invite(&mut bob_manager, &bob, 43, 1_900_030_101)?,
    )?;

    let created = alice1_groups.create_group_with_protocol(
        &mut alice1_manager,
        &mut context(44, 1_900_030_102),
        "Sender-key siblings".to_string(),
        vec![bob.owner_pubkey],
        GroupProtocol::sender_key_v1(),
    )?;

    observe_matching_invite_responses(
        &mut alice2_manager,
        &created.prepared.local_sibling.invite_responses,
        45,
        1_900_030_103,
    )?;

    let mut ctx = context(46, 1_900_030_104);
    let mut events = Vec::new();
    for delivery in created
        .prepared
        .local_sibling
        .deliveries
        .iter()
        .filter(|delivery| delivery.device_pubkey == alice2.device_pubkey)
    {
        if let Some(received) =
            manager_receive_delivery(&mut alice2_manager, &mut ctx, alice1.owner_pubkey, delivery)?
        {
            if let Some(event) = alice2_groups.handle_pairwise_payload(
                received.owner_pubkey,
                received.device_pubkey,
                &received.payload,
            )? {
                events.push(event);
            }
        }
    }

    assert_eq!(events.len(), 2);
    assert!(matches!(
        events.as_slice(),
        [
            GroupIncomingEvent::MetadataUpdated(snapshot),
            GroupIncomingEvent::MetadataUpdated(_)
        ] if snapshot.group_id == created.group.group_id
            && snapshot.protocol == GroupProtocol::sender_key_v1()
    ));
    assert_eq!(alice2_groups.known_sender_event_pubkeys().len(), 1);
    assert_eq!(
        alice2_groups
            .group(&created.group.group_id)
            .expect("local sibling has sender-key group")
            .revision,
        1
    );

    Ok(())
}

#[test]
fn sender_key_outer_message_waits_for_distribution() -> Result<()> {
    let alice = manager_device(3, 31);
    let bob = manager_device(4, 41);
    let mut alice_manager = session_manager(&alice);
    let mut bob_manager = session_manager(&bob);
    let mut alice_groups = GroupManager::new(alice.owner_pubkey);
    let mut bob_groups = GroupManager::new(bob.owner_pubkey);

    bob_manager.observe_peer_roster(alice.owner_pubkey, roster_for(&[&alice], 20));
    alice_manager.observe_peer_roster(bob.owner_pubkey, roster_for(&[&bob], 21));
    alice_manager.observe_device_invite(
        bob.owner_pubkey,
        manager_public_device_invite(&mut bob_manager, &bob, 22, 1_900_020_000)?,
    )?;

    let created = alice_groups.create_group_with_protocol(
        &mut alice_manager,
        &mut context(23, 1_900_020_001),
        "Pending dist".to_string(),
        vec![bob.owner_pubkey],
        GroupProtocol::sender_key_v1(),
    )?;
    observe_matching_invite_responses(
        &mut bob_manager,
        &created.prepared.remote.invite_responses,
        24,
        1_900_020_002,
    )?;

    let first_delivery = created.prepared.remote.deliveries[0].clone();
    let received = manager_receive_delivery(
        &mut bob_manager,
        &mut context(25, 1_900_020_003),
        alice.owner_pubkey,
        &first_delivery,
    )?
    .expect("metadata delivery");
    let _ = bob_groups.handle_pairwise_payload(
        received.owner_pubkey,
        received.device_pubkey,
        &received.payload,
    )?;

    let sent = alice_groups.send_message(
        &mut alice_manager,
        &mut context(26, 1_900_020_004),
        &created.group.group_id,
        b"before distribution".to_vec(),
    )?;
    let outer = sent.remote.sender_key_messages[0].clone();
    let result = bob_groups.handle_sender_key_message(GroupSenderKeyMessage {
        group_id: outer.group_id,
        sender_event_pubkey: outer.sender_event_pubkey,
        key_id: outer.key_id,
        message_number: outer.message_number,
        created_at: outer.created_at,
        ciphertext: outer.ciphertext,
    })?;
    assert!(matches!(
        result,
        GroupSenderKeyHandleResult::PendingDistribution { .. }
    ));

    Ok(())
}

#[test]
fn sender_key_distribution_requires_authenticated_device_provenance() -> Result<()> {
    let alice = manager_device(5, 51);
    let bob = manager_device(6, 61);
    let mut alice_manager = session_manager(&alice);
    let mut bob_manager = session_manager(&bob);
    let mut alice_groups = GroupManager::new(alice.owner_pubkey);
    let mut bob_groups = GroupManager::new(bob.owner_pubkey);

    bob_manager.observe_peer_roster(alice.owner_pubkey, roster_for(&[&alice], 30));
    alice_manager.observe_peer_roster(bob.owner_pubkey, roster_for(&[&bob], 31));
    alice_manager.observe_device_invite(
        bob.owner_pubkey,
        manager_public_device_invite(&mut bob_manager, &bob, 32, 1_900_030_000)?,
    )?;

    let created = alice_groups.create_group_with_protocol(
        &mut alice_manager,
        &mut context(33, 1_900_030_001),
        "Authenticated dist".to_string(),
        vec![bob.owner_pubkey],
        GroupProtocol::sender_key_v1(),
    )?;
    observe_matching_invite_responses(
        &mut bob_manager,
        &created.prepared.remote.invite_responses,
        34,
        1_900_030_002,
    )?;

    let metadata = manager_receive_delivery(
        &mut bob_manager,
        &mut context(35, 1_900_030_003),
        alice.owner_pubkey,
        &created.prepared.remote.deliveries[0],
    )?
    .expect("metadata delivery");
    let _ = bob_groups.handle_pairwise_payload(
        metadata.owner_pubkey,
        metadata.device_pubkey,
        &metadata.payload,
    )?;

    let distribution = manager_receive_delivery(
        &mut bob_manager,
        &mut context(36, 1_900_030_004),
        alice.owner_pubkey,
        &created.prepared.remote.deliveries[1],
    )?
    .expect("sender-key distribution delivery");
    let unauthenticated =
        bob_groups.handle_incoming(distribution.owner_pubkey, &distribution.payload);

    assert!(unauthenticated.is_err());
    assert!(bob_groups.known_sender_event_pubkeys().is_empty());

    Ok(())
}

#[test]
fn sender_key_corrupted_outer_message_does_not_advance_receiver() -> Result<()> {
    let mut fixture = established_sender_key_fixture(10, 1_900_040_000)?;

    let first = fixture.alice_groups.send_message(
        &mut fixture.alice_manager,
        &mut context(1_900_040_010, 1_900_040_010),
        &fixture.group_id,
        b"first valid".to_vec(),
    )?;
    let second = fixture.alice_groups.send_message(
        &mut fixture.alice_manager,
        &mut context(1_900_040_011, 1_900_040_011),
        &fixture.group_id,
        b"second valid".to_vec(),
    )?;

    let mut corrupted = sender_key_message_from_envelope(&first.remote.sender_key_messages[0]);
    let last = corrupted
        .ciphertext
        .last_mut()
        .expect("ciphertext must not be empty");
    *last ^= 0x44;
    let before = snapshot(&fixture.bob_groups.snapshot());

    assert!(fixture
        .bob_groups
        .handle_sender_key_message(corrupted)
        .is_err());
    assert_eq!(snapshot(&fixture.bob_groups.snapshot()), before);

    let first_result =
        fixture
            .bob_groups
            .handle_sender_key_message(sender_key_message_from_envelope(
                &first.remote.sender_key_messages[0],
            ))?;
    assert!(matches!(
        first_result,
        GroupSenderKeyHandleResult::Event(GroupIncomingEvent::Message(message))
            if message.body == b"first valid".to_vec()
    ));

    let second_result =
        fixture
            .bob_groups
            .handle_sender_key_message(sender_key_message_from_envelope(
                &second.remote.sender_key_messages[0],
            ))?;
    assert!(matches!(
        second_result,
        GroupSenderKeyHandleResult::Event(GroupIncomingEvent::Message(message))
            if message.body == b"second valid".to_vec()
    ));

    Ok(())
}

#[test]
fn sender_key_duplicate_outer_message_is_rejected_without_losing_next_message() -> Result<()> {
    let mut fixture = established_sender_key_fixture(12, 1_900_041_000)?;

    let first = fixture.alice_groups.send_message(
        &mut fixture.alice_manager,
        &mut context(1_900_041_010, 1_900_041_010),
        &fixture.group_id,
        b"once".to_vec(),
    )?;
    let second = fixture.alice_groups.send_message(
        &mut fixture.alice_manager,
        &mut context(1_900_041_011, 1_900_041_011),
        &fixture.group_id,
        b"after duplicate".to_vec(),
    )?;
    let first_message = sender_key_message_from_envelope(&first.remote.sender_key_messages[0]);

    let first_result = fixture
        .bob_groups
        .handle_sender_key_message(first_message.clone())?;
    assert!(matches!(
        first_result,
        GroupSenderKeyHandleResult::Event(GroupIncomingEvent::Message(message))
            if message.body == b"once".to_vec()
    ));
    let after_first = snapshot(&fixture.bob_groups.snapshot());

    assert!(fixture
        .bob_groups
        .handle_sender_key_message(first_message)
        .is_err());
    assert_eq!(snapshot(&fixture.bob_groups.snapshot()), after_first);

    let second_result =
        fixture
            .bob_groups
            .handle_sender_key_message(sender_key_message_from_envelope(
                &second.remote.sender_key_messages[0],
            ))?;
    assert!(matches!(
        second_result,
        GroupSenderKeyHandleResult::Event(GroupIncomingEvent::Message(message))
            if message.body == b"after duplicate".to_vec()
    ));

    Ok(())
}

#[test]
fn sender_key_unknown_key_id_is_pending_without_mutating_state() -> Result<()> {
    let mut fixture = established_sender_key_fixture(14, 1_900_042_000)?;
    let sent = fixture.alice_groups.send_message(
        &mut fixture.alice_manager,
        &mut context(1_900_042_010, 1_900_042_010),
        &fixture.group_id,
        b"known sender unknown key".to_vec(),
    )?;
    let mut message = sender_key_message_from_envelope(&sent.remote.sender_key_messages[0]);
    message.key_id = message.key_id.wrapping_add(1);
    let before = snapshot(&fixture.bob_groups.snapshot());

    let result = fixture.bob_groups.handle_sender_key_message(message)?;

    assert!(matches!(
        result,
        GroupSenderKeyHandleResult::PendingDistribution { .. }
    ));
    assert_eq!(snapshot(&fixture.bob_groups.snapshot()), before);

    Ok(())
}

#[test]
fn sender_key_valid_ciphertext_with_invalid_group_plaintext_does_not_burn_message_number(
) -> Result<()> {
    let mut fixture = established_sender_key_fixture(16, 1_900_043_000)?;
    let receiver_snapshot = fixture.bob_groups.snapshot();
    let sender_record = receiver_snapshot
        .sender_keys
        .iter()
        .find(|record| record.group_id == fixture.group_id)
        .expect("sender-key record");
    let key_id = sender_record.latest_key_id.expect("latest sender key");
    let mut forged_sender_state = sender_record
        .states
        .iter()
        .find(|state| state.key_id() == key_id)
        .expect("sender-key state")
        .clone();
    let forged_plaintext = serde_json::to_vec(&json!({
        "wire_format_version": 1,
        "group_id": fixture.group_id,
        "revision": 999,
        "body": [102, 111, 114, 103, 101, 100]
    }))
    .expect("forged plaintext json");
    let (message_number, ciphertext) = forged_sender_state
        .encrypt_to_bytes(&forged_plaintext)
        .expect("forge sender-key ciphertext");
    let forged = GroupSenderKeyMessage {
        group_id: fixture.group_id.clone(),
        sender_event_pubkey: sender_record.sender_event_pubkey,
        key_id,
        message_number,
        created_at: UnixSeconds(1_900_043_010),
        ciphertext,
    };
    let before = snapshot(&fixture.bob_groups.snapshot());

    let result = fixture
        .bob_groups
        .handle_sender_key_message(forged)
        .expect("future revision should be queued, not rejected");

    assert!(matches!(
        result,
        GroupSenderKeyHandleResult::PendingRevision {
            required_revision: 999,
            ..
        }
    ));
    assert_eq!(snapshot(&fixture.bob_groups.snapshot()), before);

    let legitimate = fixture.alice_groups.send_message(
        &mut fixture.alice_manager,
        &mut context(1_900_043_011, 1_900_043_011),
        &fixture.group_id,
        b"legitimate after forged".to_vec(),
    )?;
    let result = fixture
        .bob_groups
        .handle_sender_key_message(sender_key_message_from_envelope(
            &legitimate.remote.sender_key_messages[0],
        ))?;
    assert!(matches!(
        result,
        GroupSenderKeyHandleResult::Event(GroupIncomingEvent::Message(message))
            if message.body == b"legitimate after forged".to_vec()
    ));

    Ok(())
}

#[test]
fn sender_key_group_manager_snapshot_roundtrip_preserves_pending_decrypt_state() -> Result<()> {
    let mut fixture = established_sender_key_fixture(18, 1_900_044_000)?;
    let sent = fixture.alice_groups.send_message(
        &mut fixture.alice_manager,
        &mut context(1_900_044_010, 1_900_044_010),
        &fixture.group_id,
        b"after restore".to_vec(),
    )?;
    let json = snapshot(&fixture.bob_groups.snapshot());
    let restored_snapshot: GroupManagerSnapshot = serde_json::from_str(&json).unwrap();
    let mut restored = GroupManager::from_snapshot(restored_snapshot)?;

    let result = restored.handle_sender_key_message(sender_key_message_from_envelope(
        &sent.remote.sender_key_messages[0],
    ))?;

    assert!(matches!(
        result,
        GroupSenderKeyHandleResult::Event(GroupIncomingEvent::Message(message))
            if message.body == b"after restore".to_vec()
                && message.sender_device == Some(fixture.alice.device_pubkey)
    ));

    Ok(())
}

#[test]
fn sender_key_added_member_receives_distribution_at_current_iteration() -> Result<()> {
    let mut fixture = established_sender_key_fixture(20, 1_900_045_000)?;
    let carol = manager_device(22, 62);
    let mut carol_manager = session_manager(&carol);
    let mut carol_groups = GroupManager::new(carol.owner_pubkey);

    carol_manager.observe_peer_roster(
        fixture.alice.owner_pubkey,
        roster_for(&[&fixture.alice], 1_900_045_010),
    );
    fixture
        .alice_manager
        .observe_peer_roster(carol.owner_pubkey, roster_for(&[&carol], 1_900_045_011));
    fixture.alice_manager.observe_device_invite(
        carol.owner_pubkey,
        manager_public_device_invite(&mut carol_manager, &carol, 1_900_045_012, 1_900_045_012)?,
    )?;

    let first = fixture.alice_groups.send_message(
        &mut fixture.alice_manager,
        &mut context(1_900_045_013, 1_900_045_013),
        &fixture.group_id,
        b"before carol".to_vec(),
    )?;
    let first_result =
        fixture
            .bob_groups
            .handle_sender_key_message(sender_key_message_from_envelope(
                &first.remote.sender_key_messages[0],
            ))?;
    assert!(matches!(
        first_result,
        GroupSenderKeyHandleResult::Event(GroupIncomingEvent::Message(message))
            if message.body == b"before carol".to_vec()
    ));

    let added = fixture.alice_groups.add_members(
        &mut fixture.alice_manager,
        &mut context(1_900_045_014, 1_900_045_014),
        &fixture.group_id,
        vec![carol.owner_pubkey],
    )?;
    observe_matching_invite_responses(
        &mut carol_manager,
        &added.remote.invite_responses,
        1_900_045_015,
        1_900_045_015,
    )?;
    let carol_events = deliver_pairwise_group_events_for(
        &mut carol_manager,
        &mut carol_groups,
        carol.owner_pubkey,
        fixture.alice.owner_pubkey,
        &added,
        1_900_045_016,
        1_900_045_016,
    )?;
    assert_eq!(carol_events.len(), 2);

    let future = fixture.alice_groups.send_message(
        &mut fixture.alice_manager,
        &mut context(1_900_045_017, 1_900_045_017),
        &fixture.group_id,
        b"welcome carol".to_vec(),
    )?;
    let result = carol_groups.handle_sender_key_message(sender_key_message_from_envelope(
        &future.remote.sender_key_messages[0],
    ))?;

    assert!(matches!(
        result,
        GroupSenderKeyHandleResult::Event(GroupIncomingEvent::Message(message))
            if message.body == b"welcome carol".to_vec()
                && message.sender_device == Some(fixture.alice.device_pubkey)
    ));

    Ok(())
}

#[test]
fn sender_key_removed_member_does_not_receive_rotated_future_key() -> Result<()> {
    let alice = manager_device(30, 70);
    let bob = manager_device(31, 71);
    let carol = manager_device(32, 72);
    let mut alice_manager = session_manager(&alice);
    let mut bob_manager = session_manager(&bob);
    let mut carol_manager = session_manager(&carol);
    let mut alice_groups = GroupManager::new(alice.owner_pubkey);
    let mut bob_groups = GroupManager::new(bob.owner_pubkey);
    let mut carol_groups = GroupManager::new(carol.owner_pubkey);

    bob_manager.observe_peer_roster(alice.owner_pubkey, roster_for(&[&alice], 1_900_046_000));
    carol_manager.observe_peer_roster(alice.owner_pubkey, roster_for(&[&alice], 1_900_046_001));
    alice_manager.observe_peer_roster(bob.owner_pubkey, roster_for(&[&bob], 1_900_046_002));
    alice_manager.observe_peer_roster(carol.owner_pubkey, roster_for(&[&carol], 1_900_046_003));
    alice_manager.observe_device_invite(
        bob.owner_pubkey,
        manager_public_device_invite(&mut bob_manager, &bob, 1_900_046_004, 1_900_046_004)?,
    )?;
    alice_manager.observe_device_invite(
        carol.owner_pubkey,
        manager_public_device_invite(&mut carol_manager, &carol, 1_900_046_005, 1_900_046_005)?,
    )?;

    let created = alice_groups.create_group_with_protocol(
        &mut alice_manager,
        &mut context(1_900_046_006, 1_900_046_006),
        "Remove rotation".to_string(),
        vec![bob.owner_pubkey, carol.owner_pubkey],
        GroupProtocol::sender_key_v1(),
    )?;
    observe_matching_invite_responses(
        &mut bob_manager,
        &created.prepared.remote.invite_responses,
        1_900_046_007,
        1_900_046_007,
    )?;
    observe_matching_invite_responses(
        &mut carol_manager,
        &created.prepared.remote.invite_responses,
        1_900_046_008,
        1_900_046_008,
    )?;
    assert_eq!(
        deliver_pairwise_group_events_for(
            &mut bob_manager,
            &mut bob_groups,
            bob.owner_pubkey,
            alice.owner_pubkey,
            &created.prepared,
            1_900_046_009,
            1_900_046_009,
        )?
        .len(),
        2
    );
    assert_eq!(
        deliver_pairwise_group_events_for(
            &mut carol_manager,
            &mut carol_groups,
            carol.owner_pubkey,
            alice.owner_pubkey,
            &created.prepared,
            1_900_046_010,
            1_900_046_010,
        )?
        .len(),
        2
    );

    let removed = alice_groups.remove_members(
        &mut alice_manager,
        &mut context(1_900_046_011, 1_900_046_011),
        &created.group.group_id,
        vec![carol.owner_pubkey],
    )?;
    assert_eq!(
        deliver_pairwise_group_events_for(
            &mut bob_manager,
            &mut bob_groups,
            bob.owner_pubkey,
            alice.owner_pubkey,
            &removed,
            1_900_046_012,
            1_900_046_012,
        )?
        .len(),
        2
    );
    assert_eq!(
        deliver_pairwise_group_events_for(
            &mut carol_manager,
            &mut carol_groups,
            carol.owner_pubkey,
            alice.owner_pubkey,
            &removed,
            1_900_046_013,
            1_900_046_013,
        )?
        .len(),
        1
    );

    let future = alice_groups.send_message(
        &mut alice_manager,
        &mut context(1_900_046_014, 1_900_046_014),
        &created.group.group_id,
        b"after removal".to_vec(),
    )?;
    let future_message = sender_key_message_from_envelope(&future.remote.sender_key_messages[0]);
    let bob_result = bob_groups.handle_sender_key_message(future_message.clone())?;
    assert!(matches!(
        bob_result,
        GroupSenderKeyHandleResult::Event(GroupIncomingEvent::Message(message))
            if message.body == b"after removal".to_vec()
    ));

    let carol_result = carol_groups.handle_sender_key_message(future_message)?;
    assert!(matches!(
        carol_result,
        GroupSenderKeyHandleResult::PendingDistribution { .. }
    ));

    Ok(())
}
