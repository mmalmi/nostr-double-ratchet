use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use crossbeam_channel::Receiver;
use nostr::nips::nip44::{self, Version};
use nostr::{EventBuilder, Keys, Kind, Tag, Timestamp, UnsignedEvent};
use nostr_double_ratchet::{
    utils::kdf, Error, Header, InMemoryStorage, Invite, Session, SessionManager,
    SessionManagerEvent, MESSAGE_EVENT_KIND,
};

fn drain_events(rx: &Receiver<SessionManagerEvent>) {
    while rx.try_recv().is_ok() {}
}

fn recv_invalid_rumor(rx: &Receiver<SessionManagerEvent>) -> (String, String) {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(2) {
            panic!("Timed out waiting for InvalidRumor event");
        }
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(200)) {
            if let SessionManagerEvent::InvalidRumor {
                reason, content, ..
            } = ev
            {
                return (reason, content);
            }
        }
    }
}

fn recv_decrypted_message(rx: &Receiver<SessionManagerEvent>) -> String {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(2) {
            panic!("Timed out waiting for DecryptedMessage event");
        }
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(200)) {
            if let SessionManagerEvent::DecryptedMessage { content, .. } = ev {
                return content;
            }
        }
    }
}

/// Craft an outer kind-1060 message event from an arbitrary inner rumor JSON.
///
/// This intentionally mirrors `Session::send_event` but lets us inject malformed rumors.
fn craft_outer_from_plaintext(session: &mut Session, plaintext: &str, now_s: u64) -> nostr::Event {
    let sending_chain_key = session
        .state
        .sending_chain_key
        .expect("expected initiator session with sending_chain_key");

    let kdf_outputs = kdf(&sending_chain_key, &[1u8], 2);
    session.state.sending_chain_key = Some(kdf_outputs[0]);
    let message_key = kdf_outputs[1];

    let header = Header {
        number: session.state.sending_chain_message_number,
        next_public_key: hex::encode(session.state.our_next_nostr_key.public_key.to_bytes()),
        previous_chain_length: session.state.previous_sending_chain_message_count,
    };
    session.state.sending_chain_message_number += 1;

    let conversation_key = nip44::v2::ConversationKey::new(message_key);
    let encrypted_bytes =
        nip44::v2::encrypt_to_bytes(&conversation_key, plaintext).expect("encrypt_to_bytes failed");
    let encrypted_data = base64::engine::general_purpose::STANDARD.encode(encrypted_bytes);

    let our_current = session
        .state
        .our_current_nostr_key
        .as_ref()
        .expect("expected initiator session with our_current_nostr_key");
    let their_pk = session
        .state
        .their_next_nostr_public_key
        .expect("expected initiator session with their_next_nostr_public_key");

    let our_sk = nostr::SecretKey::from_slice(&our_current.private_key).unwrap();

    let encrypted_header = nip44::encrypt(
        &our_sk,
        &their_pk,
        &serde_json::to_string(&header).unwrap(),
        Version::V2,
    )
    .unwrap();

    let tags = vec![Tag::parse(&["header".to_string(), encrypted_header]).unwrap()];

    let author_pubkey = our_current.public_key;

    let unsigned_event = EventBuilder::new(Kind::from(MESSAGE_EVENT_KIND as u16), encrypted_data)
        .tags(tags)
        .custom_created_at(Timestamp::from(now_s))
        .build(author_pubkey);

    let author_keys = Keys::new(our_sk);
    unsigned_event.sign_with_keys(&author_keys).unwrap()
}

fn build_invalid_rumor_json(pubkey: nostr::PublicKey) -> String {
    let mut rumor: UnsignedEvent = EventBuilder::new(Kind::from(14u16), "hello")
        .custom_created_at(Timestamp::from(1))
        .build(pubkey);

    // Compute a valid id, then mutate content without recomputing.
    rumor.ensure_id();
    assert!(rumor.id.is_some());
    rumor.content = "tampered".to_string();

    serde_json::to_string(&rumor).unwrap()
}

