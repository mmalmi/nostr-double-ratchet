use nostr::Keys;
use nostr_double_ratchet::{invite_response_event, AppKeys, DeviceEntry, InviteNostrExt};
use nostr_double_ratchet::{Invite, Result};

#[test]
fn owner_claim_verification_requires_app_keys_for_multi_device() -> Result<()> {
    let alice_keys = Keys::generate();
    let alice_pk = alice_keys.public_key();
    let alice_sk = alice_keys.secret_key().to_secret_bytes();
    let invite = Invite::create_new(alice_pk, Some("alice-device".to_string()), None)?;

    let device_keys = Keys::generate();
    let device_pk = device_keys.public_key();
    let device_sk = device_keys.secret_key().to_secret_bytes();

    let owner_keys = Keys::generate();
    let owner_pk = owner_keys.public_key();

    let (_session, response_envelope) = invite.accept_with_owner(
        device_pk,
        device_sk,
        Some("device-1".to_string()),
        Some(owner_pk),
    )?;
    let response_event = invite_response_event(&response_envelope)?;
    let response = invite
        .process_invite_response(&response_event, alice_sk)?
        .expect("expected invite response");

    assert_eq!(
        response.resolved_owner_pubkey().to_bytes(),
        owner_pk.to_bytes()
    );
    assert!(!response.has_verified_owner_claim(None));

    let app_keys = AppKeys::new(vec![DeviceEntry::new(device_pk, 1)]);
    assert!(response.has_verified_owner_claim(Some(&app_keys)));

    Ok(())
}

#[test]
fn owner_claim_verification_allows_single_device_without_app_keys() -> Result<()> {
    let alice_keys = Keys::generate();
    let alice_pk = alice_keys.public_key();
    let alice_sk = alice_keys.secret_key().to_secret_bytes();
    let invite = Invite::create_new(alice_pk, Some("alice-device".to_string()), None)?;

    let device_keys = Keys::generate();
    let device_pk = device_keys.public_key();
    let device_sk = device_keys.secret_key().to_secret_bytes();

    let (_session, response_envelope) =
        invite.accept_with_owner(device_pk, device_sk, None, Some(device_pk))?;
    let response_event = invite_response_event(&response_envelope)?;
    let response = invite
        .process_invite_response(&response_event, alice_sk)?
        .expect("expected invite response");

    assert_eq!(
        response.resolved_owner_pubkey().to_bytes(),
        response.invitee_identity.to_bytes()
    );
    assert!(response.has_verified_owner_claim(None));

    Ok(())
}
