use nostr::Keys;
use nostr_double_ratchet::{Invite, Result, SessionManagerEvent};

#[test]
fn test_invite_listen_and_accept() -> Result<()> {
    let alice_keys = Keys::generate();
    let alice_pk = alice_keys.public_key();
    let alice_sk = alice_keys.secret_key().to_secret_bytes();

    let invite = Invite::create_new(alice_pk, Some("alice-device".to_string()), None)?;

    let bob_keys = Keys::generate();
    let bob_pk = bob_keys.public_key();
    let bob_sk = bob_keys.secret_key().to_secret_bytes();

    let (_bob_session, acceptance_event) =
        invite.accept(bob_pk, bob_sk, Some("bob-device".to_string()))?;

    // Create event channel for listen()
    let (event_tx, _event_rx) = crossbeam_channel::unbounded::<SessionManagerEvent>();

    // Simulate receiving the acceptance event
    // In real usage, this would be handled by the relay/subscription system
    // For this test, we'll directly process it
    invite.listen(&event_tx)?;

    // Since we can't mock the subscription system easily, we'll directly test
    // invite response processing via process_invite_response
    if let Some((alice_session, identity, device_id)) =
        invite.process_invite_response(&acceptance_event, alice_sk)?
    {
        assert_eq!(identity.to_bytes(), bob_pk.to_bytes());
        assert_eq!(device_id, Some("bob-device".to_string()));
        assert!(alice_session.state.receiving_chain_key.is_none());
        assert!(alice_session.state.sending_chain_key.is_none());
    } else {
        panic!("Expected invite response to be processed successfully");
    }

    Ok(())
}

#[test]
fn test_from_user_subscription() -> Result<()> {
    let alice_keys = Keys::generate();
    let alice_pk = alice_keys.public_key();

    let invite = Invite::create_new(alice_pk, Some("device-1".to_string()), None)?;
    let unsigned_event = invite.get_event()?;

    // Sign the event
    let signed_event = unsigned_event
        .sign_with_keys(&alice_keys)
        .map_err(|_e| nostr_double_ratchet::Error::Invite("Failed to sign event".to_string()))?;

    // Create event channel for from_user()
    let (event_tx, _event_rx) = crossbeam_channel::unbounded::<SessionManagerEvent>();

    Invite::from_user(alice_pk, &event_tx)?;

    // Test that we can parse the invite from the signed event
    let parsed_invite = Invite::from_event(&signed_event)?;
    assert_eq!(parsed_invite.inviter.to_bytes(), alice_pk.to_bytes());
    assert_eq!(parsed_invite.device_id, Some("device-1".to_string()));

    Ok(())
}

#[test]
fn test_listen_without_device_id() -> Result<()> {
    let alice_keys = Keys::generate();
    let alice_pk = alice_keys.public_key();
    let alice_sk = alice_keys.secret_key().to_secret_bytes();

    let invite = Invite::create_new(alice_pk, Some("alice-device".to_string()), None)?;

    let bob_keys = Keys::generate();
    let bob_pk = bob_keys.public_key();
    let bob_sk = bob_keys.secret_key().to_secret_bytes();

    let (_bob_session, acceptance_event) = invite.accept(bob_pk, bob_sk, None)?;

    // Create event channel for listen()
    let (event_tx, _event_rx) = crossbeam_channel::unbounded::<SessionManagerEvent>();

    invite.listen(&event_tx)?;

    // Directly process the invite response
    if let Some((_alice_session, identity, device_id)) =
        invite.process_invite_response(&acceptance_event, alice_sk)?
    {
        assert_eq!(identity.to_bytes(), bob_pk.to_bytes());
        assert_eq!(device_id, None);
    } else {
        panic!("Expected invite response to be processed successfully");
    }

    Ok(())
}
