use nostr_double_ratchet::{
    group::{GROUP_SENDER_KEY_DISTRIBUTION_KIND, GROUP_SENDER_KEY_MESSAGE_KIND},
    sender_key::{SenderKeyDistribution, SenderKeyState},
    Invite, SharedChannel, CHAT_MESSAGE_KIND,
};

#[test]
fn session_sender_key_distribution_then_shared_channel_message_roundtrip() {
    // Use a valid secp256k1 secret key for the shared channel.
    let channel_keys = nostr::Keys::generate();
    let secret_bytes = channel_keys.secret_key().to_secret_bytes();
    let channel = SharedChannel::new(&secret_bytes).unwrap();

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
    let key_id = 123u32;
    let chain_key = [7u8; 32];
    let dist = SenderKeyDistribution::new(group_id.clone(), key_id, chain_key, 0);
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

    let (n, ciphertext) = sender_state.encrypt(&inner_plaintext_json).unwrap();

    let env_inner_unsigned = nostr::EventBuilder::new(
        nostr::Kind::Custom(GROUP_SENDER_KEY_MESSAGE_KIND as u16),
        &ciphertext,
    )
    .tag(nostr::Tag::parse(&["l".to_string(), group_id.clone()]).unwrap())
    .tag(nostr::Tag::parse(&["key".to_string(), key_id.to_string()]).unwrap())
    .tag(nostr::Tag::parse(&["n".to_string(), n.to_string()]).unwrap())
    .custom_created_at(nostr::Timestamp::from(now))
    .build(identity_pk);

    let env_inner_signed = env_inner_unsigned.sign_with_keys(&identity_keys).unwrap();
    assert!(env_inner_signed.verify().is_ok());

    let env_outer = channel
        .create_event(&serde_json::to_string(&env_inner_signed).unwrap())
        .unwrap();
    let env_decrypted = channel.decrypt_event(&env_outer).unwrap();
    let parsed_env_inner: nostr::Event =
        nostr::JsonUtil::from_json(env_decrypted.as_str()).unwrap();
    assert!(parsed_env_inner.verify().is_ok());

    let parsed_n = parsed_env_inner
        .tags
        .iter()
        .find_map(|t| {
            let v = t.clone().to_vec();
            if v.first().map(|s| s.as_str()) == Some("n") {
                v.get(1)?.parse::<u32>().ok()
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(parsed_n, n);

    let decrypted_inner_json = receiver_state
        .decrypt(parsed_n, &parsed_env_inner.content)
        .unwrap();
    let decrypted_inner: serde_json::Value = serde_json::from_str(&decrypted_inner_json).unwrap();
    assert_eq!(decrypted_inner["content"].as_str(), Some("hello"));
}
