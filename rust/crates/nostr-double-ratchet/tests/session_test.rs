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

#[test]
fn test_send_reaction_format() {
    // Create a test session
    let alice_keys = nostr::Keys::generate();
    let bob_keys = nostr::Keys::generate();

    let invite = nostr_double_ratchet::Invite::create_new(
        alice_keys.public_key(),
        None,
        None,
    ).unwrap();

    // Bob accepts - after accept, BOB can send first (he's the ratchet initiator)
    let (mut bob_session, _response) = invite.accept(
        bob_keys.public_key(),
        bob_keys.secret_key().to_secret_bytes(),
        None,
    ).unwrap();

    // Send a reaction (from Bob, who can send)
    let message_id = "test-message-id-12345";
    let emoji = "👍";
    
    let reaction_event = bob_session.send_reaction(message_id, emoji).unwrap();
    
    // The outer event should be kind 1060 (encrypted wrapper)
    assert_eq!(reaction_event.kind.as_u16(), 1060);
    
    // Verify the event can be decrypted by creating a receiving session
    // (This is complex, so we'll just verify the format for now)
    
    // The reaction should have been encrypted, so we can't directly inspect inner content
    // But we can verify the event was created successfully
    assert!(!reaction_event.id.to_hex().is_empty());
    assert!(!reaction_event.sig.to_string().is_empty());
    
    println!("Reaction event created successfully: {}", reaction_event.id.to_hex());
}

#[test]
fn test_reaction_roundtrip() {
    // Create Alice and Bob
    let alice_keys = nostr::Keys::generate();
    let bob_keys = nostr::Keys::generate();

    let invite = nostr_double_ratchet::Invite::create_new(
        alice_keys.public_key(),
        None,
        None,
    ).unwrap();

    // Bob accepts the invite - after accept, BOB can send first!
    let (mut bob_session, response) = invite.accept(
        bob_keys.public_key(),
        bob_keys.secret_key().to_secret_bytes(),
        None,
    ).unwrap();

    // Alice processes the response - Alice needs to receive first
    let (mut alice_session, _, _) = invite.process_invite_response(
        &response,
        alice_keys.secret_key().to_secret_bytes(),
    ).unwrap().unwrap();

    // Bob sends the first message (he's the ratchet initiator after accept)
    let msg_event = bob_session.send("Hello Alice!".to_string()).unwrap();
    let msg_id = msg_event.id.to_hex();
    
    // Alice receives the message - now Alice can send
    let decrypted = alice_session.receive(&msg_event).unwrap().unwrap();
    let rumor: serde_json::Value = serde_json::from_str(&decrypted).unwrap();
    assert_eq!(rumor["content"].as_str().unwrap(), "Hello Alice!");
    
    // Now Alice sends a reaction to Bob's message
    let reaction_event = alice_session.send_reaction(&msg_id, "❤️").unwrap();
    
    // Bob receives the reaction
    let decrypted_reaction = bob_session.receive(&reaction_event).unwrap().unwrap();
    let reaction_rumor: serde_json::Value = serde_json::from_str(&decrypted_reaction).unwrap();
    
    // Verify the inner event is a reaction (kind 7)
    assert_eq!(reaction_rumor["kind"].as_u64().unwrap(), 7);
    
    // Verify the content is the reaction payload
    let content: serde_json::Value = serde_json::from_str(
        reaction_rumor["content"].as_str().unwrap()
    ).unwrap();
    assert_eq!(content["type"].as_str().unwrap(), "reaction");
    assert_eq!(content["messageId"].as_str().unwrap(), msg_id);
    assert_eq!(content["emoji"].as_str().unwrap(), "❤️");
    
    // Verify the e tag
    let tags = reaction_rumor["tags"].as_array().unwrap();
    let e_tag = tags.iter().find(|t| t[0].as_str() == Some("e")).unwrap();
    assert_eq!(e_tag[1].as_str().unwrap(), msg_id);
    
    println!("Reaction roundtrip test passed!");
}
