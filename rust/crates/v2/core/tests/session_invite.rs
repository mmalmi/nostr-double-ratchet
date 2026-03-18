use ndr_v2_core::{
    InviteAcceptInput, InviteCreateInput, InviteProcessResponseInput, InviteProcessResponseResult,
    InviteState, SessionInitInput, SessionReceiveInput, SessionReceiveResult, SessionSendInput,
    SessionState,
};
use nostr::{EventBuilder, Keys, PublicKey, SecretKey, Timestamp, UnsignedEvent};

fn keypair(byte: u8) -> (SecretKey, PublicKey) {
    let bytes = [byte; 32];
    let sk = SecretKey::from_slice(&bytes).unwrap();
    let pk = Keys::new(sk.clone()).public_key();
    (sk, pk)
}

fn key_bytes(byte: u8) -> [u8; 32] {
    [byte; 32]
}

fn inner_event(kind: u16, content: &str, created_at: u64) -> UnsignedEvent {
    let mut event = EventBuilder::new(nostr::Kind::Custom(kind), content)
        .custom_created_at(Timestamp::from(created_at))
        .build(Keys::generate().public_key());
    event.ensure_id();
    event
}

#[test]
fn session_send_returns_next_state_and_outputs() {
    let (_, their_ephemeral_pubkey) = keypair(1);
    let (our_ephemeral_sk, _) = keypair(2);
    let init = SessionInitInput {
        session_id: None,
        their_ephemeral_nostr_public_key: their_ephemeral_pubkey,
        our_ephemeral_nostr_private_key: our_ephemeral_sk.to_secret_bytes(),
        our_next_nostr_private_key: [3u8; 32],
        is_initiator: true,
        shared_secret: [4u8; 32],
    };
    let state = SessionState::init(init).unwrap();
    let input = SessionSendInput {
        inner_event: inner_event(14, "hello", 1),
        now_secs: 10,
        now_ms: 10_000,
    };

    let output = state.send_event(input).unwrap();

    assert_ne!(output.next.sending_chain_message_number, state.sending_chain_message_number);
    assert_eq!(u32::from(output.outer_event.kind.as_u16()), 1060);
    assert!(output.inner_event.id.is_some());
}

#[test]
fn session_receive_relevant_event_returns_decrypted() {
    let (_, alice_identity_pk) = keypair(10);
    let (alice_ephemeral_sk, alice_ephemeral_pk) = keypair(11);
    let (bob_ephemeral_sk, bob_ephemeral_pk) = keypair(12);

    let alice = SessionState::init(SessionInitInput {
        session_id: None,
        their_ephemeral_nostr_public_key: bob_ephemeral_pk,
        our_ephemeral_nostr_private_key: alice_ephemeral_sk.to_secret_bytes(),
        our_next_nostr_private_key: [13u8; 32],
        is_initiator: true,
        shared_secret: [14u8; 32],
    })
    .unwrap();

    let bob = SessionState::init(SessionInitInput {
        session_id: None,
        their_ephemeral_nostr_public_key: alice_ephemeral_pk,
        our_ephemeral_nostr_private_key: bob_ephemeral_sk.to_secret_bytes(),
        our_next_nostr_private_key: [15u8; 32],
        is_initiator: false,
        shared_secret: [14u8; 32],
    })
    .unwrap();

    let send = alice
        .send_event(SessionSendInput {
            inner_event: inner_event(14, &alice_identity_pk.to_hex(), 2),
            now_secs: 20,
            now_ms: 20_000,
        })
        .unwrap();

    let received = bob.receive_event(SessionReceiveInput {
        outer_event: send.outer_event,
        replacement_next_nostr_private_key: [16u8; 32],
    });

    match received {
        SessionReceiveResult::Decrypted { inner_event, meta, .. } => {
            assert_eq!(meta.sender, alice_ephemeral_pk);
            assert_eq!(inner_event.content, alice_identity_pk.to_hex());
        }
        other => panic!("expected decrypted result, got {other:?}"),
    }
}

