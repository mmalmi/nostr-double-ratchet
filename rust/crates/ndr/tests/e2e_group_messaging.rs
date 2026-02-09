#![allow(dead_code)]

use nostr_double_ratchet::{Invite, Session};
use std::path::Path;

mod helpers {
    pub struct TestInstance {
        pub _temp: tempfile::TempDir,
        pub data_dir: std::path::PathBuf,
        pub pubkey: String,
        pub private_key: String,
    }

    pub fn create_instance() -> TestInstance {
        let temp = tempfile::TempDir::new().unwrap();
        let data_dir = temp.path().to_path_buf();
        std::fs::create_dir_all(&data_dir).unwrap();

        let keys = nostr::Keys::generate();
        let sk_hex = keys.secret_key().to_secret_hex();
        let pubkey = keys.public_key().to_hex();

        // Write config
        let config = serde_json::json!({
            "private_key": sk_hex,
            "relays": []
        });
        std::fs::write(data_dir.join("config.json"), config.to_string()).unwrap();

        TestInstance {
            _temp: temp,
            data_dir,
            pubkey,
            private_key: sk_hex,
        }
    }
}

/// Minimal storage for test assertions
struct TestStorage {
    groups_dir: std::path::PathBuf,
    group_messages_dir: std::path::PathBuf,
    chats_dir: std::path::PathBuf,
}

impl TestStorage {
    fn open(data_dir: &Path) -> Self {
        let groups_dir = data_dir.join("groups");
        let group_messages_dir = data_dir.join("group_messages");
        let chats_dir = data_dir.join("chats");
        std::fs::create_dir_all(&groups_dir).unwrap();
        std::fs::create_dir_all(&group_messages_dir).unwrap();
        std::fs::create_dir_all(&chats_dir).unwrap();
        TestStorage {
            groups_dir,
            group_messages_dir,
            chats_dir,
        }
    }

    fn save_group(&self, group: &nostr_double_ratchet::group::GroupData) {
        let path = self.groups_dir.join(format!("{}.json", group.id));
        let content = serde_json::to_string_pretty(group).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn get_group(&self, id: &str) -> Option<nostr_double_ratchet::group::GroupData> {
        let path = self.groups_dir.join(format!("{}.json", id));
        if !path.exists() {
            return None;
        }
        let content = std::fs::read_to_string(path).unwrap();
        Some(serde_json::from_str(&content).unwrap())
    }

    fn save_chat(&self, id: &str, their_pubkey: &str, session_state: &str) {
        let chat = serde_json::json!({
            "id": id,
            "their_pubkey": their_pubkey,
            "created_at": 1700000000u64,
            "last_message_at": null,
            "session_state": session_state,
        });
        let path = self.chats_dir.join(format!("{}.json", id));
        std::fs::write(path, chat.to_string()).unwrap();
    }

    fn list_group_messages(&self, group_id: &str) -> Vec<serde_json::Value> {
        let dir = self.group_messages_dir.join(group_id);
        if !dir.exists() {
            return Vec::new();
        }
        let mut all = Vec::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            if entry
                .path()
                .extension()
                .map(|e| e == "json")
                .unwrap_or(false)
            {
                let content = std::fs::read_to_string(entry.path()).unwrap();
                let msgs: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
                all.extend(msgs);
            }
        }
        all
    }
}

/// Create a 1:1 session pair (Alice inviter, Bob acceptor)
fn create_session_pair(
    alice_pk: &nostr::PublicKey,
    alice_sk: &[u8; 32],
    bob_pk: &nostr::PublicKey,
    bob_sk: &[u8; 32],
) -> (Session, Session, nostr::Event) {
    let invite = Invite::create_new(*alice_pk, None, None).unwrap();
    let (bob_session, response_event) = invite.accept(*bob_pk, *bob_sk, None).unwrap();

    let alice_session = invite
        .process_invite_response(&response_event, *alice_sk)
        .unwrap()
        .unwrap()
        .session;

    (alice_session, bob_session, response_event)
}

