#![cfg(any())]

use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Receiver;

use nostr::{JsonUtil, Keys, PublicKey, UnsignedEvent};
use nostr_double_ratchet::{
    AppKeys, DeviceEntry, FileStorageAdapter, InMemoryStorage, Invite, Result, SendOptions,
    Session, SessionManager, SessionManagerEvent, StorageAdapter, MESSAGE_EVENT_KIND,
};

const EVENT_TIMEOUT: Duration = Duration::from_secs(5);
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(100);

fn load_stored_user_record(
    storage_root: &std::path::Path,
    owner_pubkey: PublicKey,
) -> nostr_double_ratchet::StoredUserRecord {
    let path = storage_root.join(format!("user_{}.json", owner_pubkey.to_hex()));
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()))
}

fn device_has_receiving_session(
    stored: &nostr_double_ratchet::StoredUserRecord,
    device_id: &str,
) -> bool {
    let Some(device) = stored
        .devices
        .iter()
        .find(|device| device.device_id == device_id)
    else {
        return false;
    };

    device
        .active_session
        .iter()
        .chain(device.inactive_sessions.iter())
        .any(|session| {
            session.receiving_chain_key.is_some()
                || session.their_current_nostr_public_key.is_some()
                || session.receiving_chain_message_number > 0
        })
}

fn device_has_send_capable_session(
    stored: &nostr_double_ratchet::StoredUserRecord,
    device_id: &str,
) -> bool {
    let Some(device) = stored
        .devices
        .iter()
        .find(|device| device.device_id == device_id)
    else {
        return false;
    };

    device
        .active_session
        .iter()
        .chain(device.inactive_sessions.iter())
        .any(|session| Session::new(session.clone(), "debug".to_string()).can_send())
}

fn recv_signed_event(rx: &Receiver<SessionManagerEvent>) -> nostr::Event {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > EVENT_TIMEOUT {
            panic!("Timed out waiting for PublishSigned event");
        }
        if let Ok(event) = rx.recv_timeout(EVENT_POLL_INTERVAL) {
            match event {
                SessionManagerEvent::PublishSigned(signed)
                | SessionManagerEvent::PublishSignedForInnerEvent { event: signed, .. } => {
                    return signed;
                }
                _ => {}
            }
        }
    }
}

fn recv_signed_event_of_kind(rx: &Receiver<SessionManagerEvent>, kind: u32) -> nostr::Event {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > EVENT_TIMEOUT {
            panic!("Timed out waiting for PublishSigned event of kind {kind}");
        }
        if let Ok(event) = rx.recv_timeout(EVENT_POLL_INTERVAL) {
            match event {
                SessionManagerEvent::PublishSigned(signed)
                | SessionManagerEvent::PublishSignedForInnerEvent { event: signed, .. } => {
                    if signed.kind.as_u16() == kind as u16 {
                        return signed;
                    }
                }
                _ => {}
            }
        }
    }
}

