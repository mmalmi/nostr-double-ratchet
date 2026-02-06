use nostr_double_ratchet::{
    OneToManyChannel, SenderKeyState, CHAT_MESSAGE_KIND, MESSAGE_EVENT_KIND,
};

#[test]
fn one_to_many_outer_payload_roundtrip() {
    let sender_event_keys = nostr::Keys::generate();
    let identity_keys = nostr::Keys::generate();

    let key_id = 123u32;
    let chain_key = [7u8; 32];
    let mut sender_state = SenderKeyState::new(key_id, chain_key, 0);
    let mut receiver_state = SenderKeyState::new(key_id, chain_key, 0);

    let now = 1_700_000_000u64;

    // Build a realistic inner event (unsigned rumor-like event).
    let inner = nostr::EventBuilder::new(nostr::Kind::Custom(CHAT_MESSAGE_KIND as u16), "hello")
        .custom_created_at(nostr::Timestamp::from(now))
        .build(identity_keys.public_key());
    let inner_json = serde_json::to_string(&inner).unwrap();

    let channel = OneToManyChannel::default();
    let outer = channel
        .encrypt_to_outer_event(
            &sender_event_keys,
            &mut sender_state,
            &inner_json,
            nostr::Timestamp::from(now),
        )
        .unwrap();

    assert_eq!(outer.kind, nostr::Kind::Custom(MESSAGE_EVENT_KIND as u16));
    assert_eq!(outer.pubkey, sender_event_keys.public_key());
    assert!(outer.tags.is_empty());
    assert!(outer.verify().is_ok());

    let parsed = channel.parse_outer_content(&outer.content).unwrap();
    assert_eq!(parsed.key_id, key_id);
    let plaintext = parsed.decrypt(&mut receiver_state).unwrap();
    assert_eq!(plaintext, inner_json);
}
