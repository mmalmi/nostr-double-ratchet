mod support;

use nostr_double_ratchet::{DomainError, Error, GroupIncomingEvent, GroupProtocol, Result};
use nostr_double_ratchet_nostr::NostrGroupManager as GroupManager;
use support::{context, manager_device, roster_for, session_manager};

fn create_remote_owned_group(
    local_owner: nostr_double_ratchet::OwnerPubkey,
    remote_owner: nostr_double_ratchet::OwnerPubkey,
) -> Result<(GroupManager, String)> {
    let mut groups = GroupManager::new(local_owner);
    let payload = serde_json::to_vec(&serde_json::json!({
        "wire_format_version": 1,
        "payload": {
            "kind": "create_group",
            "group_id": "group-1",
            "protocol": "pairwise_fanout_v1",
            "base_revision": 0,
            "new_revision": 1,
            "name": "Remote",
            "created_by": remote_owner,
            "members": [remote_owner, local_owner],
            "admins": [remote_owner],
            "created_at": 1_900_001_000u64,
            "updated_at": 1_900_001_000u64
        }
    }))?;
    let event = groups.handle_incoming(remote_owner, &payload)?;
    assert!(matches!(
        event,
        Some(GroupIncomingEvent::MetadataUpdated(snapshot)) if snapshot.protocol == GroupProtocol::PairwiseFanoutV1
    ));
    Ok((groups, "group-1".to_string()))
}

#[test]
fn non_admin_local_owner_cannot_mutate_remote_owned_group() -> Result<()> {
    let alice = manager_device(1, 11);
    let bob = manager_device(2, 21);
    let mut session_manager = session_manager(&alice);
    let (mut groups, group_id) = create_remote_owned_group(alice.owner_pubkey, bob.owner_pubkey)?;
    let mut ctx = context(1, 1_900_001_010);

    let rename = groups.update_name(
        &mut session_manager,
        &mut ctx,
        &group_id,
        "Nope".to_string(),
    );
    assert!(matches!(
        rename,
        Err(Error::Domain(DomainError::InvalidGroupOperation(message)))
            if message.contains("admin")
    ));
    Ok(())
}

#[test]
fn duplicate_and_invalid_membership_mutations_are_rejected() -> Result<()> {
    let alice = manager_device(3, 31);
    let bob = manager_device(4, 41);
    let mut session_manager = session_manager(&alice);
    let mut groups = GroupManager::new(alice.owner_pubkey);

    let mut create_ctx = context(2, 1_900_001_020);
    let created = groups.create_group(
        &mut session_manager,
        &mut create_ctx,
        "Crew".to_string(),
        vec![bob.owner_pubkey],
    )?;

    let mut add_ctx = context(3, 1_900_001_021);
    let duplicate_add = groups.add_members(
        &mut session_manager,
        &mut add_ctx,
        &created.group.group_id,
        vec![bob.owner_pubkey],
    );
    assert!(matches!(
        duplicate_add,
        Err(Error::Domain(DomainError::InvalidGroupOperation(message)))
            if message.contains("already a member")
    ));

    let mut remove_self_ctx = context(4, 1_900_001_022);
    let remove_self = groups.remove_members(
        &mut session_manager,
        &mut remove_self_ctx,
        &created.group.group_id,
        vec![alice.owner_pubkey],
    );
    assert!(matches!(
        remove_self,
        Err(Error::Domain(DomainError::InvalidGroupOperation(message)))
            if message.contains("self-removal")
    ));
    Ok(())
}

#[test]
fn removing_last_admin_and_promoting_non_member_are_rejected() -> Result<()> {
    let alice = manager_device(5, 51);
    let bob = manager_device(6, 61);
    let carol = manager_device(7, 71);
    let mut session_manager = session_manager(&alice);
    let mut groups = GroupManager::new(alice.owner_pubkey);

    let mut create_ctx = context(5, 1_900_001_030);
    let created = groups.create_group(
        &mut session_manager,
        &mut create_ctx,
        "Admins".to_string(),
        vec![bob.owner_pubkey],
    )?;

    let mut demote_ctx = context(6, 1_900_001_031);
    let demote_last_admin = groups.remove_admins(
        &mut session_manager,
        &mut demote_ctx,
        &created.group.group_id,
        vec![alice.owner_pubkey],
    );
    assert!(matches!(
        demote_last_admin,
        Err(Error::Domain(DomainError::InvalidGroupOperation(message)))
            if message.contains("last admin")
    ));

    let mut promote_ctx = context(7, 1_900_001_032);
    let promote_non_member = groups.add_admins(
        &mut session_manager,
        &mut promote_ctx,
        &created.group.group_id,
        vec![carol.owner_pubkey],
    );
    assert!(matches!(
        promote_non_member,
        Err(Error::Domain(DomainError::InvalidGroupOperation(message)))
            if message.contains("member")
    ));
    Ok(())
}

