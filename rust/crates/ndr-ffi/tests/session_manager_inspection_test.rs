use std::collections::HashMap;

use ndr_ffi::SessionManagerHandle;
use nostr::Keys;
use nostr_double_ratchet::{utils::serialize_session_state, SerializableKeyPair, SessionState};
use tempfile::tempdir;

fn make_state(
    their_current: Option<nostr::PublicKey>,
    their_next: Option<nostr::PublicKey>,
    can_receive: bool,
) -> String {
    let our_next = Keys::generate();
    let state = SessionState {
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
    };

    serialize_session_state(&state).expect("serialize session state")
}

fn sorted_strings(values: &[String]) -> Vec<String> {
    let mut sorted = values.to_vec();
    sorted.sort();
    sorted
}

#[test]
fn ffi_exposes_supported_session_manager_inspection_apis() {
    let owner = Keys::generate();
    let alice = Keys::generate();
    let bob = Keys::generate();
    let sender_a = Keys::generate();
    let sender_b = Keys::generate();

    let manager = SessionManagerHandle::new(
        owner.public_key().to_hex(),
        owner.secret_key().to_secret_hex(),
        owner.public_key().to_hex(),
        None,
    )
    .expect("create manager");
    manager.init().expect("init manager");

    manager
        .import_session_state(
            alice.public_key().to_hex(),
            make_state(
                Some(sender_a.public_key()),
                Some(sender_b.public_key()),
                true,
            ),
            Some("alice-phone".to_string()),
        )
        .expect("import alice state");
    manager
        .import_session_state(
            bob.public_key().to_hex(),
            make_state(Some(sender_b.public_key()), None, false),
            Some("bob-phone".to_string()),
        )
        .expect("import bob state");

    let mut expected_peers = vec![alice.public_key().to_hex(), bob.public_key().to_hex()];
    expected_peers.sort();
    assert_eq!(manager.known_peer_owner_pubkeys(), expected_peers);

    let stored = manager
        .get_stored_user_record_json(alice.public_key().to_hex())
        .expect("read stored user record")
        .expect("alice stored record");
    let stored_json: serde_json::Value =
        serde_json::from_str(&stored).expect("parse stored user record");
    assert_eq!(stored_json["user_id"], alice.public_key().to_hex());

    let expected_authors = vec![
        sender_a.public_key().to_hex(),
        sender_b.public_key().to_hex(),
    ];
    assert_eq!(
        sorted_strings(
            &manager
                .get_message_push_author_pubkeys(alice.public_key().to_hex())
                .expect("message push authors"),
        ),
        sorted_strings(&expected_authors)
    );

    let snapshots = manager
        .get_message_push_session_states(alice.public_key().to_hex())
        .expect("message push session states");
    assert_eq!(snapshots.len(), 1);
    assert!(snapshots[0].has_receiving_capability);
    assert_eq!(
        sorted_strings(&snapshots[0].tracked_sender_pubkeys),
        sorted_strings(&expected_authors)
    );
}

#[test]
fn ffi_lists_persisted_peers_before_init() {
    let owner = Keys::generate();
    let alice = Keys::generate();
    let sender = Keys::generate();
    let storage_dir = tempdir().expect("tempdir");
    let storage_path = storage_dir.path().to_string_lossy().to_string();

    let writer = SessionManagerHandle::new_with_storage_path(
        owner.public_key().to_hex(),
        owner.secret_key().to_secret_hex(),
        owner.public_key().to_hex(),
        storage_path.clone(),
        None,
    )
    .expect("create writer");
    writer.init().expect("init writer");
    writer
        .import_session_state(
            alice.public_key().to_hex(),
            make_state(Some(sender.public_key()), None, true),
            Some("alice-phone".to_string()),
        )
        .expect("import alice state");

    let reader = SessionManagerHandle::new_with_storage_path(
        owner.public_key().to_hex(),
        owner.secret_key().to_secret_hex(),
        owner.public_key().to_hex(),
        storage_path,
        None,
    )
    .expect("create reader");

    assert_eq!(
        reader.known_peer_owner_pubkeys(),
        vec![alice.public_key().to_hex()]
    );
    assert!(reader
        .get_stored_user_record_json(alice.public_key().to_hex())
        .expect("read stored user record")
        .is_some());
}
