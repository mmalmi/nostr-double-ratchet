use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use crossbeam_channel::Receiver;
use nostr::nips::nip44::{self, Version};
use nostr::{EventBuilder, Keys, Kind, Tag, Timestamp, UnsignedEvent};
use nostr_double_ratchet::{
    utils::kdf, Header, InMemoryStorage, Invite, Session, SessionManager, SessionManagerEvent,
    MESSAGE_EVENT_KIND,
};
use sha2::{Digest, Sha256};

fn drain_events(rx: &Receiver<SessionManagerEvent>) {
    while rx.try_recv().is_ok() {}
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

fn build_bad_id_rumor_json(pubkey: nostr::PublicKey) -> String {
    let mut rumor: UnsignedEvent = EventBuilder::new(Kind::from(14u16), "hello")
        .custom_created_at(Timestamp::from(1))
        .build(pubkey);

    // Compute a valid id, then mutate content without recomputing.
    rumor.ensure_id();
    assert!(rumor.id.is_some());
    rumor.content = "tampered".to_string();

    serde_json::to_string(&rumor).unwrap()
}

fn compute_event_hash(rumor: &serde_json::Value) -> String {
    let pubkey = rumor["pubkey"].as_str().expect("pubkey string");
    let created_at = rumor["created_at"].as_u64().expect("created_at u64");
    let kind = rumor["kind"].as_u64().expect("kind u64");
    let content = rumor["content"].as_str().expect("content string");

    let tags_value = rumor["tags"].as_array().expect("tags array");
    let mut tags: Vec<Vec<String>> = Vec::with_capacity(tags_value.len());
    for tag in tags_value {
        let arr = tag.as_array().expect("tag array");
        let mut out: Vec<String> = Vec::with_capacity(arr.len());
        for v in arr {
            out.push(v.as_str().expect("tag elem string").to_string());
        }
        tags.push(out);
    }

    let canonical = serde_json::json!([0, pubkey, created_at, kind, tags, content]);
    let canonical_json = serde_json::to_string(&canonical).expect("canonical to_string");
    hex::encode(Sha256::digest(canonical_json.as_bytes()))
}

#[test]
fn test_session_receive_recomputes_inner_rumor_id_and_stays_in_sync() {
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

    let bad_inner = build_bad_id_rumor_json(bob_keys.public_key());
    let outer = craft_outer_from_plaintext(&mut bob_session, &bad_inner, 10);

    let plaintext = alice_session.receive(&outer).unwrap().unwrap();
    let rumor: serde_json::Value = serde_json::from_str(&plaintext).unwrap();
    assert_eq!(rumor["content"].as_str().unwrap(), "tampered");

    // Receiver ignores sender-provided `id` and recomputes it locally.
    let expected_id = compute_event_hash(&rumor);
    assert_eq!(rumor["id"].as_str().unwrap(), expected_id);

    // Ratchet state should still be aligned.
    assert_eq!(
        bob_session.state.sending_chain_key.unwrap(),
        alice_session.state.receiving_chain_key.unwrap(),
        "chain keys desynced after receiving message with bad id"
    );

    // Replaying the same outer event should fail (message key already consumed) and must not corrupt
    // state (receive rolls back on decryption errors).
    assert!(alice_session.receive(&outer).is_err());

    // Next valid message should still decrypt.
    let valid_outer = bob_session.send("ok".to_string()).unwrap();
    let decrypted = alice_session.receive(&valid_outer).unwrap().unwrap();
    assert!(decrypted.contains("\"content\":\"ok\""));
}

#[test]
fn test_session_manager_delivers_messages_with_recomputed_inner_id() {
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

    let bad_inner = build_bad_id_rumor_json(bob_keys.public_key());
    let bad_outer = craft_outer_from_plaintext(&mut bob_session, &bad_inner, 10);
    manager.process_received_event(bad_outer);

    let delivered = recv_decrypted_message(&rx);
    let rumor: serde_json::Value = serde_json::from_str(&delivered).unwrap();
    assert_eq!(rumor["content"].as_str().unwrap(), "tampered");

    let expected_id = compute_event_hash(&rumor);
    assert_eq!(rumor["id"].as_str().unwrap(), expected_id);
}
