use nostr_double_ratchet::{Result, Session};
use nostr::Keys;

#[test]
fn test_session_init() -> Result<()> {
    let alice_keys = Keys::generate();
    let bob_keys = Keys::generate();

    let alice_sk = alice_keys.secret_key().to_secret_bytes();
    let bob_pk = bob_keys.public_key();

    let shared_secret = [0u8; 32];

    let alice = Session::init(
        bob_pk,
        alice_sk,
        true,
        shared_secret,
        Some("alice".to_string()),
    )?;

    assert_eq!(alice.name, "alice");
    assert!(alice.state.our_current_nostr_key.is_some());
    assert!(alice.state.sending_chain_key.is_some());
    assert_eq!(alice.state.sending_chain_message_number, 0);

    Ok(())
}

#[test]
fn test_session_init_responder() -> Result<()> {
    let alice_keys = Keys::generate();
    let bob_keys = Keys::generate();

    let bob_sk = bob_keys.secret_key().to_secret_bytes();
    let alice_pk = alice_keys.public_key();

    let shared_secret = [0u8; 32];

    let bob = Session::init(
        alice_pk,
        bob_sk,
        false,
        shared_secret,
        Some("bob".to_string()),
    )?;

    assert_eq!(bob.name, "bob");
    assert!(bob.state.our_current_nostr_key.is_none());
    assert!(bob.state.sending_chain_key.is_none());
    assert_eq!(bob.state.sending_chain_message_number, 0);

    Ok(())
}

#[test]
fn test_session_send_message() -> Result<()> {
    let alice_keys = Keys::generate();
    let bob_keys = Keys::generate();

    let alice_sk = alice_keys.secret_key().to_secret_bytes();
    let bob_pk = bob_keys.public_key();

    let shared_secret = [0u8; 32];

    let mut alice = Session::init(
        bob_pk,
        alice_sk,
        true,
        shared_secret,
        Some("alice".to_string()),
    )?;

    let event = alice.send("Hello, Bob!".to_string())?;

    assert_eq!(event.kind.as_u16(), 1060);
    assert!(!event.content.is_empty());
    assert!(event.tags.len() > 0);

    // Check for header tag
    let has_header = event.tags.iter().any(|t| {
        let v = t.clone().to_vec();
        v.first().map(|s| s.as_str()) == Some("header")
    });
    assert!(has_header);

    Ok(())
}

#[test]
fn test_multiple_messages() -> Result<()> {
    let alice_keys = Keys::generate();
    let bob_keys = Keys::generate();

    let alice_sk = alice_keys.secret_key().to_secret_bytes();
    let bob_pk = bob_keys.public_key();

    let shared_secret = [0u8; 32];

    let mut alice = Session::init(
        bob_pk,
        alice_sk,
        true,
        shared_secret,
        Some("alice".to_string()),
    )?;

    let initial_chain_number = alice.state.sending_chain_message_number;

    let _event1 = alice.send("Message 1".to_string())?;
    assert_eq!(alice.state.sending_chain_message_number, initial_chain_number + 1);

    let _event2 = alice.send("Message 2".to_string())?;
    assert_eq!(alice.state.sending_chain_message_number, initial_chain_number + 2);

    let _event3 = alice.send("Message 3".to_string())?;
    assert_eq!(alice.state.sending_chain_message_number, initial_chain_number + 3);

    Ok(())
}

#[test]
fn test_session_state_serialization() -> Result<()> {
    use nostr_double_ratchet::utils::{serialize_session_state, deserialize_session_state};

    let alice_keys = Keys::generate();
    let bob_keys = Keys::generate();

    let alice_sk = alice_keys.secret_key().to_secret_bytes();
    let bob_pk = bob_keys.public_key();

    let shared_secret = [0u8; 32];

    let alice = Session::init(
        bob_pk,
        alice_sk,
        true,
        shared_secret,
        Some("alice".to_string()),
    )?;

    // Serialize
    let serialized = serialize_session_state(&alice.state)?;
    assert!(!serialized.is_empty());

    // Deserialize
    let deserialized = deserialize_session_state(&serialized)?;

    // Verify key fields match
    assert_eq!(alice.state.root_key, deserialized.root_key);
    assert_eq!(alice.state.sending_chain_message_number, deserialized.sending_chain_message_number);
    assert_eq!(alice.state.receiving_chain_message_number, deserialized.receiving_chain_message_number);

    Ok(())
}
