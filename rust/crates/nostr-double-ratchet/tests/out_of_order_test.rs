use nostr_double_ratchet::{Result, Session};
use nostr::Keys;

#[test]
fn test_out_of_order_message_delivery() -> Result<()> {
    let alice_keys = Keys::generate();
    let bob_keys = Keys::generate();

    let alice_sk = alice_keys.secret_key().to_secret_bytes();
    let bob_sk = bob_keys.secret_key().to_secret_bytes();

    let alice_pk = alice_keys.public_key();
    let bob_pk = bob_keys.public_key();

    let shared_secret = [0u8; 32];

    let mut alice = Session::init(bob_pk, alice_sk, true, shared_secret, Some("alice".to_string()))?;
    let mut bob = Session::init(alice_pk, bob_sk, false, shared_secret, Some("bob".to_string()))?;

    let msg1 = alice.send("Message 1".to_string())?;
    let msg2 = alice.send("Message 2".to_string())?;
    let msg3 = alice.send("Message 3".to_string())?;

    let received3 = bob.receive(&msg3)?;
    assert!(received3.is_some());
    let rumor3: serde_json::Value = serde_json::from_str(&received3.unwrap())?;
    assert_eq!(rumor3["content"].as_str(), Some("Message 3"));

    let received1 = bob.receive(&msg1)?;
    assert!(received1.is_some());
    let rumor1: serde_json::Value = serde_json::from_str(&received1.unwrap())?;
    assert_eq!(rumor1["content"].as_str(), Some("Message 1"));

    let received2 = bob.receive(&msg2)?;
    assert!(received2.is_some());
    let rumor2: serde_json::Value = serde_json::from_str(&received2.unwrap())?;
    assert_eq!(rumor2["content"].as_str(), Some("Message 2"));

    Ok(())
}

#[test]
fn test_consecutive_messages_from_same_sender() -> Result<()> {
    let alice_keys = Keys::generate();
    let bob_keys = Keys::generate();

    let alice_sk = alice_keys.secret_key().to_secret_bytes();
    let bob_sk = bob_keys.secret_key().to_secret_bytes();

    let alice_pk = alice_keys.public_key();
    let bob_pk = bob_keys.public_key();

    let shared_secret = [0u8; 32];

    let mut alice = Session::init(bob_pk, alice_sk, true, shared_secret, Some("alice".to_string()))?;
    let mut bob = Session::init(alice_pk, bob_sk, false, shared_secret, Some("bob".to_string()))?;

    let alice_msg1 = alice.send("Alice 1".to_string())?;
    bob.receive(&alice_msg1)?;

    let bob_msg1 = bob.send("Bob 1".to_string())?;
    let bob_msg2 = bob.send("Bob 2".to_string())?;

    let received1 = alice.receive(&bob_msg1)?;
    let rumor1: serde_json::Value = serde_json::from_str(&received1.unwrap())?;
    assert_eq!(rumor1["content"].as_str(), Some("Bob 1"));

    let received2 = alice.receive(&bob_msg2)?;
    let rumor2: serde_json::Value = serde_json::from_str(&received2.unwrap())?;
    assert_eq!(rumor2["content"].as_str(), Some("Bob 2"));

    Ok(())
}
