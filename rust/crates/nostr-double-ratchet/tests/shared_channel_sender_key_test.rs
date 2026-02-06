use base64::Engine;
use nostr_double_ratchet::{
    group::GROUP_SENDER_KEY_DISTRIBUTION_KIND,
    sender_key::{SenderKeyDistribution, SenderKeyState},
    Invite, CHAT_MESSAGE_KIND, MESSAGE_EVENT_KIND,
};

#[test]
fn session_sender_key_distribution_then_sender_event_message_roundtrip() {
    let group_id = "g1".to_string();

    // Set up a 1:1 session pair (Alice inviter, Bob acceptor).
    let alice_keys = nostr::Keys::generate();
    let bob_keys = nostr::Keys::generate();

    let alice_pk = alice_keys.public_key();
    let bob_pk = bob_keys.public_key();

    let invite = Invite::create_new(alice_pk, None, None).unwrap();
    let (mut bob_session, response_event) = invite
        .accept(bob_pk, bob_keys.secret_key().to_secret_bytes(), None)
        .unwrap();

    let mut alice_session = invite
        .process_invite_response(&response_event, alice_keys.secret_key().to_secret_bytes())
        .unwrap()
        .unwrap()
        .session;

    // === Distribution ===
    let sender_event_keys = nostr::Keys::generate();
    let sender_event_pubkey_hex = sender_event_keys.public_key().to_hex();

    let key_id = 123u32;
    let chain_key = [7u8; 32];
    let mut dist = SenderKeyDistribution::new(group_id.clone(), key_id, chain_key, 0);
    dist.sender_event_pubkey = Some(sender_event_pubkey_hex.clone());
    let dist_json = serde_json::to_string(&dist).unwrap();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let dist_inner_unsigned = nostr::EventBuilder::new(
        nostr::Kind::Custom(GROUP_SENDER_KEY_DISTRIBUTION_KIND as u16),
        &dist_json,
    )
    .tag(nostr::Tag::parse(&["l".to_string(), group_id.clone()]).unwrap())
    .tag(nostr::Tag::parse(&["key".to_string(), key_id.to_string()]).unwrap())
    .custom_created_at(nostr::Timestamp::from(now))
    .build(bob_pk);

    // Bob (acceptor) must send first to complete the ratchet, then Alice can send later.
    // Distribution travels over the session (forward secrecy).
    let dist_outer = bob_session.send_event(dist_inner_unsigned).unwrap();
    let dist_decrypted = alice_session.receive(&dist_outer).unwrap().unwrap();
    let parsed_dist_inner: serde_json::Value = serde_json::from_str(&dist_decrypted).unwrap();

    let parsed_dist: SenderKeyDistribution =
        serde_json::from_str(parsed_dist_inner["content"].as_str().unwrap()).unwrap();
    assert_eq!(parsed_dist.group_id, group_id);
    assert_eq!(parsed_dist.key_id, key_id);
    assert_eq!(parsed_dist.chain_key, chain_key);
    assert_eq!(
        parsed_dist.sender_event_pubkey,
        Some(sender_event_pubkey_hex.clone())
    );

    let mut sender_state = SenderKeyState::new(key_id, chain_key, 0);
    let mut receiver_state = SenderKeyState::new(key_id, chain_key, 0);

    // === Message ===
    let identity_keys = bob_keys;
    let identity_pk = identity_keys.public_key();

    let inner_plaintext =
        nostr::EventBuilder::new(nostr::Kind::Custom(CHAT_MESSAGE_KIND as u16), "hello")
            .tag(nostr::Tag::parse(&["l".to_string(), group_id.clone()]).unwrap())
            .custom_created_at(nostr::Timestamp::from(now))
            .build(identity_pk);
    let inner_plaintext_json = serde_json::to_string(&inner_plaintext).unwrap();

    let (n, ciphertext_bytes) = sender_state
        .encrypt_to_bytes(&inner_plaintext_json)
        .unwrap();

    // Outer content format: base64(key_id||n||nip44_ciphertext_bytes)
    let mut payload: Vec<u8> = Vec::with_capacity(8 + ciphertext_bytes.len());
    payload.extend_from_slice(&key_id.to_be_bytes());
    payload.extend_from_slice(&n.to_be_bytes());
    payload.extend_from_slice(&ciphertext_bytes);
    let payload_b64 = base64::engine::general_purpose::STANDARD.encode(payload);

    let outer_unsigned =
        nostr::EventBuilder::new(nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16), &payload_b64)
            .custom_created_at(nostr::Timestamp::from(now))
            .build(sender_event_keys.public_key());
    let outer = outer_unsigned.sign_with_keys(&sender_event_keys).unwrap();
    assert!(outer.verify().is_ok());

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&outer.content)
        .unwrap();
    assert!(bytes.len() >= 8);
    let parsed_key_id = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
    let parsed_n = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
    let parsed_ciphertext = bytes[8..].to_vec();
    assert_eq!(parsed_key_id, key_id);
    assert_eq!(parsed_n, n);

    let decrypted_inner_json = receiver_state
        .decrypt_from_bytes(parsed_n, &parsed_ciphertext)
        .unwrap();
    let decrypted_inner: serde_json::Value = serde_json::from_str(&decrypted_inner_json).unwrap();
    assert_eq!(decrypted_inner["content"].as_str(), Some("hello"));
}