#[test]
fn incoming_control_from_non_admin_and_wrong_revision_message_are_rejected() -> Result<()> {
    let alice = manager_device(8, 81);
    let bob = manager_device(9, 91);
    let carol = manager_device(10, 101);
    let (mut groups, group_id) = create_remote_owned_group(alice.owner_pubkey, bob.owner_pubkey)?;

    let unauthorized_rename = serde_json::to_vec(&serde_json::json!({
        "wire_format_version": 1,
        "payload": {
            "kind": "rename_group",
            "group_id": group_id,
            "base_revision": 1,
            "new_revision": 2,
            "name": "Hijack"
        }
    }))?;
    let rename = groups.handle_incoming(carol.owner_pubkey, &unauthorized_rename);
    assert!(matches!(
        rename,
        Err(Error::Domain(DomainError::InvalidGroupOperation(message)))
            if message.contains("admin")
    ));

    let wrong_revision_message = serde_json::to_vec(&serde_json::json!({
        "wire_format_version": 1,
        "payload": {
            "kind": "group_message",
            "group_id": group_id,
            "revision": 9,
            "body": [104, 105]
        }
    }))?;
    let message = groups.handle_incoming(bob.owner_pubkey, &wrong_revision_message);
    assert!(matches!(
        message,
        Err(Error::Domain(DomainError::PendingGroupRevision {
            required_revision: 9,
            ..
        }))
    ));
    Ok(())
}

#[test]
fn duplicate_create_is_idempotent_and_unknown_group_message_is_rejected() -> Result<()> {
    let alice = manager_device(11, 111);
    let bob = manager_device(12, 121);
    let (mut groups, _group_id) = create_remote_owned_group(alice.owner_pubkey, bob.owner_pubkey)?;

    let duplicate_create = serde_json::to_vec(&serde_json::json!({
        "wire_format_version": 1,
        "payload": {
            "kind": "create_group",
            "group_id": "group-1",
            "protocol": "pairwise_fanout_v1",
            "base_revision": 0,
            "new_revision": 1,
            "name": "Remote",
            "created_by": bob.owner_pubkey,
            "members": [bob.owner_pubkey, alice.owner_pubkey],
            "admins": [bob.owner_pubkey],
            "created_at": 1_900_001_000u64,
            "updated_at": 1_900_001_000u64
        }
    }))?;
    let duplicate = groups.handle_incoming(bob.owner_pubkey, &duplicate_create)?;
    assert!(
        matches!(duplicate, Some(GroupIncomingEvent::MetadataUpdated(snapshot)) if snapshot.group_id == "group-1")
    );

    let unknown_message = serde_json::to_vec(&serde_json::json!({
        "wire_format_version": 1,
        "payload": {
            "kind": "group_message",
            "group_id": "missing-group",
            "revision": 1,
            "body": [111, 111, 112, 115]
        }
    }))?;
    let missing = groups.handle_incoming(bob.owner_pubkey, &unknown_message);
    assert!(matches!(
        missing,
        Err(Error::Domain(DomainError::InvalidGroupOperation(message)))
            if message.contains("unknown group")
    ));
    Ok(())
}

#[test]
fn duplicate_rename_is_idempotent() -> Result<()> {
    let alice = manager_device(15, 151);
    let bob = manager_device(16, 161);
    let (mut groups, group_id) = create_remote_owned_group(alice.owner_pubkey, bob.owner_pubkey)?;

    let rename = serde_json::to_vec(&serde_json::json!({
        "wire_format_version": 1,
        "payload": {
            "kind": "rename_group",
            "group_id": group_id,
            "base_revision": 1,
            "new_revision": 2,
            "name": "Renamed"
        }
    }))?;

    let first = groups.handle_incoming(bob.owner_pubkey, &rename)?;
    let second = groups.handle_incoming(bob.owner_pubkey, &rename)?;

    assert!(
        matches!(first, Some(GroupIncomingEvent::MetadataUpdated(snapshot)) if snapshot.revision == 2 && snapshot.name == "Renamed")
    );
    assert!(
        matches!(second, Some(GroupIncomingEvent::MetadataUpdated(snapshot)) if snapshot.revision == 2 && snapshot.name == "Renamed")
    );
    Ok(())
}

