use nostr_double_ratchet::{utils::{serialize_session_state, deserialize_session_state}, Result, Session};
use nostr::Keys;

#[test]
fn test_discard_duplicate_messages_after_restoring() -> Result<()> {
    let alice_keys = Keys::generate();
    let bob_keys = Keys::generate();

    let alice_sk = alice_keys.secret_key().to_secret_bytes();
    let bob_sk = bob_keys.secret_key().to_secret_bytes();

    let alice_pk = alice_keys.public_key();
    let bob_pk = bob_keys.public_key();

    let shared_secret = [0u8; 32];

    let mut alice = Session::init(bob_pk, alice_sk, true, shared_secret, Some("alice".to_string()))?;
    let mut bob = Session::init(alice_pk, bob_sk, false, shared_secret, Some("bob".to_string()))?;

    let mut sent_events = Vec::new();
    let messages = vec!["Message 1", "Message 2", "Message 3"];

    for message in &messages {
        let event = alice.send(message.to_string())?;
        sent_events.push(event.clone());
        let received = bob.receive(&event)?;
        assert!(received.is_some());
        let rumor: serde_json::Value = serde_json::from_str(&received.unwrap())?;
        assert_eq!(rumor["content"].as_str(), Some(*message));
    }

    let serialized_bob = serialize_session_state(&bob.state)?;
    let bob_restored = Session::new(
        deserialize_session_state(&serialized_bob)?,
        "bob_restored".to_string()
    );

    let initial_receiving_count = bob_restored.state.receiving_chain_message_number;

    for _event in &sent_events {
        let result = bob_restored.state.clone();
        assert_eq!(result.receiving_chain_message_number, initial_receiving_count);
    }

    let fresh_event = alice.send("Fresh message after duplicates".to_string())?;
    let mut bob_restored_mut = bob_restored;
    let received = bob_restored_mut.receive(&fresh_event)?;
    assert!(received.is_some());

    let rumor: serde_json::Value = serde_json::from_str(&received.unwrap())?;
    assert_eq!(rumor["content"].as_str(), Some("Fresh message after duplicates"));

    Ok(())
}

#[test]
fn test_session_reinitialization() -> Result<()> {
    let alice_keys = Keys::generate();
    let bob_keys = Keys::generate();

    let alice_sk = alice_keys.secret_key().to_secret_bytes();
    let bob_sk = bob_keys.secret_key().to_secret_bytes();

    let alice_pk = alice_keys.public_key();
    let bob_pk = bob_keys.public_key();

    let shared_secret = [0u8; 32];

    let mut alice = Session::init(bob_pk, alice_sk, true, shared_secret, Some("alice".to_string()))?;
    let mut bob = Session::init(alice_pk, bob_sk, false, shared_secret, Some("bob".to_string()))?;

    let msg1 = alice.send("Message 1".to_string())?;
    let received1 = bob.receive(&msg1)?;
    assert!(received1.is_some());
    let rumor1: serde_json::Value = serde_json::from_str(&received1.unwrap())?;
    assert_eq!(rumor1["content"].as_str(), Some("Message 1"));

    let serialized_bob_state = serialize_session_state(&bob.state)?;

    let mut bob_restored = Session::new(
        deserialize_session_state(&serialized_bob_state)?,
        "bob_restored".to_string()
    );

    let msg2 = alice.send("Message 2".to_string())?;
    let received2 = bob_restored.receive(&msg2)?;
    assert!(received2.is_some());
    let rumor2: serde_json::Value = serde_json::from_str(&received2.unwrap())?;
    assert_eq!(rumor2["content"].as_str(), Some("Message 2"));

    Ok(())
}
