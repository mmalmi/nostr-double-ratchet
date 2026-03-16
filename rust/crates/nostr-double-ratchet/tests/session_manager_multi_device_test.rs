use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Receiver;
use nostr::{JsonUtil, Keys, UnsignedEvent};
use nostr_double_ratchet::{
    AppKeys, DeviceEntry, InMemoryStorage, Invite, Result, SendOptions, SessionManager,
    SessionManagerEvent, MESSAGE_EVENT_KIND,
};

fn recv_signed_event(rx: &Receiver<SessionManagerEvent>) -> nostr::Event {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(2) {
            panic!("Timed out waiting for PublishSigned event");
        }
        if let Ok(SessionManagerEvent::PublishSigned(signed)) =
            rx.recv_timeout(Duration::from_millis(200))
        {
            return signed;
        }
    }
}

fn recv_signed_event_of_kind(
    rx: &Receiver<SessionManagerEvent>,
    kind: u32,
) -> nostr::Event {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(2) {
            panic!("Timed out waiting for PublishSigned event of kind {kind}");
        }
        if let Ok(SessionManagerEvent::PublishSigned(signed)) =
            rx.recv_timeout(Duration::from_millis(200))
        {
            if signed.kind.as_u16() == kind as u16 {
                return signed;
            }
        }
    }
}

fn recv_message_events(rx: &Receiver<SessionManagerEvent>, expected: usize) -> Vec<nostr::Event> {
    let start = std::time::Instant::now();
    let mut events = Vec::new();

    while start.elapsed() <= Duration::from_secs(2) {
        if let Ok(SessionManagerEvent::PublishSigned(signed)) =
            rx.recv_timeout(Duration::from_millis(100))
        {
            if signed.kind.as_u16() == MESSAGE_EVENT_KIND as u16 {
                events.push(signed);
                if events.len() >= expected {
                    return events;
                }
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
    while start.elapsed() <= Duration::from_secs(2) {
        if let Ok(SessionManagerEvent::DecryptedMessage { content, .. }) =
            rx.recv_timeout(Duration::from_millis(100))
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

#[test]
fn test_accept_invite_routes_session_under_claimed_owner() -> Result<()> {
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

    let accepted = manager.accept_invite(&invite, Some(bob_owner))?;
    assert_eq!(accepted.owner_pubkey, bob_owner);
    assert_eq!(accepted.device_id, bob_device_id);

    let mut saw_response = false;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        if let Ok(SessionManagerEvent::PublishSigned(signed)) =
            rx.recv_timeout(Duration::from_millis(100))
        {
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
    let new_device_invite =
        Invite::create_new(alice_new_device_pubkey, Some(alice_new_device_id.clone()), None)?;

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
