#![cfg(any())]

use std::collections::HashMap;

use crossbeam_channel::unbounded;
use nostr::Keys;
use nostr_double_ratchet::{SerializableKeyPair, SessionManager, SessionState, StoredUserRecord};

fn make_state(
    their_current: Option<nostr::PublicKey>,
    their_next: Option<nostr::PublicKey>,
    can_receive: bool,
) -> SessionState {
    let our_next = Keys::generate();

    SessionState {
        root_key: [1u8; 32],
        their_current_nostr_public_key: their_current,
        their_next_nostr_public_key: their_next,
        our_current_nostr_key: None,
        our_next_nostr_key: SerializableKeyPair {
            public_key: our_next.public_key(),
            private_key: our_next.secret_key().secret_bytes(),
        },
        receiving_chain_key: can_receive.then_some([2u8; 32]),
        sending_chain_key: Some([3u8; 32]),
        sending_chain_message_number: 0,
        receiving_chain_message_number: if can_receive { 1 } else { 0 },
        previous_sending_chain_message_count: 0,
        skipped_keys: HashMap::new(),
    }
}

fn sorted_hex_pubkeys(pubkeys: &[nostr::PublicKey]) -> Vec<String> {
    let mut hexes = pubkeys
        .iter()
        .map(|pubkey| pubkey.to_hex())
        .collect::<Vec<_>>();
    hexes.sort();
    hexes
}

#[test]
fn session_manager_exposes_storage_independent_inspection_helpers() {
    let owner = Keys::generate();
    let alice = Keys::generate();
    let bob = Keys::generate();
    let sender_a = Keys::generate();
    let sender_b = Keys::generate();
    let sender_c = Keys::generate();
    let (event_tx, _event_rx) = unbounded();

    let manager = SessionManager::new(
        owner.public_key(),
        owner.secret_key().secret_bytes(),
        owner.public_key().to_hex(),
        owner.public_key(),
        event_tx,
        None,
        None,
    );

    manager
        .import_session_state(
            alice.public_key(),
            Some("alice-phone".to_string()),
            make_state(
                Some(sender_a.public_key()),
                Some(sender_b.public_key()),
                true,
            ),
        )
        .expect("import alice phone");
    manager
        .import_session_state(
            alice.public_key(),
            Some("alice-tablet".to_string()),
            make_state(
                Some(sender_b.public_key()),
                Some(sender_c.public_key()),
                false,
            ),
        )
        .expect("import alice tablet");
    manager
        .import_session_state(
            bob.public_key(),
            Some("bob-phone".to_string()),
            make_state(Some(sender_c.public_key()), None, false),
        )
        .expect("import bob phone");

    let known_peers = manager
        .known_peer_owner_pubkeys()
        .into_iter()
        .map(|pubkey| pubkey.to_hex())
        .collect::<Vec<_>>();
    let mut expected_peers = vec![alice.public_key().to_hex(), bob.public_key().to_hex()];
    expected_peers.sort();
    assert_eq!(known_peers, expected_peers);

    let stored_json = manager
        .get_stored_user_record_json(alice.public_key())
        .expect("read stored alice record")
        .expect("alice record present");
    let stored: StoredUserRecord = serde_json::from_str(&stored_json).expect("parse alice record");
    assert_eq!(stored.user_id, alice.public_key().to_hex());
    assert_eq!(stored.devices.len(), 2);

    let authors = manager
        .get_message_push_author_pubkeys(alice.public_key())
        .into_iter()
        .map(|pubkey| pubkey.to_hex())
        .collect::<Vec<_>>();
    let mut expected_authors = vec![
        sender_a.public_key().to_hex(),
        sender_b.public_key().to_hex(),
        sender_c.public_key().to_hex(),
    ];
    expected_authors.sort();
    assert_eq!(authors, expected_authors);

    let snapshots = manager.get_message_push_session_states(alice.public_key());
    assert_eq!(snapshots.len(), 2);
    assert!(snapshots
        .iter()
        .any(|snapshot| snapshot.has_receiving_capability));
    assert!(snapshots.iter().any(|snapshot| {
        sorted_hex_pubkeys(&snapshot.tracked_sender_pubkeys)
            == sorted_hex_pubkeys(&[sender_a.public_key(), sender_b.public_key()])
    }));
    assert!(snapshots.iter().any(|snapshot| {
        sorted_hex_pubkeys(&snapshot.tracked_sender_pubkeys)
            == sorted_hex_pubkeys(&[sender_b.public_key(), sender_c.public_key()])
    }));
}