#[test]
fn test_group_message_fan_out_and_receive() {
    use nostr_double_ratchet::group::*;
    use nostr_double_ratchet::{CHAT_MESSAGE_KIND, GROUP_METADATA_KIND};

    let alice = helpers::create_instance();
    let bob = helpers::create_instance();

    let alice_storage = TestStorage::open(&alice.data_dir);
    let bob_storage = TestStorage::open(&bob.data_dir);

    // Setup 1:1 sessions
    let alice_pk = nostr::PublicKey::from_hex(&alice.pubkey).unwrap();
    let bob_pk = nostr::PublicKey::from_hex(&bob.pubkey).unwrap();
    let alice_sk_bytes = hex::decode(&alice.private_key).unwrap();
    let mut alice_sk = [0u8; 32];
    alice_sk.copy_from_slice(&alice_sk_bytes);
    let bob_sk_bytes = hex::decode(&bob.private_key).unwrap();
    let mut bob_sk = [0u8; 32];
    bob_sk.copy_from_slice(&bob_sk_bytes);

    let (mut alice_session, mut bob_session, _) =
        create_session_pair(&alice_pk, &alice_sk, &bob_pk, &bob_sk);

    // Bob (acceptor) must send first to complete the ratchet, then Alice can send.
    // In real usage, the 1:1 session is already established before group creation.
    let kickoff = bob_session.send("hello".to_string()).unwrap();
    alice_session.receive(&kickoff).unwrap();

    // Save sessions
    let alice_chat_id = "alice-bob-chat";
    let bob_chat_id = "bob-alice-chat";
    alice_storage.save_chat(
        alice_chat_id,
        &bob.pubkey,
        &serde_json::to_string(&alice_session.state).unwrap(),
    );
    bob_storage.save_chat(
        bob_chat_id,
        &alice.pubkey,
        &serde_json::to_string(&bob_session.state).unwrap(),
    );

    // Alice creates group
    let group = create_group_data("Test Group", &alice.pubkey, &[&bob.pubkey]);
    alice_storage.save_group(&group);

    // Alice sends group metadata to Bob (simulating fan-out)
    let metadata_content = build_group_metadata_content(&group, false);
    let metadata_event = nostr::EventBuilder::new(
        nostr::Kind::Custom(GROUP_METADATA_KIND as u16),
        &metadata_content,
    )
    .tag(nostr::Tag::parse(&["l".to_string(), group.id.clone()]).unwrap())
    .tag(nostr::Tag::parse(&["ms".to_string(), "1700000000000".to_string()]).unwrap())
    .build(alice_pk);

    let encrypted_metadata = alice_session.send_event(metadata_event).unwrap();

    // Bob receives and decrypts
    let decrypted_json = bob_session.receive(&encrypted_metadata).unwrap().unwrap();
    let decrypted: serde_json::Value = serde_json::from_str(&decrypted_json).unwrap();

    // Verify group tag is present
    let tags = decrypted["tags"].as_array().unwrap();
    let l_tag = tags
        .iter()
        .find(|t| t.as_array().unwrap().first().unwrap().as_str().unwrap() == "l");
    assert!(l_tag.is_some());
    let group_id_from_tag = l_tag.unwrap().as_array().unwrap()[1].as_str().unwrap();
    assert_eq!(group_id_from_tag, group.id);

    // Verify metadata can be parsed
    let content = decrypted["content"].as_str().unwrap();
    let metadata = parse_group_metadata(content).unwrap();
    assert_eq!(metadata.id, group.id);
    assert_eq!(metadata.name, "Test Group");
    assert_eq!(metadata.secret, group.secret);

    // Bob validates and creates group locally
    assert!(validate_metadata_creation(
        &metadata,
        &alice.pubkey,
        &bob.pubkey
    ));

    // Alice sends a group message
    let msg_event = nostr::EventBuilder::new(
        nostr::Kind::Custom(CHAT_MESSAGE_KIND as u16),
        "Hello group!",
    )
    .tag(nostr::Tag::parse(&["l".to_string(), group.id.clone()]).unwrap())
    .tag(nostr::Tag::parse(&["ms".to_string(), "1700000001000".to_string()]).unwrap())
    .build(alice_pk);

    let encrypted_msg = alice_session.send_event(msg_event).unwrap();

    // Bob receives
    let decrypted_msg_json = bob_session.receive(&encrypted_msg).unwrap().unwrap();
    let decrypted_msg: serde_json::Value = serde_json::from_str(&decrypted_msg_json).unwrap();

    // Verify it's a group message with the right content
    assert_eq!(decrypted_msg["content"].as_str().unwrap(), "Hello group!");
    let msg_tags = decrypted_msg["tags"].as_array().unwrap();
    let msg_group_tag = msg_tags
        .iter()
        .find(|t| t.as_array().unwrap().first().unwrap().as_str().unwrap() == "l");
    assert!(msg_group_tag.is_some());
    assert_eq!(
        msg_group_tag.unwrap().as_array().unwrap()[1]
            .as_str()
            .unwrap(),
        group.id
    );
}

