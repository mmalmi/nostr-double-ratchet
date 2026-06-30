use nostr::Keys;
use nostr_double_ratchet::{classify_message_origin, MessageOrigin};

#[test]
fn classifies_local_device_origin() {
    let our_owner = Keys::generate().public_key();
    let our_device = Keys::generate().public_key();

    let origin = classify_message_origin(
        our_owner,
        Some(our_device),
        Some(our_owner),
        Some(our_device),
    );
    assert_eq!(origin, MessageOrigin::LocalDevice);
    assert!(origin.is_self());
    assert!(!origin.is_cross_device_self());
}

#[test]
fn classifies_same_owner_other_device_origin() {
    let our_owner = Keys::generate().public_key();
    let our_device = Keys::generate().public_key();
    let other_device = Keys::generate().public_key();

    let origin = classify_message_origin(
        our_owner,
        Some(our_device),
        Some(our_owner),
        Some(other_device),
    );
    assert_eq!(origin, MessageOrigin::SameOwnerOtherDevice);
    assert!(origin.is_self());
    assert!(origin.is_cross_device_self());
}

#[test]
fn classifies_remote_owner_origin() {
    let our_owner = Keys::generate().public_key();
    let our_device = Keys::generate().public_key();
    let remote_owner = Keys::generate().public_key();
    let remote_device = Keys::generate().public_key();

    let origin = classify_message_origin(
        our_owner,
        Some(our_device),
        Some(remote_owner),
        Some(remote_device),
    );
    assert_eq!(origin, MessageOrigin::RemoteOwner);
    assert!(!origin.is_self());
    assert!(!origin.is_cross_device_self());
}

#[test]
fn classifies_unknown_when_provenance_is_incomplete() {
    let our_owner = Keys::generate().public_key();
    let our_device = Keys::generate().public_key();

    let origin = classify_message_origin(our_owner, Some(our_device), Some(our_owner), None);
    assert_eq!(origin, MessageOrigin::Unknown);
    assert!(!origin.is_self());
    assert!(!origin.is_cross_device_self());
}
