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
        if let Ok(event) = rx.recv_timeout(Duration::from_millis(200)) {
            if let SessionManagerEvent::PublishSigned(signed) = event {
                return signed;
            }
        }
    }
}

fn drain_events(rx: &Receiver<SessionManagerEvent>) {
    while rx.try_recv().is_ok() {}
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
        if let Ok(event) = rx2.recv_timeout(Duration::from_millis(50)) {
            if let SessionManagerEvent::DecryptedMessage { content, .. } = event {
                decrypted = Some(content);
                break;
            }
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
        if let Ok(event) = rx2.recv_timeout(Duration::from_millis(50)) {
            if let SessionManagerEvent::DecryptedMessage { content, .. } = event {
                decrypted = Some(content);
                break;
            }
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