#[test]
fn test_session_receive_invalid_rumor_id_is_reported_and_consumed() {
    let alice_keys = Keys::generate();
    let bob_keys = Keys::generate();

    let invite = Invite::create_new(alice_keys.public_key(), None, None).unwrap();

    // Bob accepts: Bob becomes the initiator (can send first).
    let (mut bob_session, response) = invite
        .accept(
            bob_keys.public_key(),
            bob_keys.secret_key().to_secret_bytes(),
            None,
        )
        .unwrap();

    // Alice processes response: Alice must receive first.
    let mut alice_session = invite
        .process_invite_response(&response, alice_keys.secret_key().to_secret_bytes())
        .unwrap()
        .unwrap()
        .session;

    let invalid_inner = build_invalid_rumor_json(bob_keys.public_key());
    let outer = craft_outer_from_plaintext(&mut bob_session, &invalid_inner, 10);

    let err = alice_session
        .receive(&outer)
        .expect_err("expected invalid rumor to error");
    match err {
        Error::InvalidRumor { reason, plaintext } => {
            assert!(reason.contains("id"));
            assert!(plaintext.contains("tampered"));
        }
        other => panic!("unexpected error: {other:?}"),
    }

    // Even though the rumor was invalid, the ratchet state should have advanced consistently.
    assert_eq!(
        bob_session.state.sending_chain_key.unwrap(),
        alice_session.state.receiving_chain_key.unwrap(),
        "chain keys desynced after invalid rumor"
    );

    // Header encryption/decryption should still be aligned (identity keys didn't change).
    let inviter_ephemeral_sk =
        nostr::SecretKey::from_slice(&invite.inviter_ephemeral_private_key.unwrap()).unwrap();
    let inviter_ephemeral_pk = nostr::Keys::new(inviter_ephemeral_sk).public_key();
    assert_eq!(
        bob_session.state.their_next_nostr_public_key.unwrap(),
        inviter_ephemeral_pk
    );
    assert_eq!(
        alice_session
            .state
            .our_current_nostr_key
            .as_ref()
            .unwrap()
            .public_key,
        inviter_ephemeral_pk
    );

    // Replaying the same outer event should NOT decrypt again (message key already consumed).
    assert!(alice_session.receive(&outer).is_err());

    // Next valid message should still decrypt (ratchet state advanced).
    let valid_outer = bob_session.send("ok".to_string()).unwrap();
    let decrypted = alice_session.receive(&valid_outer).unwrap().unwrap();
    assert!(decrypted.contains("\"content\":\"ok\""));
}

#[test]
fn test_session_manager_emits_invalid_rumor_event() {
    let alice_keys = Keys::generate();
    let bob_keys = Keys::generate();

    let invite = Invite::create_new(alice_keys.public_key(), None, None).unwrap();

    // Bob initiator session
    let (mut bob_session, response) = invite
        .accept(
            bob_keys.public_key(),
            bob_keys.secret_key().to_secret_bytes(),
            None,
        )
        .unwrap();

    // Alice responder session state (to import into manager)
    let alice_session = invite
        .process_invite_response(&response, alice_keys.secret_key().to_secret_bytes())
        .unwrap()
        .unwrap()
        .session;

    let (tx, rx) = crossbeam_channel::unbounded::<SessionManagerEvent>();
    let manager = SessionManager::new(
        alice_keys.public_key(),
        alice_keys.secret_key().to_secret_bytes(),
        hex::encode(alice_keys.public_key().to_bytes()),
        alice_keys.public_key(),
        tx,
        Some(Arc::new(InMemoryStorage::new()) as Arc<dyn nostr_double_ratchet::StorageAdapter>),
        None,
    );

    manager.init().unwrap();

    // Import session with Bob
    manager
        .import_session_state(
            bob_keys.public_key(),
            Some(hex::encode(bob_keys.public_key().to_bytes())),
            alice_session.state.clone(),
        )
        .unwrap();

    drain_events(&rx);

    // Send an invalid inner rumor
    let invalid_inner = build_invalid_rumor_json(bob_keys.public_key());
    let invalid_outer = craft_outer_from_plaintext(&mut bob_session, &invalid_inner, 10);
    manager.process_received_event(invalid_outer);

    let (reason, plaintext) = recv_invalid_rumor(&rx);
    assert!(reason.contains("id"));
    assert!(plaintext.contains("tampered"));

    // Now send a valid message and ensure it decrypts.
    let valid_outer = bob_session.send("ok".to_string()).unwrap();
    manager.process_received_event(valid_outer);

    let decrypted = recv_decrypted_message(&rx);
    assert!(decrypted.contains("\"content\":\"ok\""));
}
