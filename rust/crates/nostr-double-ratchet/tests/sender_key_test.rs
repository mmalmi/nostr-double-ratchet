use nostr_double_ratchet::sender_key::SenderKeyState;

#[test]
fn sender_key_roundtrip_single_message() {
    let chain_key = [7u8; 32];

    let mut sender = SenderKeyState::new(1, chain_key, 0);
    let mut receiver = SenderKeyState::new(1, chain_key, 0);

    let (iteration, ciphertext) = sender.encrypt("hello").unwrap();
    assert_eq!(iteration, 0);

    let plaintext = receiver.decrypt(iteration, &ciphertext).unwrap();
    assert_eq!(plaintext, "hello");

    // Both sides should advance in lockstep after decrypting the same message.
    assert_eq!(sender.iteration(), receiver.iteration());
    assert_eq!(sender.chain_key(), receiver.chain_key());
}

#[test]
fn sender_key_decrypt_out_of_order() {
    let chain_key = [9u8; 32];

    let mut sender = SenderKeyState::new(1, chain_key, 0);
    let mut receiver = SenderKeyState::new(1, chain_key, 0);

    let (n0, c0) = sender.encrypt("m0").unwrap();
    let (n1, c1) = sender.encrypt("m1").unwrap();
    assert_eq!(n0, 0);
    assert_eq!(n1, 1);

    // Deliver second message first.
    assert_eq!(receiver.decrypt(n1, &c1).unwrap(), "m1");
    // Then deliver the first message; it should still decrypt via stored skipped keys.
    assert_eq!(receiver.decrypt(n0, &c0).unwrap(), "m0");
}

#[test]
fn sender_key_rejects_too_many_skipped_messages() {
    let chain_key = [3u8; 32];
    let mut receiver = SenderKeyState::new(1, chain_key, 0);

    // If the message number is far ahead, we should fail fast.
    let err = receiver.decrypt(100_000, "AA").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Too many skipped messages"),
        "expected TooManySkippedMessages, got: {msg}"
    );
}
