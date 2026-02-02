use nostr::Keys;
use nostr_double_ratchet::{Invite, Result, INVITE_EVENT_KIND, INVITE_RESPONSE_KIND};

#[test]
fn test_create_new_invite() -> Result<()> {
    let alice_keys = Keys::generate();
    let alice_pk = alice_keys.public_key();

    let invite = Invite::create_new(alice_pk, Some("Test Device".to_string()), Some(5))?;

    assert_eq!(
        hex::encode(invite.inviter_ephemeral_public_key.to_bytes()).len(),
        64
    );
    assert_eq!(hex::encode(invite.shared_secret).len(), 64);
    assert_eq!(invite.inviter.to_bytes(), alice_pk.to_bytes());
    assert_eq!(invite.device_id, Some("Test Device".to_string()));
    assert_eq!(invite.max_uses, Some(5));
    assert!(invite.inviter_ephemeral_private_key.is_some());

    Ok(())
}

#[test]
fn test_url_generation_and_parsing() -> Result<()> {
    let alice_keys = Keys::generate();
    let alice_pk = alice_keys.public_key();

    let invite = Invite::create_new(alice_pk, None, None)?;
    let url = invite.get_url("https://iris.to")?;

    assert!(url.contains("https://iris.to#"));

    let parsed_invite = Invite::from_url(&url)?;
    assert_eq!(parsed_invite.inviter.to_bytes(), invite.inviter.to_bytes());
    assert_eq!(
        parsed_invite.inviter_ephemeral_public_key.to_bytes(),
        invite.inviter_ephemeral_public_key.to_bytes()
    );
    assert_eq!(parsed_invite.shared_secret, invite.shared_secret);

    Ok(())
}

#[test]
fn test_invite_get_event_requires_device_id() -> Result<()> {
    let alice_keys = Keys::generate();
    let alice_pk = alice_keys.public_key();

    let invite = Invite::create_new(alice_pk, None, None)?;

    let result = invite.get_event();
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Device ID required"));

    Ok(())
}

#[test]
fn test_invite_event_conversion() -> Result<()> {
    let alice_keys = Keys::generate();
    let alice_pk = alice_keys.public_key();

    let invite = Invite::create_new(alice_pk, Some("test-device".to_string()), None)?;

    let unsigned_event = invite.get_event()?;

    assert_eq!(unsigned_event.kind.as_u16(), INVITE_EVENT_KIND as u16);
    assert_eq!(unsigned_event.pubkey.to_bytes(), alice_pk.to_bytes());

    let has_ephemeral_key = unsigned_event.tags.iter().any(|t| {
        let v = t.clone().to_vec();
        v.first().map(|s| s.as_str()) == Some("ephemeralKey")
    });
    assert!(has_ephemeral_key);

    let has_shared_secret = unsigned_event.tags.iter().any(|t| {
        let v = t.clone().to_vec();
        v.first().map(|s| s.as_str()) == Some("sharedSecret")
    });
    assert!(has_shared_secret);

    let has_d_tag = unsigned_event.tags.iter().any(|t| {
        let v = t.clone().to_vec();
        v.get(0).map(|s| s.as_str()) == Some("d")
            && v.get(1).map(|s| s.as_str()) == Some("double-ratchet/invites/test-device")
    });
    assert!(has_d_tag);

    let has_l_tag = unsigned_event.tags.iter().any(|t| {
        let v = t.clone().to_vec();
        v.get(0).map(|s| s.as_str()) == Some("l")
            && v.get(1).map(|s| s.as_str()) == Some("double-ratchet/invites")
    });
    assert!(has_l_tag);

    // Sign the event before parsing
    let signed_event = unsigned_event
        .sign_with_keys(&alice_keys)
        .map_err(|_e| nostr_double_ratchet::Error::Invite("Failed to sign event".to_string()))?;
    let parsed_invite = Invite::from_event(&signed_event)?;

    assert_eq!(
        parsed_invite.inviter_ephemeral_public_key.to_bytes(),
        invite.inviter_ephemeral_public_key.to_bytes()
    );
    assert_eq!(parsed_invite.shared_secret, invite.shared_secret);
    assert_eq!(parsed_invite.inviter.to_bytes(), alice_pk.to_bytes());
    assert_eq!(parsed_invite.device_id, Some("test-device".to_string()));

    Ok(())
}

#[test]
fn test_invite_accept_creates_session() -> Result<()> {
    let alice_keys = Keys::generate();
    let alice_pk = alice_keys.public_key();

    let invite = Invite::create_new(alice_pk, None, None)?;

    let bob_keys = Keys::generate();
    let bob_pk = bob_keys.public_key();
    let bob_sk = bob_keys.secret_key().to_secret_bytes();

    let (session, event) = invite.accept(bob_pk, bob_sk, Some("device-1".to_string()))?;

    assert!(session.state.sending_chain_key.is_some());
    assert_eq!(event.kind.as_u16(), INVITE_RESPONSE_KIND as u16);
    assert_ne!(event.pubkey.to_bytes(), bob_pk.to_bytes());

    let has_p_tag = event.tags.iter().any(|t| {
        let v = t.clone().to_vec();
        v.get(0).map(|s| s.as_str()) == Some("p")
            && v.get(1).map(|s| s.as_str())
                == Some(&hex::encode(invite.inviter_ephemeral_public_key.to_bytes()))
    });
    assert!(has_p_tag);

    Ok(())
}

#[test]
fn test_invite_serialization() -> Result<()> {
    let alice_keys = Keys::generate();
    let alice_pk = alice_keys.public_key();

    let invite = Invite::create_new(alice_pk, Some("device-1".to_string()), Some(10))?;

    let serialized = invite.serialize()?;
    let deserialized = Invite::deserialize(&serialized)?;

    assert_eq!(
        deserialized.inviter_ephemeral_public_key.to_bytes(),
        invite.inviter_ephemeral_public_key.to_bytes()
    );
    assert_eq!(deserialized.shared_secret, invite.shared_secret);
    assert_eq!(deserialized.inviter.to_bytes(), invite.inviter.to_bytes());
    assert_eq!(deserialized.device_id, invite.device_id);
    assert_eq!(deserialized.max_uses, invite.max_uses);

    Ok(())
}

#[test]
fn test_accept_with_device_id() -> Result<()> {
    let alice_keys = Keys::generate();
    let alice_pk = alice_keys.public_key();

    let invite = Invite::create_new(alice_pk, None, None)?;

    let bob_keys = Keys::generate();
    let bob_pk = bob_keys.public_key();
    let bob_sk = bob_keys.secret_key().to_secret_bytes();

    let (session, event) = invite.accept(bob_pk, bob_sk, Some("device-1".to_string()))?;

    assert!(session.state.sending_chain_key.is_some());
    assert_eq!(event.kind.as_u16(), INVITE_RESPONSE_KIND as u16);

    Ok(())
}

#[test]
fn test_accept_without_device_id() -> Result<()> {
    let alice_keys = Keys::generate();
    let alice_pk = alice_keys.public_key();

    let invite = Invite::create_new(alice_pk, None, None)?;

    let bob_keys = Keys::generate();
    let bob_pk = bob_keys.public_key();
    let bob_sk = bob_keys.secret_key().to_secret_bytes();

    let (session, _event) = invite.accept(bob_pk, bob_sk, None)?;

    assert!(session.state.sending_chain_key.is_some());

    Ok(())
}