#[test]
fn session_receive_irrelevant_event_returns_not_for_this_session() {
    let (_, their_ephemeral_pubkey) = keypair(20);
    let (our_ephemeral_sk, _) = keypair(21);
    let state = SessionState::init(SessionInitInput {
        session_id: None,
        their_ephemeral_nostr_public_key: their_ephemeral_pubkey,
        our_ephemeral_nostr_private_key: our_ephemeral_sk.to_secret_bytes(),
        our_next_nostr_private_key: [22u8; 32],
        is_initiator: false,
        shared_secret: [23u8; 32],
    })
    .unwrap();

    let unrelated = EventBuilder::new(nostr::Kind::Custom(1060), "ciphertext")
        .custom_created_at(Timestamp::from(30))
        .build(Keys::generate().public_key())
        .sign_with_keys(&Keys::generate())
        .unwrap();

    let result = state.receive_event(SessionReceiveInput {
        outer_event: unrelated,
        replacement_next_nostr_private_key: [24u8; 32],
    });

    assert!(matches!(result, SessionReceiveResult::NotForThisSession { .. }));
}

#[test]
fn session_receive_malformed_but_relevant_returns_invalid_relevant() {
    let (alice_ephemeral_sk, alice_ephemeral_pk) = keypair(30);
    let (bob_ephemeral_sk, bob_ephemeral_pk) = keypair(31);

    let alice = SessionState::init(SessionInitInput {
        session_id: None,
        their_ephemeral_nostr_public_key: bob_ephemeral_pk,
        our_ephemeral_nostr_private_key: alice_ephemeral_sk.to_secret_bytes(),
        our_next_nostr_private_key: [32u8; 32],
        is_initiator: true,
        shared_secret: [33u8; 32],
    })
    .unwrap();

    let bob = SessionState::init(SessionInitInput {
        session_id: None,
        their_ephemeral_nostr_public_key: alice_ephemeral_pk,
        our_ephemeral_nostr_private_key: bob_ephemeral_sk.to_secret_bytes(),
        our_next_nostr_private_key: [34u8; 32],
        is_initiator: false,
        shared_secret: [33u8; 32],
    })
    .unwrap();

    let mut send = alice
        .send_event(SessionSendInput {
            inner_event: inner_event(14, "hello", 3),
            now_secs: 40,
            now_ms: 40_000,
        })
        .unwrap()
        .outer_event;
    send.content = "tampered".to_string();

    let received = bob.receive_event(SessionReceiveInput {
        outer_event: send,
        replacement_next_nostr_private_key: [35u8; 32],
    });

    assert!(matches!(
        received,
        SessionReceiveResult::InvalidRelevant { .. }
    ));
}

#[test]
fn invite_accept_returns_session_and_response() {
    let (_, inviter_pk) = keypair(40);
    let invite = InviteState::create(InviteCreateInput {
        invite_id: None,
        inviter: inviter_pk,
        inviter_ephemeral_private_key: [41u8; 32],
        shared_secret: [42u8; 32],
        created_at: 100,
        device_id: Some("inviter-device".to_string()),
        max_uses: None,
        purpose: None,
        owner_public_key: None,
    })
    .unwrap();
    let (_, invitee_pk) = keypair(44);

    let accepted = invite
        .accept(InviteAcceptInput {
            invitee_public_key: invitee_pk,
            invitee_identity_private_key: key_bytes(44),
            invitee_session_private_key: key_bytes(45),
            invitee_next_nostr_private_key: key_bytes(46),
            envelope_sender_private_key: key_bytes(47),
            response_created_at: 101,
            device_id: Some("invitee-device".to_string()),
            owner_public_key: Some(invitee_pk),
            session_id: None,
        })
        .unwrap();

    assert_eq!(u32::from(accepted.response_event.kind.as_u16()), 1059);
    assert!(accepted.session.can_send());
}

