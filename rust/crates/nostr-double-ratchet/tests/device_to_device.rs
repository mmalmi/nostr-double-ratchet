use nostr::{EventBuilder, Keys, PublicKey, SecretKey, Timestamp, UnsignedEvent};
use nostr_double_ratchet::{
    Invite, InviteAcceptInput, InviteCreateInput, InviteProcessResponseInput,
    InviteProcessResponseResult, SessionReceiveInput, SessionReceiveResult, SessionSendInput,
    MESSAGE_EVENT_KIND,
};

fn keypair(byte: u8) -> (SecretKey, PublicKey) {
    let bytes = [byte; 32];
    let sk = SecretKey::from_slice(&bytes).unwrap();
    let pk = Keys::new(sk.clone()).public_key();
    (sk, pk)
}

fn key_bytes(byte: u8) -> [u8; 32] {
    [byte; 32]
}

fn inner_event(author: PublicKey, kind: u16, content: &str, created_at: u64) -> UnsignedEvent {
    let mut event = EventBuilder::new(nostr::Kind::Custom(kind), content)
        .custom_created_at(Timestamp::from(created_at))
        .build(author);
    event.ensure_id();
    event
}

#[test]
fn device_to_device_handshake_establishes_compatible_sessions() {
    let (alice_identity_sk, alice_identity_pk) = keypair(100);
    let (_, bob_identity_pk) = keypair(101);

    let invite = Invite::create(InviteCreateInput {
        inviter: alice_identity_pk,
        inviter_ephemeral_private_key: key_bytes(102),
        shared_secret: key_bytes(103),
        created_at: 1_700_000_000,
        device_id: Some("alice-device".to_string()),
        max_uses: None,
        purpose: Some("chat".to_string()),
        owner_public_key: Some(alice_identity_pk),
    })
    .unwrap();

    let accepted = invite
        .accept(InviteAcceptInput {
            invitee_public_key: bob_identity_pk,
            invitee_identity_private_key: key_bytes(101),
            invitee_session_private_key: key_bytes(104),
            invitee_next_nostr_private_key: key_bytes(105),
            envelope_sender_private_key: key_bytes(106),
            response_created_at: 1_700_000_010,
            device_id: Some("bob-device".to_string()),
            owner_public_key: Some(bob_identity_pk),
        })
        .unwrap();

    let bob_session = accepted.session.clone();
    let bob_current_pubkey = bob_session.state.our_current_nostr_key.as_ref().unwrap().public_key;

    assert!(bob_session.can_send());
    assert_eq!(
        bob_session.state.their_next_nostr_public_key,
        Some(invite.inviter_ephemeral_public_key)
    );
    assert!(accepted.next_invite.used_by.contains(&bob_identity_pk));

    let processed = invite.process_response(InviteProcessResponseInput {
        event: accepted.response_event,
        inviter_identity_private_key: alice_identity_sk.to_secret_bytes(),
        inviter_next_nostr_private_key: key_bytes(107),
    });

    match processed {
        InviteProcessResponseResult::Accepted {
            next_invite,
            session: alice_session,
            meta,
        } => {
            assert_eq!(meta.invitee_identity, bob_identity_pk);
            assert_eq!(meta.device_id.as_deref(), Some("bob-device"));
            assert_eq!(meta.owner_public_key, Some(bob_identity_pk));
            assert!(!alice_session.can_send());
            assert_eq!(
                alice_session.state.their_next_nostr_public_key,
                Some(bob_current_pubkey)
            );
            assert!(next_invite.used_by.contains(&bob_identity_pk));
        }
        other => panic!("expected accepted invite response, got {other:?}"),
    }
}

#[test]
fn device_to_device_handshake_allows_bob_to_send_and_alice_to_decrypt() {
    let (alice_identity_sk, alice_identity_pk) = keypair(110);
    let (_, bob_identity_pk) = keypair(111);

    let invite = Invite::create(InviteCreateInput {
        inviter: alice_identity_pk,
        inviter_ephemeral_private_key: key_bytes(112),
        shared_secret: key_bytes(113),
        created_at: 1_700_000_000,
        device_id: Some("alice-device".to_string()),
        max_uses: None,
        purpose: Some("chat".to_string()),
        owner_public_key: Some(alice_identity_pk),
    })
    .unwrap();

    let accepted = invite
        .accept(InviteAcceptInput {
            invitee_public_key: bob_identity_pk,
            invitee_identity_private_key: key_bytes(111),
            invitee_session_private_key: key_bytes(114),
            invitee_next_nostr_private_key: key_bytes(115),
            envelope_sender_private_key: key_bytes(116),
            response_created_at: 1_700_000_010,
            device_id: Some("bob-device".to_string()),
            owner_public_key: Some(bob_identity_pk),
        })
        .unwrap();

    let bob_session = accepted.session.clone();
    let bob_sender_pubkey = bob_session.state.our_current_nostr_key.as_ref().unwrap().public_key;

    let alice_session = match invite.process_response(InviteProcessResponseInput {
        event: accepted.response_event,
        inviter_identity_private_key: alice_identity_sk.to_secret_bytes(),
        inviter_next_nostr_private_key: key_bytes(117),
    }) {
        InviteProcessResponseResult::Accepted { session, .. } => session,
        other => panic!("expected accepted invite response, got {other:?}"),
    };

    assert!(!alice_session.can_send());

    let send = bob_session
        .send_event(SessionSendInput {
            inner_event: inner_event(bob_identity_pk, 14, "hello from bob", 1_700_000_020),
            now_secs: 1_700_000_020,
            now_ms: 1_700_000_020_123,
        })
        .unwrap();

    assert_eq!(u32::from(send.outer_event.kind.as_u16()), MESSAGE_EVENT_KIND);
    assert_ne!(send.next, bob_session);

    let received = alice_session.receive_event(SessionReceiveInput {
        outer_event: send.outer_event.clone(),
        replacement_next_nostr_private_key: key_bytes(118),
    });

    match received {
        SessionReceiveResult::Decrypted {
            next,
            inner_event: Some(inner_event),
            meta,
            plaintext,
        } => {
            assert_eq!(inner_event.content, "hello from bob");
            assert_eq!(inner_event.pubkey, bob_identity_pk);
            assert!(plaintext.contains("\"content\":\"hello from bob\""));
            assert_eq!(meta.sender, bob_sender_pubkey);
            assert_eq!(meta.outer_event_id, send.outer_event.id);
            assert_ne!(next, alice_session);
            assert!(next.can_send());
        }
        other => panic!("expected decrypted session message, got {other:?}"),
    }
}