#[test]
fn create_and_sync_reject_unknown_protocol() -> Result<()> {
    let alice = manager_device(19, 191);
    let bob = manager_device(20, 201);
    let (mut groups, group_id) = create_remote_owned_group(alice.owner_pubkey, bob.owner_pubkey)?;

    let unknown_create = serde_json::to_vec(&serde_json::json!({
        "wire_format_version": 1,
        "payload": {
            "kind": "create_group",
            "group_id": group_id,
            "protocol": "future_group_v1",
            "base_revision": 0,
            "new_revision": 1,
            "name": "Remote",
            "created_by": bob.owner_pubkey,
            "members": [bob.owner_pubkey, alice.owner_pubkey],
            "admins": [bob.owner_pubkey],
            "created_at": 1_900_001_000u64,
            "updated_at": 1_900_001_000u64
        }
    }))?;
    let create = groups.handle_incoming(bob.owner_pubkey, &unknown_create);
    assert!(matches!(create, Ok(None)));

    let unknown_sync = serde_json::to_vec(&serde_json::json!({
        "wire_format_version": 1,
        "payload": {
            "kind": "sync_group",
            "group_id": "group-1",
            "protocol": "future_group_v1",
            "revision": 1,
            "name": "Remote",
            "created_by": bob.owner_pubkey,
            "members": [bob.owner_pubkey, alice.owner_pubkey],
            "admins": [bob.owner_pubkey],
            "created_at": 1_900_001_000u64,
            "updated_at": 1_900_001_000u64
        }
    }))?;
    let sync = groups.handle_incoming(alice.owner_pubkey, &unknown_sync);
    assert!(matches!(sync, Ok(None)));
    Ok(())
}

#[test]
fn removed_member_cannot_send_after_processing_removal() -> Result<()> {
    let alice = manager_device(13, 131);
    let bob = manager_device(14, 141);
    let mut alice_manager = session_manager(&alice);
    let mut bob_manager = session_manager(&bob);
    let mut alice_groups = GroupManager::new(alice.owner_pubkey);
    let mut bob_groups = GroupManager::new(bob.owner_pubkey);

    bob_manager.observe_peer_roster(alice.owner_pubkey, roster_for(&[&alice], 49));
    alice_manager.observe_peer_roster(bob.owner_pubkey, roster_for(&[&bob], 50));
    alice_manager.observe_device_invite(
        bob.owner_pubkey,
        support::manager_public_device_invite(&mut bob_manager, &bob, 8, 1_900_001_040)?,
    )?;

    let mut create_ctx = context(8, 1_900_001_041);
    let created = alice_groups.create_group(
        &mut alice_manager,
        &mut create_ctx,
        "Revoked".to_string(),
        vec![bob.owner_pubkey],
    )?;

    let bob_recipient = bob_manager
        .snapshot()
        .local_invite
        .expect("bob local invite exists")
        .inviter_ephemeral_public_key;
    let mut observe_ctx = context(9, 1_900_001_042);
    for response in created
        .prepared
        .remote
        .invite_responses
        .iter()
        .filter(|response| response.recipient == bob_recipient)
    {
        support::manager_observe_invite_response(&mut bob_manager, &mut observe_ctx, response)?;
    }
    let mut deliver_ctx = context(10, 1_900_001_043);
    for delivery in created
        .prepared
        .remote
        .deliveries
        .iter()
        .filter(|delivery| delivery.device_pubkey == bob.device_pubkey)
    {
        let received = support::manager_receive_delivery(
            &mut bob_manager,
            &mut deliver_ctx,
            alice.owner_pubkey,
            delivery,
        )?
        .expect("group create delivery");
        bob_groups.handle_incoming(received.owner_pubkey, &received.payload)?;
    }

    let mut remove_ctx = context(11, 1_900_001_044);
    let removal = alice_groups.remove_members(
        &mut alice_manager,
        &mut remove_ctx,
        &created.group.group_id,
        vec![bob.owner_pubkey],
    )?;
    let mut removal_deliver_ctx = context(12, 1_900_001_045);
    for delivery in removal
        .remote
        .deliveries
        .iter()
        .filter(|delivery| delivery.device_pubkey == bob.device_pubkey)
    {
        let received = support::manager_receive_delivery(
            &mut bob_manager,
            &mut removal_deliver_ctx,
            alice.owner_pubkey,
            delivery,
        )?
        .expect("removal delivery");
        bob_groups.handle_incoming(received.owner_pubkey, &received.payload)?;
    }

    let mut bob_send_ctx = context(13, 1_900_001_046);
    let send = bob_groups.send_message(
        &mut bob_manager,
        &mut bob_send_ctx,
        &created.group.group_id,
        b"still here".to_vec(),
    );
    assert!(matches!(
        send,
        Err(Error::Domain(DomainError::InvalidGroupOperation(message)))
            if message.contains("member")
    ));
    Ok(())
}