#[test]
fn test_group_metadata_update_flow() {
    use nostr_double_ratchet::group::*;

    let alice = helpers::create_instance();
    let bob = helpers::create_instance();

    // Alice creates group
    let group = create_group_data("Original Name", &alice.pubkey, &[&bob.pubkey]);

    // Alice updates group name
    let updated = update_group_data(
        &group,
        &GroupUpdate {
            name: Some("New Name".to_string()),
            description: Some("A description".to_string()),
            picture: None,
        },
        &alice.pubkey,
    )
    .unwrap();

    // Build metadata for fan-out
    let content = build_group_metadata_content(&updated, false);
    let metadata = parse_group_metadata(&content).unwrap();

    // Bob validates update
    let validation = validate_metadata_update(&group, &metadata, &alice.pubkey, &bob.pubkey);
    assert_eq!(validation, MetadataValidation::Accept);

    // Apply update
    let bob_group = apply_metadata_update(&group, &metadata);
    assert_eq!(bob_group.name, "New Name");
    assert_eq!(bob_group.description, Some("A description".to_string()));
}

#[test]
fn test_member_removal_metadata_flow() {
    use nostr_double_ratchet::group::*;

    let alice = helpers::create_instance();
    let bob = helpers::create_instance();
    let carol = helpers::create_instance();

    // Alice creates group with Bob and Carol
    let group = create_group_data(
        "Three Members",
        &alice.pubkey,
        &[&bob.pubkey, &carol.pubkey],
    );
    let original_secret = group.secret.clone();

    // Alice removes Carol
    let updated = remove_group_member(&group, &carol.pubkey, &alice.pubkey).unwrap();
    assert_ne!(
        updated.secret, original_secret,
        "Secret should rotate on member removal"
    );
    assert!(!updated.members.contains(&carol.pubkey));

    // Build metadata for Bob (with secret) and Carol (without secret)
    let bob_content = build_group_metadata_content(&updated, false);
    let carol_content = build_group_metadata_content(&updated, true);

    let bob_metadata = parse_group_metadata(&bob_content).unwrap();
    let carol_metadata = parse_group_metadata(&carol_content).unwrap();

    // Bob gets the new secret
    assert!(bob_metadata.secret.is_some());
    assert_eq!(bob_metadata.secret, updated.secret);

    // Carol does NOT get the new secret
    assert!(carol_metadata.secret.is_none());

    // Carol sees she's been removed
    let carol_validation =
        validate_metadata_update(&group, &carol_metadata, &alice.pubkey, &carol.pubkey);
    assert_eq!(carol_validation, MetadataValidation::Removed);
}