#[test]
fn invite_process_matching_response_returns_accepted() {
    let (inviter_sk, inviter_pk) = keypair(50);
    let invite = InviteState::create(InviteCreateInput {
        invite_id: None,
        inviter: inviter_pk,
        inviter_ephemeral_private_key: key_bytes(51),
        shared_secret: key_bytes(52),
        created_at: 200,
        device_id: Some("inviter".to_string()),
        max_uses: None,
        purpose: None,
        owner_public_key: None,
    })
    .unwrap();
    let (_, invitee_pk) = keypair(54);
    let accepted = invite
        .accept(InviteAcceptInput {
            invitee_public_key: invitee_pk,
            invitee_identity_private_key: key_bytes(54),
            invitee_session_private_key: key_bytes(55),
            invitee_next_nostr_private_key: key_bytes(56),
            envelope_sender_private_key: key_bytes(57),
            response_created_at: 201,
            device_id: Some("invitee".to_string()),
            owner_public_key: Some(invitee_pk),
            session_id: None,
        })
        .unwrap();

    let processed = invite.process_response(InviteProcessResponseInput {
        event: accepted.response_event,
        inviter_identity_private_key: inviter_sk.to_secret_bytes(),
        inviter_next_nostr_private_key: key_bytes(59),
        session_id: None,
    });

    match processed {
        InviteProcessResponseResult::Accepted { meta, .. } => {
            assert_eq!(meta.invitee_identity, invitee_pk);
            assert_eq!(meta.owner_public_key, Some(invitee_pk));
        }
        other => panic!("expected accepted invite response, got {other:?}"),
    }
}

#[test]
fn invite_process_nonmatching_response_returns_not_for_this_invite() {
    let (_, inviter_a) = keypair(60);
    let invite_a = InviteState::create(InviteCreateInput {
        invite_id: None,
        inviter: inviter_a,
        inviter_ephemeral_private_key: key_bytes(61),
        shared_secret: key_bytes(62),
        created_at: 300,
        device_id: Some("a".to_string()),
        max_uses: None,
        purpose: None,
        owner_public_key: None,
    })
    .unwrap();
    let (_, inviter_b) = keypair(63);
    let invite_b = InviteState::create(InviteCreateInput {
        invite_id: None,
        inviter: inviter_b,
        inviter_ephemeral_private_key: key_bytes(64),
        shared_secret: key_bytes(65),
        created_at: 301,
        device_id: Some("b".to_string()),
        max_uses: None,
        purpose: None,
        owner_public_key: None,
    })
    .unwrap();
    let (_, invitee_pk) = keypair(67);
    let accepted = invite_b
        .accept(InviteAcceptInput {
            invitee_public_key: invitee_pk,
            invitee_identity_private_key: key_bytes(67),
            invitee_session_private_key: key_bytes(68),
            invitee_next_nostr_private_key: key_bytes(69),
            envelope_sender_private_key: key_bytes(70),
            response_created_at: 302,
            device_id: None,
            owner_public_key: None,
            session_id: None,
        })
        .unwrap();

    let processed = invite_a.process_response(InviteProcessResponseInput {
        event: accepted.response_event,
        inviter_identity_private_key: key_bytes(60),
        inviter_next_nostr_private_key: key_bytes(72),
        session_id: None,
    });

    assert!(matches!(
        processed,
        InviteProcessResponseResult::NotForThisInvite { .. }
    ));
}

#[test]
fn invite_process_invalid_relevant_response_returns_invalid_relevant() {
    let (inviter_sk, inviter_pk) = keypair(73);
    let invite = InviteState::create(InviteCreateInput {
        invite_id: None,
        inviter: inviter_pk,
        inviter_ephemeral_private_key: key_bytes(74),
        shared_secret: key_bytes(75),
        created_at: 400,
        device_id: Some("inviter".to_string()),
        max_uses: None,
        purpose: None,
        owner_public_key: None,
    })
    .unwrap();
    let (_, invitee_pk) = keypair(77);
    let mut accepted = invite
        .accept(InviteAcceptInput {
            invitee_public_key: invitee_pk,
            invitee_identity_private_key: key_bytes(77),
            invitee_session_private_key: key_bytes(78),
            invitee_next_nostr_private_key: key_bytes(79),
            envelope_sender_private_key: key_bytes(80),
            response_created_at: 401,
            device_id: None,
            owner_public_key: None,
            session_id: None,
        })
        .unwrap()
        .response_event;
    accepted.content = "tampered".to_string();

    let processed = invite.process_response(InviteProcessResponseInput {
        event: accepted,
        inviter_identity_private_key: inviter_sk.to_secret_bytes(),
        inviter_next_nostr_private_key: key_bytes(82),
        session_id: None,
    });

    assert!(matches!(
        processed,
        InviteProcessResponseResult::InvalidRelevant { .. }
    ));
}