fn recv_message_events(rx: &Receiver<SessionManagerEvent>, expected: usize) -> Vec<nostr::Event> {
    let start = std::time::Instant::now();
    let mut events = Vec::new();

    while start.elapsed() <= EVENT_TIMEOUT {
        if let Ok(event) = rx.recv_timeout(EVENT_POLL_INTERVAL) {
            match event {
                SessionManagerEvent::PublishSigned(signed)
                | SessionManagerEvent::PublishSignedForInnerEvent { event: signed, .. } => {
                    if signed.kind.as_u16() == MESSAGE_EVENT_KIND as u16 {
                        events.push(signed);
                        if events.len() >= expected {
                            return events;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    panic!(
        "Timed out waiting for {expected} PublishSigned message events, got {}",
        events.len()
    );
}

fn recv_decrypted_containing(rx: &Receiver<SessionManagerEvent>, needle: &str) -> String {
    let start = std::time::Instant::now();
    while start.elapsed() <= EVENT_TIMEOUT {
        if let Ok(SessionManagerEvent::DecryptedMessage { content, .. }) =
            rx.recv_timeout(EVENT_POLL_INTERVAL)
        {
            if content.contains(needle) {
                return content;
            }
        }
    }

    panic!("Timed out waiting for decrypted message containing {needle}");
}

fn drain_events(rx: &Receiver<SessionManagerEvent>) {
    while rx.try_recv().is_ok() {}
}

fn recv_invite_response_and_message_event(
    rx: &Receiver<SessionManagerEvent>,
) -> (nostr::Event, nostr::Event) {
    let start = std::time::Instant::now();
    let mut response = None;
    let mut message = None;

    while start.elapsed() <= EVENT_TIMEOUT {
        if let Ok(event) = rx.recv_timeout(EVENT_POLL_INTERVAL) {
            let signed = match event {
                SessionManagerEvent::PublishSigned(signed)
                | SessionManagerEvent::PublishSignedForInnerEvent { event: signed, .. } => signed,
                _ => continue,
            };
            if signed.kind.as_u16() == nostr_double_ratchet::INVITE_RESPONSE_KIND as u16 {
                response = Some(signed);
            } else if signed.kind.as_u16() == MESSAGE_EVENT_KIND as u16 {
                message = Some(signed);
            }

            if let (Some(response), Some(message)) = (response.clone(), message.clone()) {
                return (response, message);
            }
        }
    }

    panic!(
        "Timed out waiting for invite response + bootstrap message: response={}, message={}",
        response.is_some(),
        message.is_some()
    );
}

fn import_session_from_response(
    invite: &Invite,
    inviter_private_key: [u8; 32],
    manager: &SessionManager,
    response_event: &nostr::Event,
) -> Result<(PublicKey, String)> {
    let response = invite
        .process_invite_response(response_event, inviter_private_key)?
        .expect("expected invite response payload");
    let owner_pubkey = response.resolved_owner_pubkey();
    let device_id = response
        .device_id
        .clone()
        .unwrap_or_else(|| response.invitee_identity.to_hex());
    manager.import_session_state(
        owner_pubkey,
        Some(device_id.clone()),
        response.session.state,
    )?;
    Ok((owner_pubkey, device_id))
}

fn new_link_invite(
    device_pubkey: PublicKey,
    device_id: String,
    owner_pubkey: PublicKey,
) -> Result<Invite> {
    let mut invite = Invite::create_new(device_pubkey, Some(device_id), None)?;
    invite.purpose = Some("link".to_string());
    invite.owner_public_key = Some(owner_pubkey);
    Ok(invite)
}

fn new_public_invite(
    device_pubkey: PublicKey,
    device_id: String,
    owner_pubkey: PublicKey,
) -> Result<Invite> {
    let mut invite = Invite::create_new(device_pubkey, Some(device_id), None)?;
    invite.owner_public_key = Some(owner_pubkey);
    Ok(invite)
}

#[test]
fn test_accept_invite_routes_session_under_verified_claimed_owner() -> Result<()> {
    let alice_keys = Keys::generate();
    let alice_owner = alice_keys.public_key();

    let bob_owner_keys = Keys::generate();
    let bob_owner = bob_owner_keys.public_key();
    let bob_device_keys = Keys::generate();
    let bob_device = bob_device_keys.public_key();
    let bob_device_id = bob_device.to_hex();

    let mut invite = Invite::create_new(bob_device, Some(bob_device_id.clone()), None)?;
    invite.owner_public_key = Some(bob_owner);

    let (tx, rx) = crossbeam_channel::unbounded();
    let manager = SessionManager::new(
        alice_keys.public_key(),
        alice_keys.secret_key().to_secret_bytes(),
        alice_keys.public_key().to_hex(),
        alice_owner,
        tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        None,
    );
    manager.init()?;
    drain_events(&rx);

    let bob_app_keys_event = AppKeys::new(vec![DeviceEntry::new(bob_device, 1)])
        .get_event(bob_owner)
        .sign_with_keys(&bob_owner_keys)?;
    manager.process_received_event(bob_app_keys_event);
    drain_events(&rx);

    let accepted = manager.accept_invite(&invite, Some(bob_owner))?;
    assert_eq!(accepted.owner_pubkey, bob_owner);
    assert_eq!(accepted.device_id, bob_device_id);

    let mut saw_response = false;
    let start = std::time::Instant::now();
    while start.elapsed() < EVENT_TIMEOUT {
        if let Ok(event) = rx.recv_timeout(EVENT_POLL_INTERVAL) {
            let signed = match event {
                SessionManagerEvent::PublishSigned(signed)
                | SessionManagerEvent::PublishSignedForInnerEvent { event: signed, .. } => signed,
                _ => continue,
            };
            if signed.kind.as_u16() == nostr_double_ratchet::INVITE_RESPONSE_KIND as u16 {
                saw_response = true;
                break;
            }
        }
    }
    assert!(
        saw_response,
        "expected invite response publish from accept_invite"
    );

    let exported = manager.export_active_session_state(bob_owner)?;
    assert!(
        exported.is_some(),
        "expected active session under owner/device"
    );

    Ok(())
}

#[test]
fn test_replayed_invite_is_ignored_once_accept_bootstrap_uses_session() -> Result<()> {
    let alice_keys = Keys::generate();
    let alice_pubkey = alice_keys.public_key();
    let alice_device_id = alice_pubkey.to_hex();

    let bob_keys = Keys::generate();
    let bob_pubkey = bob_keys.public_key();
    let bob_device_id = bob_pubkey.to_hex();

    let invite = Invite::create_new(alice_pubkey, Some(alice_device_id.clone()), None)?;

    let (alice_tx, alice_rx) = crossbeam_channel::unbounded();
    let (bob_tx, bob_rx) = crossbeam_channel::unbounded();

    let alice_mgr = SessionManager::new(
        alice_pubkey,
        alice_keys.secret_key().to_secret_bytes(),
        alice_device_id.clone(),
        alice_pubkey,
        alice_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        None,
    );
    let bob_mgr = SessionManager::new(
        bob_pubkey,
        bob_keys.secret_key().to_secret_bytes(),
        bob_device_id.clone(),
        bob_pubkey,
        bob_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        None,
    );

    alice_mgr.init()?;
    bob_mgr.init()?;
    drain_events(&alice_rx);
    drain_events(&bob_rx);

    let first_accept = bob_mgr.accept_invite(&invite, Some(alice_pubkey))?;
    assert!(first_accept.created_new_session);
    let (first_response, _) = recv_invite_response_and_message_event(&bob_rx);
    let (peer_pubkey, remote_device_id) = import_session_from_response(
        &invite,
        alice_keys.secret_key().to_secret_bytes(),
        &alice_mgr,
        &first_response,
    )?;
    assert_eq!(peer_pubkey, bob_pubkey);
    assert_eq!(remote_device_id, bob_device_id);
    drain_events(&alice_rx);
    drain_events(&bob_rx);

    let refreshed_accept = bob_mgr.accept_invite(&invite, Some(alice_pubkey))?;
    assert!(
        !refreshed_accept.created_new_session,
        "accept_invite bootstrap should consume the send-only session so replayed invites are ignored",
    );
    drain_events(&bob_rx);

    bob_mgr.send_text(alice_pubkey, "fresh response can decrypt".to_string(), None)?;
    let sent = recv_message_events(&bob_rx, 1);
    alice_mgr.process_received_event(sent[0].clone());

    let decrypted =
        recv_decrypted_containing(&alice_rx, "\"content\":\"fresh response can decrypt\"");
    assert!(decrypted.contains("\"content\":\"fresh response can decrypt\""));

    Ok(())
}

#[test]
fn test_mutual_same_device_chat_invites_still_allow_bidirectional_messages() -> Result<()> {
    let alice_keys = Keys::generate();
    let alice_pubkey = alice_keys.public_key();
    let alice_device_id = alice_pubkey.to_hex();

    let bob_keys = Keys::generate();
    let bob_pubkey = bob_keys.public_key();
    let bob_device_id = bob_pubkey.to_hex();

    let alice_invite = Invite::create_new(alice_pubkey, Some(alice_device_id.clone()), None)?;
    let bob_invite = Invite::create_new(bob_pubkey, Some(bob_device_id.clone()), None)?;

    let (alice_tx, alice_rx) = crossbeam_channel::unbounded();
    let (bob_tx, bob_rx) = crossbeam_channel::unbounded();

    let alice_mgr = SessionManager::new(
        alice_pubkey,
        alice_keys.secret_key().to_secret_bytes(),
        alice_device_id.clone(),
        alice_pubkey,
        alice_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        None,
    );
    let bob_mgr = SessionManager::new(
        bob_pubkey,
        bob_keys.secret_key().to_secret_bytes(),
        bob_device_id.clone(),
        bob_pubkey,
        bob_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        None,
    );

    alice_mgr.init()?;
    bob_mgr.init()?;
    drain_events(&alice_rx);
    drain_events(&bob_rx);

    let bob_accept = bob_mgr.accept_invite(&alice_invite, Some(alice_pubkey))?;
    assert!(bob_accept.created_new_session);
    let (bob_response, _) = recv_invite_response_and_message_event(&bob_rx);
    let (bob_owner, bob_remote_device_id) = import_session_from_response(
        &alice_invite,
        alice_keys.secret_key().to_secret_bytes(),
        &alice_mgr,
        &bob_response,
    )?;
    assert_eq!(bob_owner, bob_pubkey);
    assert_eq!(bob_remote_device_id, bob_device_id);
    drain_events(&alice_rx);
    drain_events(&bob_rx);

    let alice_accept = alice_mgr.accept_invite(&bob_invite, Some(bob_pubkey))?;
    assert!(
        !alice_accept.created_new_session,
        "reverse invite from the same device should not fork the conversation path",
    );
    drain_events(&alice_rx);
    drain_events(&bob_rx);

    bob_mgr.send_text(alice_pubkey, "bob can still reach alice".to_string(), None)?;
    let bob_sent = recv_message_events(&bob_rx, 1);
    alice_mgr.process_received_event(bob_sent[0].clone());
    let alice_decrypted =
        recv_decrypted_containing(&alice_rx, "\"content\":\"bob can still reach alice\"");
    assert!(alice_decrypted.contains("\"content\":\"bob can still reach alice\""));
    drain_events(&alice_rx);
    drain_events(&bob_rx);

    alice_mgr.send_text(bob_pubkey, "alice can still reach bob".to_string(), None)?;
    let alice_sent = recv_message_events(&alice_rx, 1);
    bob_mgr.process_received_event(alice_sent[0].clone());
    let bob_decrypted =
        recv_decrypted_containing(&bob_rx, "\"content\":\"alice can still reach bob\"");
    assert!(bob_decrypted.contains("\"content\":\"alice can still reach bob\""));

    Ok(())
}

#[test]
fn test_simultaneous_mutual_same_device_invites_converge_via_response_processing() -> Result<()> {
    let alice_keys = Keys::generate();
    let alice_pubkey = alice_keys.public_key();
    let alice_device_id = alice_pubkey.to_hex();

    let bob_keys = Keys::generate();
    let bob_pubkey = bob_keys.public_key();
    let bob_device_id = bob_pubkey.to_hex();

    let alice_invite = Invite::create_new(alice_pubkey, Some(alice_device_id.clone()), None)?;
    let bob_invite = Invite::create_new(bob_pubkey, Some(bob_device_id.clone()), None)?;

    let (alice_tx, alice_rx) = crossbeam_channel::unbounded();
    let (bob_tx, bob_rx) = crossbeam_channel::unbounded();

    let alice_mgr = SessionManager::new(
        alice_pubkey,
        alice_keys.secret_key().to_secret_bytes(),
        alice_device_id.clone(),
        alice_pubkey,
        alice_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        None,
    );
    let bob_mgr = SessionManager::new(
        bob_pubkey,
        bob_keys.secret_key().to_secret_bytes(),
        bob_device_id.clone(),
        bob_pubkey,
        bob_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        None,
    );

    alice_mgr.init()?;
    bob_mgr.init()?;
    drain_events(&alice_rx);
    drain_events(&bob_rx);

    let bob_accept = bob_mgr.accept_invite(&alice_invite, Some(alice_pubkey))?;
    assert!(bob_accept.created_new_session);
    let (bob_response, bob_bootstrap) = recv_invite_response_and_message_event(&bob_rx);

    let alice_accept = alice_mgr.accept_invite(&bob_invite, Some(bob_pubkey))?;
    assert!(alice_accept.created_new_session);
    let (alice_response, alice_bootstrap) = recv_invite_response_and_message_event(&alice_rx);

    let (alice_peer, alice_remote_device_id) = import_session_from_response(
        &alice_invite,
        alice_keys.secret_key().to_secret_bytes(),
        &alice_mgr,
        &bob_response,
    )?;
    assert_eq!(alice_peer, bob_pubkey);
    assert_eq!(alice_remote_device_id, bob_device_id);

    let (bob_peer, bob_remote_device_id) = import_session_from_response(
        &bob_invite,
        bob_keys.secret_key().to_secret_bytes(),
        &bob_mgr,
        &alice_response,
    )?;
    assert_eq!(bob_peer, alice_pubkey);
    assert_eq!(bob_remote_device_id, alice_device_id);

    alice_mgr.process_received_event(bob_bootstrap);
    bob_mgr.process_received_event(alice_bootstrap);
    drain_events(&alice_rx);
    drain_events(&bob_rx);

    bob_mgr.send_text(
        alice_pubkey,
        "bob reaches alice after mutual invites".to_string(),
        None,
    )?;
    let bob_sent = recv_message_events(&bob_rx, 1);
    alice_mgr.process_received_event(bob_sent[0].clone());
    let alice_decrypted = recv_decrypted_containing(
        &alice_rx,
        "\"content\":\"bob reaches alice after mutual invites\"",
    );
    assert!(alice_decrypted.contains("\"content\":\"bob reaches alice after mutual invites\""));
    drain_events(&alice_rx);
    drain_events(&bob_rx);

    alice_mgr.send_text(
        bob_pubkey,
        "alice reaches bob after mutual invites".to_string(),
        None,
    )?;
    let alice_sent = recv_message_events(&alice_rx, 1);
    bob_mgr.process_received_event(alice_sent[0].clone());
    let bob_decrypted = recv_decrypted_containing(
        &bob_rx,
        "\"content\":\"alice reaches bob after mutual invites\"",
    );
    assert!(bob_decrypted.contains("\"content\":\"alice reaches bob after mutual invites\""));

    Ok(())
}

#[test]
fn test_replayed_invite_ignored_after_send_only_session_is_used() -> Result<()> {
    let alice_keys = Keys::generate();
    let alice_pubkey = alice_keys.public_key();
    let alice_device_id = alice_pubkey.to_hex();

    let bob_keys = Keys::generate();
    let bob_pubkey = bob_keys.public_key();
    let bob_device_id = bob_pubkey.to_hex();

    let invite = Invite::create_new(alice_pubkey, Some(alice_device_id.clone()), None)?;

    let (alice_tx, alice_rx) = crossbeam_channel::unbounded();
    let (bob_tx, bob_rx) = crossbeam_channel::unbounded();

    let alice_mgr = SessionManager::new(
        alice_pubkey,
        alice_keys.secret_key().to_secret_bytes(),
        alice_device_id.clone(),
        alice_pubkey,
        alice_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        None,
    );
    let bob_mgr = SessionManager::new(
        bob_pubkey,
        bob_keys.secret_key().to_secret_bytes(),
        bob_device_id.clone(),
        bob_pubkey,
        bob_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        None,
    );

    alice_mgr.init()?;
    bob_mgr.init()?;
    drain_events(&alice_rx);
    drain_events(&bob_rx);

    let first_accept = bob_mgr.accept_invite(&invite, Some(alice_pubkey))?;
    assert!(first_accept.created_new_session);
    let first_response =
        recv_signed_event_of_kind(&bob_rx, nostr_double_ratchet::INVITE_RESPONSE_KIND);
    let (peer_pubkey, remote_device_id) = import_session_from_response(
        &invite,
        alice_keys.secret_key().to_secret_bytes(),
        &alice_mgr,
        &first_response,
    )?;
    assert_eq!(peer_pubkey, bob_pubkey);
    assert_eq!(remote_device_id, bob_device_id);
    drain_events(&alice_rx);
    drain_events(&bob_rx);

    bob_mgr.send_text(
        alice_pubkey,
        "first send establishes path".to_string(),
        None,
    )?;
    let first_message = recv_message_events(&bob_rx, 1);
    alice_mgr.process_received_event(first_message[0].clone());
    let decrypted =
        recv_decrypted_containing(&alice_rx, "\"content\":\"first send establishes path\"");
    assert!(decrypted.contains("\"content\":\"first send establishes path\""));
    drain_events(&alice_rx);
    drain_events(&bob_rx);

    let replayed_accept = bob_mgr.accept_invite(&invite, Some(alice_pubkey))?;
    assert!(
        !replayed_accept.created_new_session,
        "used send-only session should ignore replayed invite refreshes",
    );

    bob_mgr.send_text(
        alice_pubkey,
        "replayed invite did not churn path".to_string(),
        None,
    )?;
    let second_message = recv_message_events(&bob_rx, 1);
    alice_mgr.process_received_event(second_message[0].clone());
    let decrypted = recv_decrypted_containing(
        &alice_rx,
        "\"content\":\"replayed invite did not churn path\"",
    );
    assert!(decrypted.contains("\"content\":\"replayed invite did not churn path\""));

    Ok(())
}

#[test]
fn test_processing_peer_public_invite_upgrades_response_import_to_send_capable() -> Result<()> {
    let alice_keys = Keys::generate();
    let alice_pubkey = alice_keys.public_key();
    let alice_device_id = alice_pubkey.to_hex();

    let bob_keys = Keys::generate();
    let bob_pubkey = bob_keys.public_key();
    let bob_device_id = bob_pubkey.to_hex();

    let alice_public_invite =
        Invite::create_new(alice_pubkey, Some(alice_device_id.clone()), None)?;
    let bob_public_invite = Invite::create_new(bob_pubkey, Some(bob_device_id.clone()), None)?;

    let (alice_tx, alice_rx) = crossbeam_channel::unbounded();
    let (bob_tx, bob_rx) = crossbeam_channel::unbounded();

    let alice_mgr = SessionManager::new(
        alice_pubkey,
        alice_keys.secret_key().to_secret_bytes(),
        alice_device_id.clone(),
        alice_pubkey,
        alice_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        None,
    );
    let bob_mgr = SessionManager::new(
        bob_pubkey,
        bob_keys.secret_key().to_secret_bytes(),
        bob_device_id.clone(),
        bob_pubkey,
        bob_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        None,
    );

    alice_mgr.init()?;
    bob_mgr.init()?;
    drain_events(&alice_rx);
    drain_events(&bob_rx);

    let accepted = bob_mgr.accept_invite(&alice_public_invite, Some(alice_pubkey))?;
    assert!(accepted.created_new_session);
    let alice_public_response =
        recv_signed_event_of_kind(&bob_rx, nostr_double_ratchet::INVITE_RESPONSE_KIND);
    let (peer_pubkey, remote_device_id) = import_session_from_response(
        &alice_public_invite,
        alice_keys.secret_key().to_secret_bytes(),
        &alice_mgr,
        &alice_public_response,
    )?;
    assert_eq!(peer_pubkey, bob_pubkey);
    assert_eq!(remote_device_id, bob_device_id);

    let imported_sendable = alice_mgr
        .export_active_sessions()
        .into_iter()
        .find(|(owner, device_id, _)| *owner == bob_pubkey && device_id == &bob_device_id)
        .map(|(_, _, state)| Session::new(state, "debug".to_string()).can_send())
        .unwrap_or(false);
    assert!(
        !imported_sendable,
        "response import alone should not already be send-capable",
    );

    alice_mgr.process_received_event(bob_public_invite.get_event()?.sign_with_keys(&bob_keys)?);
    let bob_public_response =
        recv_signed_event_of_kind(&alice_rx, nostr_double_ratchet::INVITE_RESPONSE_KIND);
    let (alice_peer_pubkey, alice_remote_device_id) = import_session_from_response(
        &bob_public_invite,
        bob_keys.secret_key().to_secret_bytes(),
        &bob_mgr,
        &bob_public_response,
    )?;
    assert_eq!(alice_peer_pubkey, alice_pubkey);
    assert_eq!(alice_remote_device_id, alice_device_id);
    drain_events(&alice_rx);

    let upgraded_sendable = alice_mgr
        .export_active_sessions()
        .into_iter()
        .find(|(owner, device_id, _)| *owner == bob_pubkey && device_id == &bob_device_id)
        .map(|(_, _, state)| Session::new(state, "debug".to_string()).can_send())
        .unwrap_or(false);
    assert!(
        upgraded_sendable,
        "processing the peer public invite should upgrade the device to a send-capable session",
    );

    alice_mgr.send_text(
        bob_pubkey,
        "public invite refresh makes reply sendable".to_string(),
        None,
    )?;
    let sent = recv_message_events(&alice_rx, 1);
    bob_mgr.process_received_event(sent[0].clone());

    let decrypted = recv_decrypted_containing(
        &bob_rx,
        "\"content\":\"public invite refresh makes reply sendable\"",
    );
    assert!(decrypted.contains("\"content\":\"public invite refresh makes reply sendable\""));

    Ok(())
}

#[test]
fn test_processing_same_owner_sibling_public_invite_upgrades_response_import() -> Result<()> {
    let owner_keys = Keys::generate();
    let owner_pubkey = owner_keys.public_key();

    let current_keys = Keys::generate();
    let current_pubkey = current_keys.public_key();
    let current_device_id = current_pubkey.to_hex();

    let sibling_keys = Keys::generate();
    let sibling_pubkey = sibling_keys.public_key();
    let sibling_device_id = sibling_pubkey.to_hex();

    let current_public_invite =
        new_public_invite(current_pubkey, current_device_id.clone(), owner_pubkey)?;
    let sibling_public_invite =
        new_public_invite(sibling_pubkey, sibling_device_id.clone(), owner_pubkey)?;

    let (current_tx, current_rx) = crossbeam_channel::unbounded();
    let (sibling_tx, sibling_rx) = crossbeam_channel::unbounded();

    let current_mgr = SessionManager::new(
        current_pubkey,
        current_keys.secret_key().to_secret_bytes(),
        current_device_id.clone(),
        owner_pubkey,
        current_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        Some(current_public_invite.clone()),
    );
    let sibling_mgr = SessionManager::new(
        sibling_pubkey,
        sibling_keys.secret_key().to_secret_bytes(),
        sibling_device_id.clone(),
        owner_pubkey,
        sibling_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        Some(sibling_public_invite.clone()),
    );

    current_mgr.init()?;
    sibling_mgr.init()?;
    drain_events(&current_rx);
    drain_events(&sibling_rx);

    let app_keys = AppKeys::new(vec![
        DeviceEntry::new(owner_pubkey, 1),
        DeviceEntry::new(current_pubkey, 2),
        DeviceEntry::new(sibling_pubkey, 3),
    ]);
    let app_keys_event = app_keys
        .get_event(owner_pubkey)
        .sign_with_keys(&owner_keys)?;
    current_mgr.process_received_event(app_keys_event.clone());
    sibling_mgr.process_received_event(app_keys_event);
    drain_events(&current_rx);
    drain_events(&sibling_rx);

    let accepted = sibling_mgr.accept_invite(&current_public_invite, Some(owner_pubkey))?;
    assert!(accepted.created_new_session);
    let current_response =
        recv_signed_event_of_kind(&sibling_rx, nostr_double_ratchet::INVITE_RESPONSE_KIND);
    current_mgr.process_received_event(current_response);
    drain_events(&current_rx);
    drain_events(&sibling_rx);

    let imported_sendable = current_mgr
        .export_active_sessions()
        .into_iter()
        .find(|(owner, device_id, _)| *owner == owner_pubkey && device_id == &sibling_device_id)
        .map(|(_, _, state)| Session::new(state, "debug".to_string()).can_send())
        .unwrap_or(false);
    assert!(
        !imported_sendable,
        "response import alone should not already be send-capable for same-owner siblings",
    );

    let accepted = current_mgr.accept_invite(&sibling_public_invite, Some(owner_pubkey))?;
    assert!(
        accepted.created_new_session,
        "same-owner sibling public invite should refresh an imported response-only session",
    );
    let _ = recv_signed_event_of_kind(&current_rx, nostr_double_ratchet::INVITE_RESPONSE_KIND);
    drain_events(&current_rx);

    let upgraded_sendable = current_mgr
        .export_active_sessions()
        .into_iter()
        .find(|(owner, device_id, _)| *owner == owner_pubkey && device_id == &sibling_device_id)
        .map(|(_, _, state)| Session::new(state, "debug".to_string()).can_send())
        .unwrap_or(false);
    assert!(
        upgraded_sendable,
        "processing the sibling public invite should upgrade the same-owner device to a send-capable session",
    );

    Ok(())
}

#[test]
fn test_multi_device_self_fanout() -> Result<()> {
    let owner_keys = Keys::generate();
    let owner_pubkey = owner_keys.public_key();

    let device1_keys = Keys::generate();
    let device2_keys = Keys::generate();

    let device1_id = hex::encode(device1_keys.public_key().to_bytes());
    let device2_id = hex::encode(device2_keys.public_key().to_bytes());

    let invite1 = Invite::create_new(device1_keys.public_key(), Some(device1_id.clone()), None)?;
    let invite2 = Invite::create_new(device2_keys.public_key(), Some(device2_id.clone()), None)?;

    let (tx1, rx1) = crossbeam_channel::unbounded();
    let (tx2, rx2) = crossbeam_channel::unbounded();

    let manager1 = SessionManager::new(
        device1_keys.public_key(),
        device1_keys.secret_key().to_secret_bytes(),
        device1_id.clone(),
        owner_pubkey,
        tx1,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        Some(invite1.clone()),
    );

    let manager2 = SessionManager::new(
        device2_keys.public_key(),
        device2_keys.secret_key().to_secret_bytes(),
        device2_id.clone(),
        owner_pubkey,
        tx2,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        Some(invite2.clone()),
    );

    manager1.init()?;
    manager2.init()?;
    drain_events(&rx1);
    drain_events(&rx2);

    // Publish AppKeys listing both devices
    let app_keys = AppKeys::new(vec![
        DeviceEntry::new(device1_keys.public_key(), 1),
        DeviceEntry::new(device2_keys.public_key(), 2),
    ]);

    let app_keys_event = app_keys
        .get_event(owner_pubkey)
        .sign_with_keys(&owner_keys)?;

    manager1.process_received_event(app_keys_event.clone());
    manager2.process_received_event(app_keys_event);

    // Device2 accepts device1 invite
    let invite_event = invite1.get_event()?.sign_with_keys(&device1_keys)?;
    manager2.process_received_event(invite_event);

    // Deliver invite response back to device1
    let response_event = loop {
        let signed = recv_signed_event(&rx2);
        if signed.kind.as_u16() == nostr_double_ratchet::INVITE_RESPONSE_KIND as u16 {
            break signed;
        }
    };
    manager1.process_received_event(response_event);

    // Device2 sends first to establish ratchet for device1
    manager2.send_text(owner_pubkey, "ping".to_string(), None)?;
    let ping_event = loop {
        let signed = recv_signed_event(&rx2);
        if signed.kind.as_u16() == MESSAGE_EVENT_KIND as u16 {
            break signed;
        }
    };
    manager1.process_received_event(ping_event);

    // Send a message to self (owner) from device1; should fan out to device2
    manager1.send_text(owner_pubkey, "hello".to_string(), None)?;

    // Deliver encrypted message to device2
    let message_event = loop {
        let signed = recv_signed_event(&rx1);
        if signed.kind.as_u16() == MESSAGE_EVENT_KIND as u16 {
            break signed;
        }
    };
    manager2.process_received_event(message_event);

    // Expect decrypted message on device2
    let mut decrypted = None;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(1) {
        if let Ok(SessionManagerEvent::DecryptedMessage { content, .. }) =
            rx2.recv_timeout(Duration::from_millis(50))
        {
            decrypted = Some(content);
            break;
        }
    }

    let content = decrypted.expect("Expected decrypted message");
    assert!(content.contains("\"content\":\"hello\""));

    Ok(())
}

#[test]
fn test_send_text_with_expiration_tag_propagates_to_receiver() -> Result<()> {
    let owner_keys = Keys::generate();
    let owner_pubkey = owner_keys.public_key();

    let device1_keys = Keys::generate();
    let device2_keys = Keys::generate();

    let device1_id = hex::encode(device1_keys.public_key().to_bytes());
    let device2_id = hex::encode(device2_keys.public_key().to_bytes());

    let invite1 = Invite::create_new(device1_keys.public_key(), Some(device1_id.clone()), None)?;
    let invite2 = Invite::create_new(device2_keys.public_key(), Some(device2_id.clone()), None)?;

    let (tx1, rx1) = crossbeam_channel::unbounded();
    let (tx2, rx2) = crossbeam_channel::unbounded();

    let manager1 = SessionManager::new(
        device1_keys.public_key(),
        device1_keys.secret_key().to_secret_bytes(),
        device1_id.clone(),
        owner_pubkey,
        tx1,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        Some(invite1.clone()),
    );

    let manager2 = SessionManager::new(
        device2_keys.public_key(),
        device2_keys.secret_key().to_secret_bytes(),
        device2_id.clone(),
        owner_pubkey,
        tx2,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        Some(invite2.clone()),
    );

    manager1.init()?;
    manager2.init()?;
    drain_events(&rx1);
    drain_events(&rx2);

    // Publish AppKeys listing both devices
    let app_keys = AppKeys::new(vec![
        DeviceEntry::new(device1_keys.public_key(), 1),
        DeviceEntry::new(device2_keys.public_key(), 2),
    ]);

    let app_keys_event = app_keys
        .get_event(owner_pubkey)
        .sign_with_keys(&owner_keys)?;

    manager1.process_received_event(app_keys_event.clone());
    manager2.process_received_event(app_keys_event);

    // Device2 accepts device1 invite
    let invite_event = invite1.get_event()?.sign_with_keys(&device1_keys)?;
    manager2.process_received_event(invite_event);

    // Deliver invite response back to device1
    let response_event = loop {
        let signed = recv_signed_event(&rx2);
        if signed.kind.as_u16() == nostr_double_ratchet::INVITE_RESPONSE_KIND as u16 {
            break signed;
        }
    };
    manager1.process_received_event(response_event);

    // Device2 sends first to establish ratchet for device1
    manager2.send_text(owner_pubkey, "ping".to_string(), None)?;
    let ping_event = loop {
        let signed = recv_signed_event(&rx2);
        if signed.kind.as_u16() == MESSAGE_EVENT_KIND as u16 {
            break signed;
        }
    };
    manager1.process_received_event(ping_event);

    // Send a message to self (owner) from device1 with an expiration tag.
    let expires_at = 1_700_000_000u64;
    manager1.set_peer_send_options(
        owner_pubkey,
        Some(SendOptions {
            expires_at: Some(expires_at),
            ttl_seconds: None,
        }),
    )?;
    manager1.send_text(owner_pubkey, "hello".to_string(), None)?;

    // Deliver encrypted message to device2
    let message_event = loop {
        let signed = recv_signed_event(&rx1);
        if signed.kind.as_u16() == MESSAGE_EVENT_KIND as u16 {
            break signed;
        }
    };
    manager2.process_received_event(message_event);

    // Expect decrypted message on device2
    let mut decrypted = None;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(1) {
        if let Ok(SessionManagerEvent::DecryptedMessage { content, .. }) =
            rx2.recv_timeout(Duration::from_millis(50))
        {
            decrypted = Some(content);
            break;
        }
    }

    let plaintext = decrypted.expect("Expected decrypted message");
    let rumor = UnsignedEvent::from_json(&plaintext).expect("valid rumor JSON");

    let exp = rumor.tags.iter().find_map(|t| {
        let v = t.clone().to_vec();
        if v.first().map(|s| s.as_str()) == Some(nostr_double_ratchet::EXPIRATION_TAG) {
            v.get(1).cloned()
        } else {
            None
        }
    });

    let expected = expires_at.to_string();
    assert_eq!(exp.as_deref(), Some(expected.as_str()));

    Ok(())
}

#[test]
fn test_delayed_link_bootstrap_flushes_queued_self_sync_message() -> Result<()> {
    let owner_keys = Keys::generate();
    let owner_pubkey = owner_keys.public_key();
    let owner_device_id = owner_pubkey.to_hex();

    let linked_keys = Keys::generate();
    let linked_pubkey = linked_keys.public_key();
    let linked_device_id = linked_pubkey.to_hex();

    let link_invite = new_link_invite(linked_pubkey, linked_device_id.clone(), owner_pubkey)?;

    let (owner_tx, owner_rx) = crossbeam_channel::unbounded();
    let (linked_tx, linked_rx) = crossbeam_channel::unbounded();

    let owner_mgr = SessionManager::new(
        owner_pubkey,
        owner_keys.secret_key().to_secret_bytes(),
        owner_device_id.clone(),
        owner_pubkey,
        owner_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        None,
    );
    let linked_mgr = SessionManager::new(
        linked_pubkey,
        linked_keys.secret_key().to_secret_bytes(),
        linked_device_id.clone(),
        owner_pubkey,
        linked_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        None,
    );

    owner_mgr.init()?;
    linked_mgr.init()?;
    drain_events(&owner_rx);
    drain_events(&linked_rx);

    let accepted = owner_mgr.accept_invite(&link_invite, Some(owner_pubkey))?;
    assert_eq!(accepted.owner_pubkey, owner_pubkey);
    let (link_response, link_bootstrap) = recv_invite_response_and_message_event(&owner_rx);
    let (linked_peer, linked_remote_device_id) = import_session_from_response(
        &link_invite,
        linked_keys.secret_key().to_secret_bytes(),
        &linked_mgr,
        &link_response,
    )?;
    assert_eq!(linked_peer, owner_pubkey);
    assert_eq!(linked_remote_device_id, owner_device_id);
    drain_events(&owner_rx);
    drain_events(&linked_rx);

    let queued = linked_mgr.send_text(owner_pubkey, "queued before bootstrap".to_string(), None)?;
    assert!(
        queued.is_empty(),
        "message should stay queued until the first owner bootstrap arrives"
    );

    linked_mgr.process_received_event(link_bootstrap);

    let flushed = recv_message_events(&linked_rx, 1);
    assert_eq!(flushed.len(), 1);
    owner_mgr.process_received_event(flushed[0].clone());

    let decrypted = recv_decrypted_containing(&owner_rx, "\"content\":\"queued before bootstrap\"");
    assert!(decrypted.contains("\"content\":\"queued before bootstrap\""));

    Ok(())
}

#[test]
fn test_existing_peer_fans_out_to_newly_added_device_after_appkeys_and_invite() -> Result<()> {
    let alice_owner_keys = Keys::generate();
    let alice_owner_pubkey = alice_owner_keys.public_key();
    let alice_owner_device_id = alice_owner_pubkey.to_hex();

    let alice_new_device_keys = Keys::generate();
    let alice_new_device_pubkey = alice_new_device_keys.public_key();
    let alice_new_device_id = alice_new_device_pubkey.to_hex();

    let bob_keys = Keys::generate();
    let bob_pubkey = bob_keys.public_key();
    let bob_device_id = bob_pubkey.to_hex();

    let owner_invite = Invite::create_new(
        alice_owner_pubkey,
        Some(alice_owner_device_id.clone()),
        None,
    )?;
    let new_device_invite = Invite::create_new(
        alice_new_device_pubkey,
        Some(alice_new_device_id.clone()),
        None,
    )?;

    let (alice_owner_tx, alice_owner_rx) = crossbeam_channel::unbounded();
    let (alice_new_tx, alice_new_rx) = crossbeam_channel::unbounded();
    let (bob_tx, bob_rx) = crossbeam_channel::unbounded();

    let alice_owner_mgr = SessionManager::new(
        alice_owner_pubkey,
        alice_owner_keys.secret_key().to_secret_bytes(),
        alice_owner_device_id.clone(),
        alice_owner_pubkey,
        alice_owner_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        Some(owner_invite.clone()),
    );
    let alice_new_mgr = SessionManager::new(
        alice_new_device_pubkey,
        alice_new_device_keys.secret_key().to_secret_bytes(),
        alice_new_device_id.clone(),
        alice_owner_pubkey,
        alice_new_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        Some(new_device_invite.clone()),
    );
    let bob_mgr = SessionManager::new(
        bob_pubkey,
        bob_keys.secret_key().to_secret_bytes(),
        bob_device_id,
        bob_pubkey,
        bob_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        None,
    );

    alice_owner_mgr.init()?;
    alice_new_mgr.init()?;
    bob_mgr.init()?;
    drain_events(&alice_owner_rx);
    drain_events(&alice_new_rx);
    drain_events(&bob_rx);

    // Bob already has an existing session with Alice's original device.
    let accepted = bob_mgr.accept_invite(&owner_invite, Some(alice_owner_pubkey))?;
    assert_eq!(accepted.owner_pubkey, alice_owner_pubkey);
    let owner_response =
        recv_signed_event_of_kind(&bob_rx, nostr_double_ratchet::INVITE_RESPONSE_KIND);
    alice_owner_mgr.process_received_event(owner_response);
    drain_events(&bob_rx);

    bob_mgr.send_text(alice_owner_pubkey, "seed existing chat".to_string(), None)?;
    let seed_message = recv_message_events(&bob_rx, 1)
        .into_iter()
        .next()
        .expect("expected seed message");
    alice_owner_mgr.process_received_event(seed_message);

    let initial_decrypted =
        recv_decrypted_containing(&alice_owner_rx, "\"content\":\"seed existing chat\"");
    assert!(initial_decrypted.contains("\"content\":\"seed existing chat\""));
    drain_events(&alice_owner_rx);
    drain_events(&bob_rx);

    // Alice publishes updated AppKeys adding the new device.
    let app_keys = AppKeys::new(vec![
        DeviceEntry::new(alice_owner_pubkey, 1),
        DeviceEntry::new(alice_new_device_pubkey, 2),
    ]);
    let app_keys_event = app_keys
        .get_event(alice_owner_pubkey)
        .sign_with_keys(&alice_owner_keys)?;
    bob_mgr.process_received_event(app_keys_event);
    drain_events(&bob_rx);

    // Bob learns the new device invite and auto-accepts it.
    let new_device_invite_event = new_device_invite
        .get_event()?
        .sign_with_keys(&alice_new_device_keys)?;
    bob_mgr.process_received_event(new_device_invite_event);
    let new_device_response =
        recv_signed_event_of_kind(&bob_rx, nostr_double_ratchet::INVITE_RESPONSE_KIND);
    alice_new_mgr.process_received_event(new_device_response);
    drain_events(&alice_new_rx);
    drain_events(&bob_rx);

    // The next send to Alice should fan out to both devices.
    bob_mgr.send_text(
        alice_owner_pubkey,
        "fanout to old and new device".to_string(),
        None,
    )?;
    let fanout_messages = recv_message_events(&bob_rx, 2);
    assert_eq!(fanout_messages.len(), 2);

    for event in &fanout_messages {
        alice_owner_mgr.process_received_event(event.clone());
        alice_new_mgr.process_received_event(event.clone());
    }

    let owner_decrypted = recv_decrypted_containing(
        &alice_owner_rx,
        "\"content\":\"fanout to old and new device\"",
    );
    let new_device_decrypted = recv_decrypted_containing(
        &alice_new_rx,
        "\"content\":\"fanout to old and new device\"",
    );

    assert!(owner_decrypted.contains("\"content\":\"fanout to old and new device\""));
    assert!(new_device_decrypted.contains("\"content\":\"fanout to old and new device\""));

    Ok(())
}

#[test]
fn test_linked_receiver_restores_and_receives_after_restart() -> Result<()> {
    let alice_owner_keys = Keys::generate();
    let alice_owner_pubkey = alice_owner_keys.public_key();
    let alice_owner_device_id = alice_owner_pubkey.to_hex();

    let alice_new_device_keys = Keys::generate();
    let alice_new_device_pubkey = alice_new_device_keys.public_key();
    let alice_new_device_id = alice_new_device_pubkey.to_hex();

    let bob_owner_keys = Keys::generate();
    let bob_owner_pubkey = bob_owner_keys.public_key();
    let bob_owner_device_id = bob_owner_pubkey.to_hex();

    let bob_linked_keys = Keys::generate();
    let bob_linked_pubkey = bob_linked_keys.public_key();
    let bob_linked_device_id = bob_linked_pubkey.to_hex();

    let alice_linked_link_invite = new_link_invite(
        alice_new_device_pubkey,
        alice_new_device_id.clone(),
        alice_owner_pubkey,
    )?;
    let bob_linked_link_invite = new_link_invite(
        bob_linked_pubkey,
        bob_linked_device_id.clone(),
        bob_owner_pubkey,
    )?;

    let bob_owner_public_invite =
        Invite::create_new(bob_owner_pubkey, Some(bob_owner_device_id.clone()), None)?;
    let bob_linked_public_invite = new_public_invite(
        bob_linked_pubkey,
        bob_linked_device_id.clone(),
        bob_owner_pubkey,
    )?;
    let alice_linked_public_invite = new_public_invite(
        alice_new_device_pubkey,
        alice_new_device_id.clone(),
        alice_owner_pubkey,
    )?;

    let alice_owner_storage =
        Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>;
    let alice_linked_storage =
        Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>;
    let bob_owner_storage =
        Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>;
    let bob_linked_storage =
        Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>;

    let (alice_owner_tx, alice_owner_rx) = crossbeam_channel::unbounded();
    let (alice_linked_tx, alice_linked_rx) = crossbeam_channel::unbounded();
    let (bob_owner_tx, bob_owner_rx) = crossbeam_channel::unbounded();
    let (bob_linked_tx, bob_linked_rx) = crossbeam_channel::unbounded();

    let alice_owner_mgr = SessionManager::new(
        alice_owner_pubkey,
        alice_owner_keys.secret_key().to_secret_bytes(),
        alice_owner_device_id.clone(),
        alice_owner_pubkey,
        alice_owner_tx,
        Some(alice_owner_storage.clone()),
        None,
    );
    let alice_linked_mgr = SessionManager::new(
        alice_new_device_pubkey,
        alice_new_device_keys.secret_key().to_secret_bytes(),
        alice_new_device_id.clone(),
        alice_owner_pubkey,
        alice_linked_tx,
        Some(alice_linked_storage.clone()),
        None,
    );
    let bob_owner_mgr = SessionManager::new(
        bob_owner_pubkey,
        bob_owner_keys.secret_key().to_secret_bytes(),
        bob_owner_device_id.clone(),
        bob_owner_pubkey,
        bob_owner_tx,
        Some(bob_owner_storage.clone()),
        None,
    );
    let bob_linked_mgr = SessionManager::new(
        bob_linked_pubkey,
        bob_linked_keys.secret_key().to_secret_bytes(),
        bob_linked_device_id.clone(),
        bob_owner_pubkey,
        bob_linked_tx,
        Some(bob_linked_storage.clone()),
        None,
    );

    alice_owner_mgr.init()?;
    alice_linked_mgr.init()?;
    bob_owner_mgr.init()?;
    bob_linked_mgr.init()?;
    drain_events(&alice_owner_rx);
    drain_events(&alice_linked_rx);
    drain_events(&bob_owner_rx);
    drain_events(&bob_linked_rx);

    let accepted =
        alice_owner_mgr.accept_invite(&bob_owner_public_invite, Some(bob_owner_pubkey))?;
    assert_eq!(accepted.owner_pubkey, bob_owner_pubkey);
    let bob_owner_response =
        recv_signed_event_of_kind(&alice_owner_rx, nostr_double_ratchet::INVITE_RESPONSE_KIND);
    let (bob_owner_peer, bob_owner_remote_device_id) = import_session_from_response(
        &bob_owner_public_invite,
        bob_owner_keys.secret_key().to_secret_bytes(),
        &bob_owner_mgr,
        &bob_owner_response,
    )?;
    assert_eq!(bob_owner_peer, alice_owner_pubkey);
    assert_eq!(bob_owner_remote_device_id, alice_owner_device_id);
    drain_events(&alice_owner_rx);
    drain_events(&bob_owner_rx);

    let accepted =
        alice_owner_mgr.accept_invite(&alice_linked_link_invite, Some(alice_owner_pubkey))?;
    assert_eq!(accepted.owner_pubkey, alice_owner_pubkey);
    let (alice_link_response, alice_link_bootstrap) =
        recv_invite_response_and_message_event(&alice_owner_rx);
    let (alice_link_peer, alice_link_remote_device_id) = import_session_from_response(
        &alice_linked_link_invite,
        alice_new_device_keys.secret_key().to_secret_bytes(),
        &alice_linked_mgr,
        &alice_link_response,
    )?;
    assert_eq!(alice_link_peer, alice_owner_pubkey);
    assert_eq!(alice_link_remote_device_id, alice_owner_device_id);
    alice_linked_mgr.process_received_event(alice_link_bootstrap);
    drain_events(&alice_owner_rx);
    drain_events(&alice_linked_rx);

    let accepted = bob_owner_mgr.accept_invite(&bob_linked_link_invite, Some(bob_owner_pubkey))?;
    assert_eq!(accepted.owner_pubkey, bob_owner_pubkey);
    let (bob_link_response, bob_link_bootstrap) =
        recv_invite_response_and_message_event(&bob_owner_rx);
    let (bob_link_peer, bob_link_remote_device_id) = import_session_from_response(
        &bob_linked_link_invite,
        bob_linked_keys.secret_key().to_secret_bytes(),
        &bob_linked_mgr,
        &bob_link_response,
    )?;
    assert_eq!(bob_link_peer, bob_owner_pubkey);
    assert_eq!(bob_link_remote_device_id, bob_owner_device_id);
    bob_linked_mgr.process_received_event(bob_link_bootstrap);
    drain_events(&bob_owner_rx);
    drain_events(&bob_linked_rx);

    let alice_app_keys = AppKeys::new(vec![
        DeviceEntry::new(alice_owner_pubkey, 1),
        DeviceEntry::new(alice_new_device_pubkey, 2),
    ]);
    let alice_app_keys_event = alice_app_keys
        .get_event(alice_owner_pubkey)
        .sign_with_keys(&alice_owner_keys)?;
    alice_linked_mgr.process_received_event(alice_app_keys_event.clone());
    bob_owner_mgr.process_received_event(alice_app_keys_event.clone());
    bob_linked_mgr.process_received_event(alice_app_keys_event.clone());

    let bob_app_keys = AppKeys::new(vec![
        DeviceEntry::new(bob_owner_pubkey, 1),
        DeviceEntry::new(bob_linked_pubkey, 2),
    ]);
    let bob_app_keys_event = bob_app_keys
        .get_event(bob_owner_pubkey)
        .sign_with_keys(&bob_owner_keys)?;
    alice_linked_mgr.process_received_event(bob_app_keys_event.clone());
    alice_owner_mgr.process_received_event(bob_app_keys_event.clone());
    let accepted =
        alice_linked_mgr.accept_invite(&bob_owner_public_invite, Some(bob_owner_pubkey))?;
    assert!(
        accepted.created_new_session,
        "expected linked sender to create a direct bob-owner session from the public invite",
    );
    let bob_owner_direct_response =
        recv_signed_event_of_kind(&alice_linked_rx, nostr_double_ratchet::INVITE_RESPONSE_KIND);
    let (bob_owner_direct_peer, bob_owner_direct_device_id) = import_session_from_response(
        &bob_owner_public_invite,
        bob_owner_keys.secret_key().to_secret_bytes(),
        &bob_owner_mgr,
        &bob_owner_direct_response,
    )?;
    assert_eq!(bob_owner_direct_peer, alice_owner_pubkey);
    assert_eq!(bob_owner_direct_device_id, alice_new_device_id);
    drain_events(&alice_linked_rx);
    drain_events(&alice_owner_rx);
    drain_events(&bob_owner_rx);
    drain_events(&bob_linked_rx);

    let bob_linked_public_invite_event = bob_linked_public_invite
        .get_event()?
        .sign_with_keys(&bob_linked_keys)?;
    alice_owner_mgr.process_received_event(bob_linked_public_invite_event.clone());
    let bob_linked_public_response =
        recv_signed_event_of_kind(&alice_owner_rx, nostr_double_ratchet::INVITE_RESPONSE_KIND);
    let (bob_linked_peer, bob_linked_remote_device_id) = import_session_from_response(
        &bob_linked_public_invite,
        bob_linked_keys.secret_key().to_secret_bytes(),
        &bob_linked_mgr,
        &bob_linked_public_response,
    )?;
    assert_eq!(bob_linked_peer, alice_owner_pubkey);
    assert_eq!(bob_linked_remote_device_id, alice_owner_device_id);
    drain_events(&alice_owner_rx);
    drain_events(&bob_linked_rx);

    let alice_linked_public_invite_event = alice_linked_public_invite
        .get_event()?
        .sign_with_keys(&alice_new_device_keys)?;
    bob_linked_mgr.process_received_event(alice_linked_public_invite_event);
    let alice_linked_public_response =
        recv_signed_event_of_kind(&bob_linked_rx, nostr_double_ratchet::INVITE_RESPONSE_KIND);
    let (alice_linked_peer, alice_linked_remote_device_id) = import_session_from_response(
        &alice_linked_public_invite,
        alice_new_device_keys.secret_key().to_secret_bytes(),
        &alice_linked_mgr,
        &alice_linked_public_response,
    )?;
    assert_eq!(alice_linked_peer, bob_owner_pubkey);
    assert_eq!(alice_linked_remote_device_id, bob_linked_device_id);
    drain_events(&alice_linked_rx);
    drain_events(&bob_linked_rx);

    alice_owner_mgr.send_text(bob_owner_pubkey, "seed existing chat".to_string(), None)?;
    let seed_messages = recv_message_events(&alice_owner_rx, 3);
    for event in &seed_messages {
        alice_linked_mgr.process_received_event(event.clone());
        bob_owner_mgr.process_received_event(event.clone());
        bob_linked_mgr.process_received_event(event.clone());
    }

    let alice_self_seed =
        recv_decrypted_containing(&alice_linked_rx, "\"content\":\"seed existing chat\"");
    let owner_seed = recv_decrypted_containing(&bob_owner_rx, "\"content\":\"seed existing chat\"");
    let linked_seed =
        recv_decrypted_containing(&bob_linked_rx, "\"content\":\"seed existing chat\"");
    assert!(alice_self_seed.contains("\"content\":\"seed existing chat\""));
    assert!(owner_seed.contains("\"content\":\"seed existing chat\""));
    assert!(linked_seed.contains("\"content\":\"seed existing chat\""));
    drain_events(&alice_linked_rx);
    drain_events(&bob_owner_rx);
    drain_events(&bob_linked_rx);

    bob_linked_mgr.send_text(
        alice_owner_pubkey,
        "before restart reaches linked receiver".to_string(),
        None,
    )?;
    let before_restart_messages = recv_message_events(&bob_linked_rx, 3);
    for event in &before_restart_messages {
        alice_owner_mgr.process_received_event(event.clone());
        alice_linked_mgr.process_received_event(event.clone());
        bob_owner_mgr.process_received_event(event.clone());
    }

    let before_restart = recv_decrypted_containing(
        &alice_linked_rx,
        "\"content\":\"before restart reaches linked receiver\"",
    );
    assert!(before_restart.contains("\"content\":\"before restart reaches linked receiver\""));
    drain_events(&alice_linked_rx);
    drain_events(&alice_owner_rx);
    drain_events(&bob_owner_rx);

    let (alice_linked_restarted_tx, alice_linked_restarted_rx) = crossbeam_channel::unbounded();
    let alice_linked_restarted_mgr = SessionManager::new(
        alice_new_device_pubkey,
        alice_new_device_keys.secret_key().to_secret_bytes(),
        alice_new_device_id.clone(),
        alice_owner_pubkey,
        alice_linked_restarted_tx,
        Some(alice_linked_storage.clone()),
        None,
    );
    alice_linked_restarted_mgr.init()?;
    drain_events(&alice_linked_restarted_rx);
    alice_linked_restarted_mgr.setup_user(bob_owner_pubkey);
    alice_linked_restarted_mgr.setup_user(alice_owner_pubkey);
    drain_events(&alice_linked_restarted_rx);
    alice_linked_restarted_mgr.process_received_event(bob_app_keys_event.clone());
    alice_linked_restarted_mgr.process_received_event(alice_app_keys_event.clone());
    alice_linked_restarted_mgr.process_received_event(
        bob_owner_public_invite
            .get_event()?
            .sign_with_keys(&bob_owner_keys)?,
    );
    alice_linked_restarted_mgr.process_received_event(bob_linked_public_invite_event.clone());
    alice_linked_restarted_mgr.process_received_event(alice_linked_public_response.clone());
    drain_events(&alice_linked_restarted_rx);
    drain_events(&bob_owner_rx);

    bob_linked_mgr.send_text(
        alice_owner_pubkey,
        "after restart reaches linked receiver".to_string(),
        None,
    )?;
    let after_restart_messages = recv_message_events(&bob_linked_rx, 3);
    for event in &after_restart_messages {
        alice_owner_mgr.process_received_event(event.clone());
        alice_linked_restarted_mgr.process_received_event(event.clone());
        bob_owner_mgr.process_received_event(event.clone());
    }

    let after_restart = recv_decrypted_containing(
        &alice_linked_restarted_rx,
        "\"content\":\"after restart reaches linked receiver\"",
    );
    assert!(after_restart.contains("\"content\":\"after restart reaches linked receiver\""));
    drain_events(&alice_linked_restarted_rx);
    drain_events(&alice_owner_rx);
    drain_events(&bob_owner_rx);
    drain_events(&bob_linked_rx);

    alice_linked_restarted_mgr.send_text(
        bob_owner_pubkey,
        "after restart linked sender keeps multi-device fanout".to_string(),
        None,
    )?;
    let post_restart_fanout = recv_message_events(&alice_linked_restarted_rx, 3);
    assert_eq!(post_restart_fanout.len(), 3);
    for event in &post_restart_fanout {
        alice_owner_mgr.process_received_event(event.clone());
        bob_owner_mgr.process_received_event(event.clone());
        bob_linked_mgr.process_received_event(event.clone());
    }

    let alice_owner_self_sync = recv_decrypted_containing(
        &alice_owner_rx,
        "\"content\":\"after restart linked sender keeps multi-device fanout\"",
    );
    let bob_owner_received = recv_decrypted_containing(
        &bob_owner_rx,
        "\"content\":\"after restart linked sender keeps multi-device fanout\"",
    );
    let bob_linked_received = recv_decrypted_containing(
        &bob_linked_rx,
        "\"content\":\"after restart linked sender keeps multi-device fanout\"",
    );
    assert!(alice_owner_self_sync
        .contains("\"content\":\"after restart linked sender keeps multi-device fanout\""));
    assert!(bob_owner_received
        .contains("\"content\":\"after restart linked sender keeps multi-device fanout\""));
    assert!(bob_linked_received
        .contains("\"content\":\"after restart linked sender keeps multi-device fanout\""));

    Ok(())
}

#[test]
fn test_linked_receiver_restores_and_receives_after_restart_with_file_storage() -> Result<()> {
    let alice_owner_keys = Keys::generate();
    let alice_owner_pubkey = alice_owner_keys.public_key();
    let alice_owner_device_id = alice_owner_pubkey.to_hex();

    let alice_new_device_keys = Keys::generate();
    let alice_new_device_pubkey = alice_new_device_keys.public_key();
    let alice_new_device_id = alice_new_device_pubkey.to_hex();

    let bob_owner_keys = Keys::generate();
    let bob_owner_pubkey = bob_owner_keys.public_key();
    let bob_owner_device_id = bob_owner_pubkey.to_hex();

    let bob_linked_keys = Keys::generate();
    let bob_linked_pubkey = bob_linked_keys.public_key();
    let bob_linked_device_id = bob_linked_pubkey.to_hex();

    let alice_linked_link_invite = new_link_invite(
        alice_new_device_pubkey,
        alice_new_device_id.clone(),
        alice_owner_pubkey,
    )?;
    let bob_linked_link_invite = new_link_invite(
        bob_linked_pubkey,
        bob_linked_device_id.clone(),
        bob_owner_pubkey,
    )?;

    let bob_owner_public_invite =
        Invite::create_new(bob_owner_pubkey, Some(bob_owner_device_id.clone()), None)?;
    let bob_linked_public_invite = new_public_invite(
        bob_linked_pubkey,
        bob_linked_device_id.clone(),
        bob_owner_pubkey,
    )?;
    let alice_linked_public_invite = new_public_invite(
        alice_new_device_pubkey,
        alice_new_device_id.clone(),
        alice_owner_pubkey,
    )?;

    let temp_dir = tempfile::tempdir().expect("temp dir");
    let base = temp_dir.path();

    let alice_owner_storage: Arc<dyn StorageAdapter> =
        Arc::new(FileStorageAdapter::new(base.join("alice-owner"))?);
    let alice_linked_storage: Arc<dyn StorageAdapter> =
        Arc::new(FileStorageAdapter::new(base.join("alice-linked"))?);
    let bob_owner_storage: Arc<dyn StorageAdapter> =
        Arc::new(FileStorageAdapter::new(base.join("bob-owner"))?);
    let bob_linked_storage: Arc<dyn StorageAdapter> =
        Arc::new(FileStorageAdapter::new(base.join("bob-linked"))?);

    let (alice_owner_tx, alice_owner_rx) = crossbeam_channel::unbounded();
    let (alice_linked_tx, alice_linked_rx) = crossbeam_channel::unbounded();
    let (bob_owner_tx, bob_owner_rx) = crossbeam_channel::unbounded();
    let (bob_linked_tx, bob_linked_rx) = crossbeam_channel::unbounded();

    let alice_owner_mgr = SessionManager::new(
        alice_owner_pubkey,
        alice_owner_keys.secret_key().to_secret_bytes(),
        alice_owner_device_id.clone(),
        alice_owner_pubkey,
        alice_owner_tx,
        Some(alice_owner_storage),
        None,
    );
    let alice_linked_mgr = SessionManager::new(
        alice_new_device_pubkey,
        alice_new_device_keys.secret_key().to_secret_bytes(),
        alice_new_device_id.clone(),
        alice_owner_pubkey,
        alice_linked_tx,
        Some(alice_linked_storage),
        None,
    );
    let bob_owner_mgr = SessionManager::new(
        bob_owner_pubkey,
        bob_owner_keys.secret_key().to_secret_bytes(),
        bob_owner_device_id.clone(),
        bob_owner_pubkey,
        bob_owner_tx,
        Some(bob_owner_storage),
        None,
    );
    let bob_linked_mgr = SessionManager::new(
        bob_linked_pubkey,
        bob_linked_keys.secret_key().to_secret_bytes(),
        bob_linked_device_id.clone(),
        bob_owner_pubkey,
        bob_linked_tx,
        Some(bob_linked_storage),
        None,
    );

    alice_owner_mgr.init()?;
    alice_linked_mgr.init()?;
    bob_owner_mgr.init()?;
    bob_linked_mgr.init()?;
    drain_events(&alice_owner_rx);
    drain_events(&alice_linked_rx);
    drain_events(&bob_owner_rx);
    drain_events(&bob_linked_rx);

    let accepted =
        alice_owner_mgr.accept_invite(&bob_owner_public_invite, Some(bob_owner_pubkey))?;
    assert_eq!(accepted.owner_pubkey, bob_owner_pubkey);
    let bob_owner_response =
        recv_signed_event_of_kind(&alice_owner_rx, nostr_double_ratchet::INVITE_RESPONSE_KIND);
    let (bob_owner_peer, bob_owner_remote_device_id) = import_session_from_response(
        &bob_owner_public_invite,
        bob_owner_keys.secret_key().to_secret_bytes(),
        &bob_owner_mgr,
        &bob_owner_response,
    )?;
    assert_eq!(bob_owner_peer, alice_owner_pubkey);
    assert_eq!(bob_owner_remote_device_id, alice_owner_device_id);
    drain_events(&alice_owner_rx);
    drain_events(&bob_owner_rx);

    let accepted =
        alice_owner_mgr.accept_invite(&alice_linked_link_invite, Some(alice_owner_pubkey))?;
    assert_eq!(accepted.owner_pubkey, alice_owner_pubkey);
    let (alice_link_response, alice_link_bootstrap) =
        recv_invite_response_and_message_event(&alice_owner_rx);
    let (alice_link_peer, alice_link_remote_device_id) = import_session_from_response(
        &alice_linked_link_invite,
        alice_new_device_keys.secret_key().to_secret_bytes(),
        &alice_linked_mgr,
        &alice_link_response,
    )?;
    assert_eq!(alice_link_peer, alice_owner_pubkey);
    assert_eq!(alice_link_remote_device_id, alice_owner_device_id);
    alice_linked_mgr.process_received_event(alice_link_bootstrap);
    drain_events(&alice_owner_rx);
    drain_events(&alice_linked_rx);

    let accepted = bob_owner_mgr.accept_invite(&bob_linked_link_invite, Some(bob_owner_pubkey))?;
    assert_eq!(accepted.owner_pubkey, bob_owner_pubkey);
    let (bob_link_response, bob_link_bootstrap) =
        recv_invite_response_and_message_event(&bob_owner_rx);
    let (bob_link_peer, bob_link_remote_device_id) = import_session_from_response(
        &bob_linked_link_invite,
        bob_linked_keys.secret_key().to_secret_bytes(),
        &bob_linked_mgr,
        &bob_link_response,
    )?;
    assert_eq!(bob_link_peer, bob_owner_pubkey);
    assert_eq!(bob_link_remote_device_id, bob_owner_device_id);
    bob_linked_mgr.process_received_event(bob_link_bootstrap);
    drain_events(&bob_owner_rx);
    drain_events(&bob_linked_rx);

    let alice_app_keys = AppKeys::new(vec![
        DeviceEntry::new(alice_owner_pubkey, 1),
        DeviceEntry::new(alice_new_device_pubkey, 2),
    ]);
    let alice_app_keys_event = alice_app_keys
        .get_event(alice_owner_pubkey)
        .sign_with_keys(&alice_owner_keys)?;
    bob_owner_mgr.process_received_event(alice_app_keys_event.clone());
    bob_linked_mgr.process_received_event(alice_app_keys_event.clone());

    let bob_app_keys = AppKeys::new(vec![
        DeviceEntry::new(bob_owner_pubkey, 1),
        DeviceEntry::new(bob_linked_pubkey, 2),
    ]);
    let bob_app_keys_event = bob_app_keys
        .get_event(bob_owner_pubkey)
        .sign_with_keys(&bob_owner_keys)?;
    alice_owner_mgr.process_received_event(bob_app_keys_event.clone());
    drain_events(&alice_owner_rx);
    drain_events(&bob_owner_rx);
    drain_events(&bob_linked_rx);

    let bob_linked_public_invite_event = bob_linked_public_invite
        .get_event()?
        .sign_with_keys(&bob_linked_keys)?;
    alice_owner_mgr.process_received_event(bob_linked_public_invite_event.clone());
    let bob_linked_public_response =
        recv_signed_event_of_kind(&alice_owner_rx, nostr_double_ratchet::INVITE_RESPONSE_KIND);
    let (bob_linked_peer, bob_linked_remote_device_id) = import_session_from_response(
        &bob_linked_public_invite,
        bob_linked_keys.secret_key().to_secret_bytes(),
        &bob_linked_mgr,
        &bob_linked_public_response,
    )?;
    assert_eq!(bob_linked_peer, alice_owner_pubkey);
    assert_eq!(bob_linked_remote_device_id, alice_owner_device_id);
    drain_events(&alice_owner_rx);
    drain_events(&bob_linked_rx);

    let alice_linked_public_invite_event = alice_linked_public_invite
        .get_event()?
        .sign_with_keys(&alice_new_device_keys)?;
    bob_linked_mgr.process_received_event(alice_linked_public_invite_event);
    let alice_linked_public_response =
        recv_signed_event_of_kind(&bob_linked_rx, nostr_double_ratchet::INVITE_RESPONSE_KIND);
    let (alice_linked_peer, alice_linked_remote_device_id) = import_session_from_response(
        &alice_linked_public_invite,
        alice_new_device_keys.secret_key().to_secret_bytes(),
        &alice_linked_mgr,
        &alice_linked_public_response,
    )?;
    assert_eq!(alice_linked_peer, bob_owner_pubkey);
    assert_eq!(alice_linked_remote_device_id, bob_linked_device_id);
    drain_events(&alice_linked_rx);
    drain_events(&bob_linked_rx);

    alice_owner_mgr.send_text(bob_owner_pubkey, "seed existing chat".to_string(), None)?;
    let seed_messages = recv_message_events(&alice_owner_rx, 3);
    for event in &seed_messages {
        alice_linked_mgr.process_received_event(event.clone());
        bob_owner_mgr.process_received_event(event.clone());
        bob_linked_mgr.process_received_event(event.clone());
    }

    let alice_self_seed =
        recv_decrypted_containing(&alice_linked_rx, "\"content\":\"seed existing chat\"");
    let owner_seed = recv_decrypted_containing(&bob_owner_rx, "\"content\":\"seed existing chat\"");
    let linked_seed =
        recv_decrypted_containing(&bob_linked_rx, "\"content\":\"seed existing chat\"");
    assert!(alice_self_seed.contains("\"content\":\"seed existing chat\""));
    assert!(owner_seed.contains("\"content\":\"seed existing chat\""));
    assert!(linked_seed.contains("\"content\":\"seed existing chat\""));
    drain_events(&alice_linked_rx);
    drain_events(&bob_owner_rx);
    drain_events(&bob_linked_rx);

    bob_linked_mgr.send_text(
        alice_owner_pubkey,
        "before restart reaches linked receiver".to_string(),
        None,
    )?;
    let before_restart_messages = recv_message_events(&bob_linked_rx, 3);
    for event in &before_restart_messages {
        alice_owner_mgr.process_received_event(event.clone());
        alice_linked_mgr.process_received_event(event.clone());
        bob_owner_mgr.process_received_event(event.clone());
    }

    let before_restart = recv_decrypted_containing(
        &alice_linked_rx,
        "\"content\":\"before restart reaches linked receiver\"",
    );
    assert!(before_restart.contains("\"content\":\"before restart reaches linked receiver\""));
    drain_events(&alice_linked_rx);
    drain_events(&alice_owner_rx);
    drain_events(&bob_owner_rx);

    let stored_before_restart =
        load_stored_user_record(&base.join("alice-linked"), bob_owner_pubkey);
    assert!(
        device_has_receiving_session(&stored_before_restart, &bob_linked_device_id),
        "expected alice-linked to persist a receive-capable Bob-linked session before restart",
    );

    let (alice_linked_restarted_tx, alice_linked_restarted_rx) = crossbeam_channel::unbounded();
    let alice_linked_restarted_storage: Arc<dyn StorageAdapter> =
        Arc::new(FileStorageAdapter::new(base.join("alice-linked"))?);
    let alice_linked_restarted_mgr = SessionManager::new(
        alice_new_device_pubkey,
        alice_new_device_keys.secret_key().to_secret_bytes(),
        alice_new_device_id.clone(),
        alice_owner_pubkey,
        alice_linked_restarted_tx,
        Some(alice_linked_restarted_storage),
        None,
    );
    alice_linked_restarted_mgr.init()?;
    drain_events(&alice_linked_restarted_rx);
    alice_linked_restarted_mgr.setup_user(bob_owner_pubkey);
    alice_linked_restarted_mgr.setup_user(alice_owner_pubkey);
    drain_events(&alice_linked_restarted_rx);

    alice_linked_restarted_mgr.process_received_event(bob_app_keys_event.clone());
    alice_linked_restarted_mgr.process_received_event(alice_app_keys_event.clone());
    alice_linked_restarted_mgr.process_received_event(
        bob_owner_public_invite
            .get_event()?
            .sign_with_keys(&bob_owner_keys)?,
    );
    alice_linked_restarted_mgr.process_received_event(bob_linked_public_invite_event.clone());
    alice_linked_restarted_mgr.process_received_event(alice_linked_public_response.clone());
    drain_events(&alice_linked_restarted_rx);

    let stored_after_replay = load_stored_user_record(&base.join("alice-linked"), bob_owner_pubkey);
    assert!(
        device_has_receiving_session(&stored_after_replay, &bob_linked_device_id),
        "expected replayed AppKeys/invites to preserve Bob-linked receive state after restart",
    );

    bob_linked_mgr.send_text(
        alice_owner_pubkey,
        "after restart reaches linked receiver".to_string(),
        None,
    )?;
    let after_restart_messages = recv_message_events(&bob_linked_rx, 3);
    for event in &after_restart_messages {
        alice_owner_mgr.process_received_event(event.clone());
        alice_linked_restarted_mgr.process_received_event(event.clone());
        bob_owner_mgr.process_received_event(event.clone());
    }

    let after_restart = recv_decrypted_containing(
        &alice_linked_restarted_rx,
        "\"content\":\"after restart reaches linked receiver\"",
    );
    assert!(after_restart.contains("\"content\":\"after restart reaches linked receiver\""));

    Ok(())
}

#[test]
fn test_existing_peer_fanout_survives_sender_restart_with_file_storage() -> Result<()> {
    let alice_owner_keys = Keys::generate();
    let alice_owner_pubkey = alice_owner_keys.public_key();
    let alice_owner_device_id = alice_owner_pubkey.to_hex();

    let alice_new_device_keys = Keys::generate();
    let alice_new_device_pubkey = alice_new_device_keys.public_key();
    let alice_new_device_id = alice_new_device_pubkey.to_hex();

    let bob_keys = Keys::generate();
    let bob_pubkey = bob_keys.public_key();
    let bob_device_id = bob_pubkey.to_hex();

    let owner_invite = Invite::create_new(
        alice_owner_pubkey,
        Some(alice_owner_device_id.clone()),
        None,
    )?;
    let new_device_invite = Invite::create_new(
        alice_new_device_pubkey,
        Some(alice_new_device_id.clone()),
        None,
    )?;

    let temp_dir = tempfile::tempdir().expect("temp dir");
    let base = temp_dir.path();

    let alice_owner_storage =
        Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>;
    let alice_new_storage =
        Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>;
    let bob_storage: Arc<dyn StorageAdapter> = Arc::new(FileStorageAdapter::new(base.join("bob"))?);

    let (alice_owner_tx, alice_owner_rx) = crossbeam_channel::unbounded();
    let (alice_new_tx, alice_new_rx) = crossbeam_channel::unbounded();
    let (bob_tx, bob_rx) = crossbeam_channel::unbounded();

    let alice_owner_mgr = SessionManager::new(
        alice_owner_pubkey,
        alice_owner_keys.secret_key().to_secret_bytes(),
        alice_owner_device_id.clone(),
        alice_owner_pubkey,
        alice_owner_tx,
        Some(alice_owner_storage),
        Some(owner_invite.clone()),
    );
    let alice_new_mgr = SessionManager::new(
        alice_new_device_pubkey,
        alice_new_device_keys.secret_key().to_secret_bytes(),
        alice_new_device_id.clone(),
        alice_owner_pubkey,
        alice_new_tx,
        Some(alice_new_storage),
        Some(new_device_invite.clone()),
    );
    let bob_mgr = SessionManager::new(
        bob_pubkey,
        bob_keys.secret_key().to_secret_bytes(),
        bob_device_id.clone(),
        bob_pubkey,
        bob_tx,
        Some(bob_storage),
        None,
    );

    alice_owner_mgr.init()?;
    alice_new_mgr.init()?;
    bob_mgr.init()?;
    drain_events(&alice_owner_rx);
    drain_events(&alice_new_rx);
    drain_events(&bob_rx);

    let accepted = bob_mgr.accept_invite(&owner_invite, Some(alice_owner_pubkey))?;
    assert_eq!(accepted.owner_pubkey, alice_owner_pubkey);
    let owner_response =
        recv_signed_event_of_kind(&bob_rx, nostr_double_ratchet::INVITE_RESPONSE_KIND);
    alice_owner_mgr.process_received_event(owner_response);
    drain_events(&alice_owner_rx);
    drain_events(&bob_rx);

    bob_mgr.send_text(alice_owner_pubkey, "seed existing chat".to_string(), None)?;
    let seed_message = recv_message_events(&bob_rx, 1)
        .into_iter()
        .next()
        .expect("expected seed message");
    alice_owner_mgr.process_received_event(seed_message);
    let initial_decrypted =
        recv_decrypted_containing(&alice_owner_rx, "\"content\":\"seed existing chat\"");
    assert!(initial_decrypted.contains("\"content\":\"seed existing chat\""));
    drain_events(&alice_owner_rx);
    drain_events(&bob_rx);

    let alice_app_keys = AppKeys::new(vec![
        DeviceEntry::new(alice_owner_pubkey, 1),
        DeviceEntry::new(alice_new_device_pubkey, 2),
    ]);
    let alice_app_keys_event = alice_app_keys
        .get_event(alice_owner_pubkey)
        .sign_with_keys(&alice_owner_keys)?;
    bob_mgr.process_received_event(alice_app_keys_event.clone());
    drain_events(&bob_rx);

    let new_device_invite_event = new_device_invite
        .get_event()?
        .sign_with_keys(&alice_new_device_keys)?;
    bob_mgr.process_received_event(new_device_invite_event.clone());
    let new_device_response =
        recv_signed_event_of_kind(&bob_rx, nostr_double_ratchet::INVITE_RESPONSE_KIND);
    alice_new_mgr.process_received_event(new_device_response);
    drain_events(&alice_new_rx);
    drain_events(&bob_rx);

    bob_mgr.send_text(
        alice_owner_pubkey,
        "fanout before sender restart".to_string(),
        None,
    )?;
    let before_restart_messages = recv_message_events(&bob_rx, 2);
    assert_eq!(before_restart_messages.len(), 2);
    for event in &before_restart_messages {
        alice_owner_mgr.process_received_event(event.clone());
        alice_new_mgr.process_received_event(event.clone());
    }

    let owner_before_restart = recv_decrypted_containing(
        &alice_owner_rx,
        "\"content\":\"fanout before sender restart\"",
    );
    let linked_before_restart = recv_decrypted_containing(
        &alice_new_rx,
        "\"content\":\"fanout before sender restart\"",
    );
    assert!(owner_before_restart.contains("\"content\":\"fanout before sender restart\""));
    assert!(linked_before_restart.contains("\"content\":\"fanout before sender restart\""));
    drain_events(&alice_owner_rx);
    drain_events(&alice_new_rx);
    drain_events(&bob_rx);

    let stored_before_restart = load_stored_user_record(&base.join("bob"), alice_owner_pubkey);
    assert!(
        device_has_send_capable_session(&stored_before_restart, &alice_owner_device_id),
        "expected sender storage to retain a send-capable owner-device session before restart",
    );
    assert!(
        device_has_send_capable_session(&stored_before_restart, &alice_new_device_id),
        "expected sender storage to retain a send-capable linked-device session before restart",
    );

    let (bob_restarted_tx, bob_restarted_rx) = crossbeam_channel::unbounded();
    let bob_restarted_storage: Arc<dyn StorageAdapter> =
        Arc::new(FileStorageAdapter::new(base.join("bob"))?);
    let bob_restarted_mgr = SessionManager::new(
        bob_pubkey,
        bob_keys.secret_key().to_secret_bytes(),
        bob_device_id,
        bob_pubkey,
        bob_restarted_tx,
        Some(bob_restarted_storage),
        None,
    );
    bob_restarted_mgr.init()?;
    drain_events(&bob_restarted_rx);
    bob_restarted_mgr.setup_user(alice_owner_pubkey);
    drain_events(&bob_restarted_rx);
    bob_restarted_mgr.process_received_event(alice_app_keys_event);
    bob_restarted_mgr.process_received_event(new_device_invite_event);
    drain_events(&bob_restarted_rx);

    let stored_after_replay = load_stored_user_record(&base.join("bob"), alice_owner_pubkey);
    assert!(
        device_has_send_capable_session(&stored_after_replay, &alice_owner_device_id),
        "expected replayed AppKeys/invite to preserve the owner-device send path after restart",
    );
    assert!(
        device_has_send_capable_session(&stored_after_replay, &alice_new_device_id),
        "expected replayed AppKeys/invite to preserve the linked-device send path after restart",
    );

    bob_restarted_mgr.send_text(
        alice_owner_pubkey,
        "fanout after sender restart".to_string(),
        None,
    )?;
    let after_restart_messages = recv_message_events(&bob_restarted_rx, 2);
    assert_eq!(after_restart_messages.len(), 2);
    for event in &after_restart_messages {
        alice_owner_mgr.process_received_event(event.clone());
        alice_new_mgr.process_received_event(event.clone());
    }

    let owner_after_restart = recv_decrypted_containing(
        &alice_owner_rx,
        "\"content\":\"fanout after sender restart\"",
    );
    let linked_after_restart =
        recv_decrypted_containing(&alice_new_rx, "\"content\":\"fanout after sender restart\"");
    assert!(owner_after_restart.contains("\"content\":\"fanout after sender restart\""));
    assert!(linked_after_restart.contains("\"content\":\"fanout after sender restart\""));

    Ok(())
}

#[test]
fn test_linked_sender_fans_out_to_newly_added_peer_device() -> Result<()> {
    let alice_owner_keys = Keys::generate();
    let alice_owner_pubkey = alice_owner_keys.public_key();
    let alice_owner_device_id = alice_owner_pubkey.to_hex();

    let alice_new_device_keys = Keys::generate();
    let alice_new_device_pubkey = alice_new_device_keys.public_key();
    let alice_new_device_id = alice_new_device_pubkey.to_hex();

    let bob_owner_keys = Keys::generate();
    let bob_owner_pubkey = bob_owner_keys.public_key();
    let bob_owner_device_id = bob_owner_pubkey.to_hex();

    let bob_linked_keys = Keys::generate();
    let bob_linked_pubkey = bob_linked_keys.public_key();
    let bob_linked_device_id = bob_linked_pubkey.to_hex();

    let bob_owner_public_invite =
        Invite::create_new(bob_owner_pubkey, Some(bob_owner_device_id.clone()), None)?;
    let bob_linked_public_invite = new_public_invite(
        bob_linked_pubkey,
        bob_linked_device_id.clone(),
        bob_owner_pubkey,
    )?;
    let bob_linked_link_invite = new_link_invite(
        bob_linked_pubkey,
        bob_linked_device_id.clone(),
        bob_owner_pubkey,
    )?;
    let alice_linked_public_invite = new_public_invite(
        alice_new_device_pubkey,
        alice_new_device_id.clone(),
        alice_owner_pubkey,
    )?;
    let alice_linked_link_invite = new_link_invite(
        alice_new_device_pubkey,
        alice_new_device_id.clone(),
        alice_owner_pubkey,
    )?;

    let (alice_owner_tx, alice_owner_rx) = crossbeam_channel::unbounded();
    let (alice_new_tx, alice_new_rx) = crossbeam_channel::unbounded();
    let (bob_owner_tx, bob_owner_rx) = crossbeam_channel::unbounded();
    let (bob_linked_tx, bob_linked_rx) = crossbeam_channel::unbounded();

    let alice_owner_mgr = SessionManager::new(
        alice_owner_pubkey,
        alice_owner_keys.secret_key().to_secret_bytes(),
        alice_owner_device_id.clone(),
        alice_owner_pubkey,
        alice_owner_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        None,
    );
    let alice_new_mgr = SessionManager::new(
        alice_new_device_pubkey,
        alice_new_device_keys.secret_key().to_secret_bytes(),
        alice_new_device_id.clone(),
        alice_owner_pubkey,
        alice_new_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        None,
    );
    let bob_owner_mgr = SessionManager::new(
        bob_owner_pubkey,
        bob_owner_keys.secret_key().to_secret_bytes(),
        bob_owner_device_id.clone(),
        bob_owner_pubkey,
        bob_owner_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        None,
    );
    let bob_linked_mgr = SessionManager::new(
        bob_linked_pubkey,
        bob_linked_keys.secret_key().to_secret_bytes(),
        bob_linked_device_id.clone(),
        bob_owner_pubkey,
        bob_linked_tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        None,
    );

    alice_owner_mgr.init()?;
    alice_new_mgr.init()?;
    bob_owner_mgr.init()?;
    bob_linked_mgr.init()?;
    drain_events(&alice_owner_rx);
    drain_events(&alice_new_rx);
    drain_events(&bob_owner_rx);
    drain_events(&bob_linked_rx);

    // Mirror app bootstrap: Alice accepts Bob's public invite so Alice can send first.
    let accepted =
        alice_owner_mgr.accept_invite(&bob_owner_public_invite, Some(bob_owner_pubkey))?;
    assert_eq!(accepted.owner_pubkey, bob_owner_pubkey);
    let bob_owner_response =
        recv_signed_event_of_kind(&alice_owner_rx, nostr_double_ratchet::INVITE_RESPONSE_KIND);
    let (bob_owner_peer, bob_owner_remote_device_id) = import_session_from_response(
        &bob_owner_public_invite,
        bob_owner_keys.secret_key().to_secret_bytes(),
        &bob_owner_mgr,
        &bob_owner_response,
    )?;
    assert_eq!(bob_owner_peer, alice_owner_pubkey);
    assert_eq!(bob_owner_remote_device_id, alice_owner_device_id);
    drain_events(&alice_owner_rx);
    drain_events(&bob_owner_rx);

    // Link Alice's new device and deliver the owner-side bootstrap packet.
    let accepted =
        alice_owner_mgr.accept_invite(&alice_linked_link_invite, Some(alice_owner_pubkey))?;
    assert_eq!(accepted.owner_pubkey, alice_owner_pubkey);
    let (alice_link_response, alice_link_bootstrap) =
        recv_invite_response_and_message_event(&alice_owner_rx);
    let (alice_link_peer, alice_link_remote_device_id) = import_session_from_response(
        &alice_linked_link_invite,
        alice_new_device_keys.secret_key().to_secret_bytes(),
        &alice_new_mgr,
        &alice_link_response,
    )?;
    assert_eq!(alice_link_peer, alice_owner_pubkey);
    assert_eq!(alice_link_remote_device_id, alice_owner_device_id);
    alice_new_mgr.process_received_event(alice_link_bootstrap);
    drain_events(&alice_owner_rx);
    drain_events(&alice_new_rx);

    // Link Bob's new device and deliver the owner-side bootstrap packet.
    let accepted = bob_owner_mgr.accept_invite(&bob_linked_link_invite, Some(bob_owner_pubkey))?;
    assert_eq!(accepted.owner_pubkey, bob_owner_pubkey);
    let (bob_link_response, bob_link_bootstrap) =
        recv_invite_response_and_message_event(&bob_owner_rx);
    let (bob_link_peer, bob_link_remote_device_id) = import_session_from_response(
        &bob_linked_link_invite,
        bob_linked_keys.secret_key().to_secret_bytes(),
        &bob_linked_mgr,
        &bob_link_response,
    )?;
    assert_eq!(bob_link_peer, bob_owner_pubkey);
    assert_eq!(bob_link_remote_device_id, bob_owner_device_id);
    bob_linked_mgr.process_received_event(bob_link_bootstrap);
    drain_events(&bob_owner_rx);
    drain_events(&bob_linked_rx);

    let bob_linked_self_sendable = bob_linked_mgr
        .export_active_sessions()
        .into_iter()
        .filter(|(owner, device_id, state)| {
            *owner == bob_owner_pubkey
                && device_id == &bob_owner_device_id
                && Session::new(state.clone(), "debug".to_string()).can_send()
        })
        .count();
    assert_eq!(
        bob_linked_self_sendable, 1,
        "linked device should be able to send to the owner device right after link bootstrap"
    );

    // Publish AppKeys so peers can discover linked devices.
    let alice_app_keys = AppKeys::new(vec![
        DeviceEntry::new(alice_owner_pubkey, 1),
        DeviceEntry::new(alice_new_device_pubkey, 2),
    ]);
    let alice_app_keys_event = alice_app_keys
        .get_event(alice_owner_pubkey)
        .sign_with_keys(&alice_owner_keys)?;
    bob_owner_mgr.process_received_event(alice_app_keys_event.clone());
    bob_linked_mgr.process_received_event(alice_app_keys_event);

    let bob_app_keys = AppKeys::new(vec![
        DeviceEntry::new(bob_owner_pubkey, 1),
        DeviceEntry::new(bob_linked_pubkey, 2),
    ]);
    let bob_app_keys_event = bob_app_keys
        .get_event(bob_owner_pubkey)
        .sign_with_keys(&bob_owner_keys)?;
    alice_owner_mgr.process_received_event(bob_app_keys_event);
    drain_events(&alice_owner_rx);
    drain_events(&bob_owner_rx);
    drain_events(&bob_linked_rx);

    // Alice learns Bob's linked public invite and Bob imports the inviter-side state.
    let bob_linked_public_invite_event = bob_linked_public_invite
        .get_event()?
        .sign_with_keys(&bob_linked_keys)?;
    alice_owner_mgr.process_received_event(bob_linked_public_invite_event);
    let bob_linked_public_response =
        recv_signed_event_of_kind(&alice_owner_rx, nostr_double_ratchet::INVITE_RESPONSE_KIND);
    let (bob_linked_peer, bob_linked_remote_device_id) = import_session_from_response(
        &bob_linked_public_invite,
        bob_linked_keys.secret_key().to_secret_bytes(),
        &bob_linked_mgr,
        &bob_linked_public_response,
    )?;
    assert_eq!(bob_linked_peer, alice_owner_pubkey);
    assert_eq!(bob_linked_remote_device_id, alice_owner_device_id);
    drain_events(&alice_owner_rx);
    drain_events(&bob_linked_rx);

    // Bob linked learns Alice's linked public invite and Alice linked imports the inviter-side state.
    let alice_linked_public_invite_event = alice_linked_public_invite
        .get_event()?
        .sign_with_keys(&alice_new_device_keys)?;
    bob_linked_mgr.process_received_event(alice_linked_public_invite_event);
    let alice_linked_public_response =
        recv_signed_event_of_kind(&bob_linked_rx, nostr_double_ratchet::INVITE_RESPONSE_KIND);
    let (alice_linked_peer, alice_linked_remote_device_id) = import_session_from_response(
        &alice_linked_public_invite,
        alice_new_device_keys.secret_key().to_secret_bytes(),
        &alice_new_mgr,
        &alice_linked_public_response,
    )?;
    assert_eq!(alice_linked_peer, bob_owner_pubkey);
    assert_eq!(alice_linked_remote_device_id, bob_linked_device_id);
    drain_events(&alice_new_rx);
    drain_events(&bob_linked_rx);

    // Alice sends to Bob after learning Bob's linked device. This should fan out to Bob owner,
    // Bob linked, and Alice's linked device via self-sync.
    alice_owner_mgr.send_text(bob_owner_pubkey, "seed existing chat".to_string(), None)?;
    let seed_messages = recv_message_events(&alice_owner_rx, 3);
    assert_eq!(seed_messages.len(), 3);
    for event in &seed_messages {
        alice_new_mgr.process_received_event(event.clone());
        bob_owner_mgr.process_received_event(event.clone());
        bob_linked_mgr.process_received_event(event.clone());
    }

    let alice_self_seed =
        recv_decrypted_containing(&alice_new_rx, "\"content\":\"seed existing chat\"");
    let owner_seed = recv_decrypted_containing(&bob_owner_rx, "\"content\":\"seed existing chat\"");
    let linked_seed =
        recv_decrypted_containing(&bob_linked_rx, "\"content\":\"seed existing chat\"");
    assert!(alice_self_seed.contains("\"content\":\"seed existing chat\""));
    assert!(owner_seed.contains("\"content\":\"seed existing chat\""));
    assert!(linked_seed.contains("\"content\":\"seed existing chat\""));
    drain_events(&alice_new_rx);
    drain_events(&bob_owner_rx);
    drain_events(&bob_linked_rx);

    // The linked sender should now fan out to Alice owner, Alice linked, and Bob owner.
    bob_linked_mgr.send_text(
        alice_owner_pubkey,
        "linked sender fanout to old and new device".to_string(),
        None,
    )?;
    let fanout_messages = recv_message_events(&bob_linked_rx, 3);
    assert_eq!(fanout_messages.len(), 3);

    for event in &fanout_messages {
        alice_owner_mgr.process_received_event(event.clone());
        alice_new_mgr.process_received_event(event.clone());
        bob_owner_mgr.process_received_event(event.clone());
    }

    let bob_owner_self_copy = recv_decrypted_containing(
        &bob_owner_rx,
        "\"content\":\"linked sender fanout to old and new device\"",
    );
    let owner_decrypted = recv_decrypted_containing(
        &alice_owner_rx,
        "\"content\":\"linked sender fanout to old and new device\"",
    );
    let new_device_decrypted = recv_decrypted_containing(
        &alice_new_rx,
        "\"content\":\"linked sender fanout to old and new device\"",
    );

    assert!(
        bob_owner_self_copy.contains("\"content\":\"linked sender fanout to old and new device\"")
    );
    assert!(owner_decrypted.contains("\"content\":\"linked sender fanout to old and new device\""));
    assert!(
        new_device_decrypted.contains("\"content\":\"linked sender fanout to old and new device\"")
    );

    Ok(())
}
